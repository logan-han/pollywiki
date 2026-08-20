use crate::http::{polite_fetch, FetchOpts};
use crate::sources::tvfy::key_for;
use crate::store::Store;
use anyhow::{anyhow, Result};
use indexmap::IndexMap;
use pollywiki_schema::{Bill, Division, House, Person, Vote};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

const DEFAULT_MODEL: &str = "gemini-3.1-flash-lite";
const BATCH_SIZE: usize = 8;
const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Free-tier headroom: nightly needs a handful; the backfill spans a few runs.
fn request_cap() -> u32 {
    std::env::var("GEMINI_REQUEST_CAP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(80)
}

/// Where the model lives and how hard this run may lean on it. A value rather
/// than a set of constants so the whole summarise flow can be pointed at a
/// local server under test.
#[derive(Clone)]
pub struct Gemini {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// Spacing between calls; the free tier counts requests per minute.
    pub min_interval_ms: u64,
    /// Calls per run, so a backfill spans several nights instead of tripping
    /// the daily quota in one.
    pub request_cap: u32,
}

impl Gemini {
    pub fn from_env() -> Result<Self> {
        Ok(Gemini {
            base_url: DEFAULT_BASE_URL.to_string(),
            api_key: std::env::var("GEMINI_API_KEY")
                .map_err(|_| anyhow!("GEMINI_API_KEY not set; skipping AI summaries"))?,
            model: std::env::var("GEMINI_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string()),
            min_interval_ms: 6500,
            request_cap: request_cap(),
        })
    }

    /// politeFetch paces requests per host and retries 429/5xx with backoff,
    /// which keeps the run inside the free-tier per-minute limits.
    async fn generate(&self, prompt: &str) -> Result<String> {
        let body = serde_json::json!({
            "contents": [{ "parts": [{ "text": prompt }] }],
            "generationConfig": { "responseMimeType": "application/json", "temperature": 0.2 },
        });
        let mut opts = FetchOpts::min_interval(self.min_interval_ms)
            .with_header("x-goog-api-key", &self.api_key);
        opts.post_json = Some(serde_json::to_string(&body)?);
        let res = polite_fetch(
            &format!(
                "{}/models/{}:generateContent",
                self.base_url.trim_end_matches('/'),
                self.model
            ),
            &opts,
        )
        .await?;
        let data: Value = res.json().await?;
        data.pointer("/candidates/0/content/parts/0/text")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("gemini: empty response"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSummary {
    pub text: String,
    pub model: String,
    pub generated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_version: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiPersonNote {
    pub text: String,
    pub model: String,
    pub generated_at: String,
    pub votes_considered: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_version: Option<i64>,
}

/// Bump to regenerate every division summary after a prompt change.
const SUMMARY_PROMPT_VERSION: i64 = 3;

/// TVFY's context field is a written summary for most bill votes but a raw
/// Hansard excerpt for procedural motions. A transcript quotes speakers, so a
/// paragraph consisting solely of a sitting member's name marks it.
pub fn is_transcript(summary: &str, member_names: &HashSet<String>) -> bool {
    summary
        .split('\n')
        .map(str::trim)
        .any(|line| member_names.contains(line))
}

/// Generates the machine-written layers: neutral context for divisions whose
/// only TVFY context is a transcript, and descriptive voting-pattern notes per
/// person. Grounded strictly on the record; stored under derived/ and always
/// rendered with a clear AI label.
pub async fn summarise(store: &Store, people: &[Person]) -> Result<()> {
    summarise_with(store, people, &Gemini::from_env()?).await
}

/// The flow itself, against a given model endpoint.
pub async fn summarise_with(store: &Store, people: &[Person], gemini: &Gemini) -> Result<()> {
    let names: HashSet<String> = people.iter().map(|p| p.name.clone()).collect();

    let mut divisions: Vec<Division> = Vec::new();
    for key in store.list("canonical/divisions/").await? {
        if let Some(division) = store.get_json::<Division>(&key).await? {
            divisions.push(division);
        }
    }
    let mut bills: IndexMap<String, Bill> = IndexMap::new();
    for key in store.list("canonical/bills/").await? {
        if let Some(bill) = store.get_json::<Bill>(&key).await? {
            bills.insert(bill.id.clone(), bill);
        }
    }

    division_summaries(store, gemini, &divisions, &names, &bills).await?;
    let bill_list: Vec<&Bill> = bills.values().collect();
    bill_notes(store, gemini, &bill_list).await?;
    person_notes(store, gemini, people, &divisions).await?;
    Ok(())
}

/// Bump to regenerate every bill note after a prompt change.
const BILL_NOTE_PROMPT_VERSION: i64 = 3;

pub fn bill_note_key(id: &str) -> String {
    format!("derived/ai-bill-notes/{id}.json")
}

/// Plain-English context per bill. Unlike division summaries, general
/// knowledge of Australian parliamentary practice is allowed to explain the
/// MECHANISM (what an appropriation bill is); anything specific to this bill
/// must come from the official summary alone.
async fn bill_notes(store: &Store, gemini: &Gemini, bills: &[&Bill]) -> Result<()> {
    let mut pending: Vec<&Bill> = Vec::new();
    for bill in bills {
        let existing: Option<AiSummary> = store.get_json(&bill_note_key(&bill.id)).await?;
        if existing.map(|e| e.prompt_version) != Some(Some(BILL_NOTE_PROMPT_VERSION)) {
            pending.push(bill);
        }
    }
    if pending.is_empty() {
        println!("summarise: no bill notes pending");
        return Ok(());
    }

    let cap = gemini.request_cap;
    let mut requests = 0;
    let mut written = 0;
    let mut failed = 0;
    for (i, batch) in pending.chunks(BATCH_SIZE).enumerate() {
        requests += 1;
        if requests > cap {
            println!(
                "summarise: bill-note cap {cap} reached, {} left for next run",
                pending.len() - i * BATCH_SIZE
            );
            break;
        }
        match generate_bill_batch(gemini, batch).await {
            Ok(results) => {
                for bill in batch {
                    let Some(text) = results.get(&bill.id) else {
                        continue;
                    };
                    // An empty text records the model's judgement that the official
                    // summary is already plain enough; the bill is not retried.
                    store
                        .put_json(
                            &bill_note_key(&bill.id),
                            &AiSummary {
                                text: text.clone(),
                                model: gemini.model.clone(),
                                generated_at: crate::now_iso(),
                                prompt_version: Some(BILL_NOTE_PROMPT_VERSION),
                            },
                        )
                        .await?;
                    if !text.is_empty() {
                        written += 1;
                    }
                }
            }
            Err(err) => {
                failed += 1;
                eprintln!("summarise: bill batch at {} failed - {err}", i * BATCH_SIZE);
                if failed >= 5 {
                    return Err(anyhow!("summarise: too many failed bill batches, aborting"));
                }
            }
        }
    }
    println!(
        "summarise: wrote {written} bill notes (model {}, {failed} failed batches)",
        gemini.model
    );
    Ok(())
}

async fn generate_bill_batch(gemini: &Gemini, batch: &[&Bill]) -> Result<IndexMap<String, String>> {
    let items = batch
        .iter()
        .map(|b| {
            format!(
                "<bill id=\"{}\">\nTitle: {}\nOfficial summary: {}\n</bill>",
                b.id,
                b.title,
                b.summary.as_deref().unwrap_or("none provided")
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let prompt = format!(
        r#"You explain bills before the Australian federal parliament to ordinary readers.
For EACH bill below, decide whether an explanation adds anything, then write accordingly:
- Return an EMPTY string ONLY when the official summary is short AND every term in it would be understood by an ordinary reader with no knowledge of parliament or law. A note that merely rephrases such a summary is worse than none.
- If the summary is short but contains ANY technical, legal or institutional language — terms like "Consolidated Revenue Fund", "standing appropriations", "delegated legislation", "consequential amendments", or the mechanics of a named act — write ONE sentence that translates the jargon and mechanism into plain terms. Do not restate what is already plain. When unsure, write the sentence.
- If the summary is long or dense, write two to three sentences of plain-English context: what this KIND of bill does in the Australian system where that helps (for example, appropriation bills are the routine budget bills that authorise the government to spend money on its ordinary operations) — general knowledge of parliamentary practice is allowed for the mechanism — and what THIS bill specifically changes, using ONLY the official summary provided; never invent specifics. If the summary is missing, say only what the title supports.
Never evaluate the bill or any politician; no opinions; do not restate the bill's status. Plain Australian English.
Return JSON: an array of {{"id": "<bill id>", "summary": "<text or empty string>"}} covering every bill.

{items}"#
    );

    let text = gemini.generate(&prompt).await?;
    let mut out = IndexMap::new();
    let valid_ids: HashSet<&str> = batch.iter().map(|b| b.id.as_str()).collect();
    let rows: Vec<Value> = serde_json::from_str(&text)?;
    for row in rows {
        let (Some(id), Some(summary)) = (
            row.get("id").and_then(Value::as_str),
            row.get("summary").and_then(Value::as_str),
        ) else {
            continue;
        };
        if valid_ids.contains(id) {
            out.insert(id.to_string(), summary.trim().to_string());
        }
    }
    Ok(out)
}

async fn division_summaries(
    store: &Store,
    gemini: &Gemini,
    divisions: &[Division],
    names: &HashSet<String>,
    bills: &IndexMap<String, Bill>,
) -> Result<()> {
    let mut pending: Vec<&Division> = Vec::new();
    for division in divisions {
        let Some(summary) = &division.summary else {
            continue;
        };
        if !is_transcript(summary, names) {
            continue;
        }
        let existing: Option<AiSummary> = store.get_json(&ai_key(&division.id)).await?;
        if existing.map(|e| e.prompt_version) != Some(Some(SUMMARY_PROMPT_VERSION)) {
            pending.push(division);
        }
    }
    if pending.is_empty() {
        println!("summarise: no divisions pending");
        return Ok(());
    }

    // Same-day division titles give procedural motions their real context:
    // a rearrangement or gag motion is about whatever business surrounded it.
    let mut by_day: IndexMap<String, Vec<&Division>> = IndexMap::new();
    for d in divisions {
        by_day
            .entry(format!("{}/{}", d.house, d.date))
            .or_default()
            .push(d);
    }
    for list in by_day.values_mut() {
        list.sort_by_key(|d| d.number);
    }

    let cap = gemini.request_cap;
    let mut requests = 0;
    let mut written = 0;
    let mut failed_batches = 0;
    for (i, batch) in pending.chunks(BATCH_SIZE).enumerate() {
        requests += 1;
        if requests > cap {
            println!(
                "summarise: request cap {cap} reached, {} left for next run",
                pending.len() - i * BATCH_SIZE
            );
            break;
        }
        // A failed batch is skipped, not fatal: its divisions stay pending and
        // are retried on the next run.
        match generate_batch(gemini, batch, bills, &by_day).await {
            Ok(results) => {
                for division in batch {
                    let Some(text) = results.get(&division.id).filter(|t| !t.is_empty()) else {
                        continue;
                    };
                    store
                        .put_json(
                            &ai_key(&division.id),
                            &AiSummary {
                                text: text.clone(),
                                model: gemini.model.clone(),
                                generated_at: crate::now_iso(),
                                prompt_version: Some(SUMMARY_PROMPT_VERSION),
                            },
                        )
                        .await?;
                    written += 1;
                }
            }
            Err(err) => {
                failed_batches += 1;
                eprintln!("summarise: batch at {} failed - {err}", i * BATCH_SIZE);
                if failed_batches >= 5 {
                    return Err(anyhow!("summarise: too many failed batches, aborting run"));
                }
            }
        }
    }
    println!(
        "summarise: wrote {written} of {} pending (model {}, {failed_batches} failed batches)",
        pending.len(),
        gemini.model
    );
    Ok(())
}

pub fn ai_key(division_id: &str) -> String {
    format!("derived/ai-summaries/{}.json", key_for(division_id))
}

/// Bump to regenerate every note after a prompt change.
const NOTE_PROMPT_VERSION: i64 = 3;

pub fn note_key(slug: &str) -> String {
    format!("derived/ai-person-notes/{slug}.json")
}

const MIN_VOTES_FOR_NOTE: usize = 10;

struct PersonVoteRow<'a> {
    division: &'a Division,
    vote: Vote,
    crossed: bool,
}

async fn person_notes(
    store: &Store,
    gemini: &Gemini,
    people: &[Person],
    divisions: &[Division],
) -> Result<()> {
    let mut votes_by_slug: IndexMap<&str, Vec<PersonVoteRow>> = IndexMap::new();
    for division in divisions {
        for vote in &division.votes {
            votes_by_slug
                .entry(vote.person_slug.as_str())
                .or_default()
                .push(PersonVoteRow {
                    division,
                    vote: vote.vote,
                    crossed: vote.against_group_majority == Some(true),
                });
        }
    }

    let chamber_totals = (
        divisions
            .iter()
            .filter(|d| d.house == House::Representatives)
            .count(),
        divisions
            .iter()
            .filter(|d| d.house == House::Senate)
            .count(),
    );

    let cap = gemini.request_cap;
    let mut requests = 0;
    let mut written = 0;
    let mut failed = 0;
    for person in people {
        let empty = Vec::new();
        let votes = votes_by_slug.get(person.slug.as_str()).unwrap_or(&empty);
        if votes.len() < MIN_VOTES_FOR_NOTE {
            continue;
        }
        let existing: Option<AiPersonNote> = store.get_json(&note_key(&person.slug)).await?;
        // Regenerate when the record has moved or the prompt has changed.
        // A different model alone is not a reason: it would churn every note
        // whenever a quota fallback model runs.
        if let Some(existing) = &existing {
            if existing.votes_considered == votes.len() as i64
                && existing.prompt_version == Some(NOTE_PROMPT_VERSION)
            {
                continue;
            }
        }
        requests += 1;
        if requests > cap {
            println!("summarise: person-note cap {cap} reached, rest next run");
            break;
        }
        let chamber_divisions = match person.house {
            House::Representatives => chamber_totals.0,
            House::Senate => chamber_totals.1,
        };
        match generate_person_note(gemini, person, votes, chamber_divisions).await {
            Ok(text) => {
                store
                    .put_json(
                        &note_key(&person.slug),
                        &AiPersonNote {
                            text,
                            model: gemini.model.clone(),
                            generated_at: crate::now_iso(),
                            votes_considered: votes.len() as i64,
                            prompt_version: Some(NOTE_PROMPT_VERSION),
                        },
                    )
                    .await?;
                written += 1;
            }
            Err(err) => {
                failed += 1;
                eprintln!("summarise: note for {} failed - {err}", person.slug);
                if failed >= 5 {
                    return Err(anyhow!("summarise: too many failed person notes, aborting"));
                }
            }
        }
    }
    println!(
        "summarise: wrote {written} person notes (model {}, {failed} failed)",
        gemini.model
    );
    Ok(())
}

async fn generate_person_note(
    gemini: &Gemini,
    person: &Person,
    votes: &[PersonVoteRow<'_>],
    chamber_divisions: usize,
) -> Result<String> {
    let crossed = votes.iter().filter(|v| v.crossed).count();
    let lines = votes
        .iter()
        .map(|v| {
            format!(
                "{} | {} | voted {}{}",
                v.division.date,
                v.division.name,
                match v.vote {
                    Vote::Aye => "aye",
                    Vote::No => "no",
                },
                if v.crossed { " | crossed" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = format!(
        r#"You describe voting records in the Australian federal parliament, strictly descriptively.

Data for {name} ({group}), {house}:
- Voted in {voted} of {total} recorded divisions; {crossed} vote{plural} against their party grouping's majority ("crossed" below).
- Every recorded vote (date | division | their vote):
{lines}

The page already displays the two counts above, so NEVER restate them, never say they always voted with their party, and never say they did not cross. Write two to three sentences that add what a reader cannot see by scanning a long table:
- the subject areas that recur among the divisions they voted in, and WHICH WAY they voted on each of those subjects (aye or no);
- if they ever crossed, name the specific division(s) and how they voted;
- any other specific, countable pattern in the titles and votes (for example, several related bills all voted the same way).

Describe motions by their formal type (second reading, consideration in detail amendment, suspension of standing orders, censure motion, order for the production of documents) and subject. NEVER characterise a motion by political direction or origin: "critical of the government", "opposition-led" and "government-supported" are banned, because every member of a governing party opposes censure motions and every opposition member supports them, so such phrases carry no information. Copy bill titles EXACTLY as they appear in the data, letter for letter.

Strictly descriptive: never praise or criticise; no evaluative adjectives (loyal, rebellious, strong, poor, impressive); no motives, ideology or character; nothing not present in the data. Australian English spelling (favour, criticise, labour). Return JSON {{"note": "..."}}"#,
        name = person.name,
        group = person.group,
        house = match person.house {
            House::Senate => "Senate",
            House::Representatives => "House of Representatives",
        },
        voted = votes.len(),
        total = chamber_divisions,
        crossed = crossed,
        plural = if crossed == 1 { "" } else { "s" },
        lines = lines,
    );

    let text = gemini.generate(&prompt).await?;
    let parsed: Value = serde_json::from_str(&text)?;
    let note = parsed
        .get("note")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| anyhow!("gemini: missing note field"))?;
    Ok(note.to_string())
}

async fn generate_batch(
    gemini: &Gemini,
    batch: &[&Division],
    bills: &IndexMap<String, Bill>,
    by_day: &IndexMap<String, Vec<&Division>>,
) -> Result<IndexMap<String, String>> {
    let items = batch
        .iter()
        .map(|d| {
            let related = d
                .bill_ids
                .iter()
                .filter_map(|id| bills.get(id))
                .map(|b| {
                    format!(
                        "- {}: {}",
                        b.title,
                        b.summary.as_deref().unwrap_or("no official summary")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let same_day = by_day
                .get(&format!("{}/{}", d.house, d.date))
                .map(|list| {
                    list.iter()
                        .filter(|s| s.id != d.id)
                        .map(|s| format!("- (division {}) {}", s.number, s.name))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            format!(
                "<division id=\"{id}\">\nMotion: {name} (division {number} of the day)\nHouse: {house}, Date: {date}\n{related_block}{same_day_block}Official record excerpt:\n{excerpt}\n</division>",
                id = d.id,
                name = d.name,
                number = d.number,
                house = match d.house {
                    House::Senate => "Senate",
                    House::Representatives => "House of Representatives",
                },
                date = d.date,
                related_block = if related.is_empty() {
                    String::new()
                } else {
                    format!("Related bills (official summaries):\n{related}\n")
                },
                same_day_block = if same_day.is_empty() {
                    String::new()
                } else {
                    format!("Other divisions in this chamber the same day, in order:\n{same_day}\n")
                },
                excerpt = utf16_slice(d.summary.as_deref().unwrap_or(""), 2200),
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let prompt = format!(
        r#"You write neutral context notes for recorded votes (divisions) in the Australian federal parliament.
For EACH division below, write one to two sentences of CONTEXT: what the bill, amendment or motion is about in substance, and what question was actually being decided (for example, a second reading decides whether to agree with a bill in principle; a rearrangement motion changes the order of business).
For procedural motions (rearrangement, suspension of standing orders, closure, adjournment), use the same-day division list to say what business the motion concerned when it is evident, for example which bill was brought on for debate or voted on soon after. Copy bill titles exactly as given.
The page already shows the result and the vote counts, so NEVER say whether it passed or failed, never mention the numbers, and do not describe the outcome at all.
Rules: use ONLY the provided material; no opinions, no speculation about motives; plain Australian English; do not evaluate any politician or law.
Return JSON: an array of {{"id": "<division id>", "summary": "<text>"}} covering every division.

{items}"#
    );

    let text = gemini.generate(&prompt).await?;
    let mut out = IndexMap::new();
    let valid_ids: HashSet<&str> = batch.iter().map(|d| d.id.as_str()).collect();
    let rows: Vec<Value> = serde_json::from_str(&text)?;
    for row in rows {
        let (Some(id), Some(summary)) = (
            row.get("id").and_then(Value::as_str),
            row.get("summary").and_then(Value::as_str),
        ) else {
            continue;
        };
        if !summary.is_empty() && valid_ids.contains(id) {
            out.insert(id.to_string(), summary.trim().to_string());
        }
    }
    Ok(out)
}

/// String.prototype.slice counts UTF-16 code units.
fn utf16_slice(input: &str, max_units: usize) -> String {
    let units: Vec<u16> = input.encode_utf16().take(max_units).collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{LocalStore, Store};
    use crate::test_http::{Response, TestServer};
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn scratch(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/summarise-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn new_store(name: &str) -> Store {
        Store::Local(LocalStore::new(scratch(name)))
    }

    /// Points the flow at a local server and keeps the pacing out of the way;
    /// the per-host spacing is what the real free tier needs, not the test.
    fn gemini(server: &TestServer) -> Gemini {
        Gemini {
            base_url: server.base.clone(),
            api_key: "test-key".to_string(),
            model: "test-model".to_string(),
            min_interval_ms: 1,
            request_cap: 80,
        }
    }

    /// The envelope the real API returns: the model's JSON arrives as text
    /// inside a candidate part.
    fn candidate(text: &str) -> String {
        serde_json::json!({
            "candidates": [{ "content": { "parts": [{ "text": text }] } }]
        })
        .to_string()
    }

    fn division(id: &str, number: i64, name: &str, summary: Option<&str>) -> Division {
        let mut d: Division = serde_json::from_str(&format!(
            r#"{{"id":"{id}","house":"representatives","date":"2026-08-12","number":{number},
                 "name":"{name}","result":"passed","ayes":80,"noes":40}}"#
        ))
        .expect("division fixture");
        d.summary = summary.map(str::to_string);
        d
    }

    fn person(slug: &str, name: &str) -> Person {
        serde_json::from_str(&format!(
            r#"{{"slug":"{slug}","name":"{name}","house":"representatives",
                 "group":"Example","groupSlug":"example","ids":{{}},"links":{{}}}}"#
        ))
        .expect("person fixture")
    }

    fn bill(id: &str, title: &str, summary: Option<&str>) -> Bill {
        let mut b: Bill = serde_json::from_str(&format!(
            r#"{{"id":"{id}","title":"{title}","parliament":48,"chamber":"representatives",
                 "status":"Before the House"}}"#
        ))
        .expect("bill fixture");
        b.summary = summary.map(str::to_string);
        b
    }

    fn vote(slug: &str, crossed: bool) -> pollywiki_schema::VoteCast {
        serde_json::from_str(&format!(
            r#"{{"personSlug":"{slug}","vote":"aye","againstGroupMajority":{crossed}}}"#
        ))
        .expect("vote fixture")
    }

    #[test]
    fn derived_keys_are_stable_and_namespaced() {
        assert_eq!(
            ai_key("representatives/2026-08-12/7"),
            "derived/ai-summaries/representatives-2026-08-12-7.json"
        );
        assert_eq!(
            note_key("alex-paterson"),
            "derived/ai-person-notes/alex-paterson.json"
        );
        assert_eq!(bill_note_key("r7123"), "derived/ai-bill-notes/r7123.json");
    }

    #[test]
    fn utf16_slicing_never_panics_on_a_split_surrogate_pair() {
        assert_eq!(utf16_slice("hello", 4), "hell");
        assert_eq!(utf16_slice("hello", 99), "hello");
        assert_eq!(utf16_slice("", 4), "");
        // An emoji is two utf-16 units. Cutting between them must degrade
        // lossily rather than panic, which is why the cap is counted in units.
        let pair = "a\u{1f600}b";
        assert_eq!(utf16_slice(pair, 1), "a");
        assert_eq!(utf16_slice(pair, 3), "a\u{1f600}");
        assert_eq!(utf16_slice(pair, 2).chars().count(), 2);
    }

    #[test]
    fn the_request_cap_is_always_a_usable_number() {
        assert!(request_cap() > 0);
    }

    #[test]
    fn transcripts_are_marked_by_a_bare_member_name_line() {
        let names: HashSet<String> = ["Sue Lines".to_string()].into();
        assert!(is_transcript(
            "Sue Lines\n\nThe question is that...",
            &names
        ));
        assert!(is_transcript(
            "Context first.\n  Sue Lines  \nMore.",
            &names
        ));
        assert!(!is_transcript(
            "A written summary mentioning Sue Lines inline.",
            &names
        ));
    }
    #[tokio::test]
    async fn a_transcript_division_gets_a_summary_and_a_written_one_does_not() {
        let server = TestServer::start(|req| {
            // The prompt must carry the transcript and the same-day ledger.
            assert!(req.body.contains("Rearrangement"), "prompt lost the motion");
            Response::json(candidate(
                &serde_json::json!([
                    { "id": "representatives/2026-08-12/1", "summary": "Context for the motion." },
                    { "id": "not-in-this-batch", "summary": "ignored" }
                ])
                .to_string(),
            ))
        });
        let store = new_store("transcript");
        let names: HashSet<String> = ["Milton Dick".to_string()].into();
        let transcript = division(
            "representatives/2026-08-12/1",
            1,
            "Rearrangement",
            Some("Milton Dick\n\nThe question is that the motion be agreed to."),
        );
        let written = division(
            "representatives/2026-08-12/2",
            2,
            "Second reading",
            Some("A volunteer-written summary of the bill."),
        );
        let divisions = vec![transcript.clone(), written.clone()];

        division_summaries(
            &store,
            &gemini(&server),
            &divisions,
            &names,
            &IndexMap::new(),
        )
        .await
        .expect("division summaries");

        let stored: Option<AiSummary> = store.get_json(&ai_key(&transcript.id)).await.unwrap();
        let stored = stored.expect("transcript division summarised");
        assert_eq!(stored.text, "Context for the motion.");
        assert_eq!(stored.model, "test-model");
        assert_eq!(stored.prompt_version, Some(SUMMARY_PROMPT_VERSION));
        // A written summary is already context; it is never sent to the model.
        let skipped: Option<AiSummary> = store.get_json(&ai_key(&written.id)).await.unwrap();
        assert!(skipped.is_none(), "written summaries are left alone");
        assert_eq!(server.hits(), 1, "one batch, one call");
    }

    #[tokio::test]
    async fn a_summary_at_the_current_prompt_version_is_not_regenerated() {
        let server = TestServer::start(|_| Response::json(candidate("[]")));
        let store = new_store("uptodate");
        let names: HashSet<String> = ["Milton Dick".to_string()].into();
        let d = division(
            "representatives/2026-08-12/1",
            1,
            "Rearrangement",
            Some("Milton Dick\n\nQ."),
        );
        store
            .put_json(
                &ai_key(&d.id),
                &AiSummary {
                    text: "Already written.".to_string(),
                    model: "old-model".to_string(),
                    generated_at: "2026-08-01T00:00:00.000Z".to_string(),
                    prompt_version: Some(SUMMARY_PROMPT_VERSION),
                },
            )
            .await
            .unwrap();

        division_summaries(
            &store,
            &gemini(&server),
            std::slice::from_ref(&d),
            &names,
            &IndexMap::new(),
        )
        .await
        .expect("no work");
        assert_eq!(server.hits(), 0, "nothing pending, nothing called");

        // A prompt bump is what forces the rewrite, so an older version does.
        store
            .put_json(
                &ai_key(&d.id),
                &AiSummary {
                    text: "Stale.".to_string(),
                    model: "old-model".to_string(),
                    generated_at: "2026-08-01T00:00:00.000Z".to_string(),
                    prompt_version: Some(SUMMARY_PROMPT_VERSION - 1),
                },
            )
            .await
            .unwrap();
        division_summaries(&store, &gemini(&server), &[d], &names, &IndexMap::new())
            .await
            .expect("regen");
        assert_eq!(server.hits(), 1, "an older prompt version is regenerated");
    }

    #[tokio::test]
    async fn the_request_cap_stops_the_run_and_leaves_the_rest_pending() {
        let server = TestServer::start(|_| {
            Response::json(candidate(
                &serde_json::json!([{ "id": "x", "summary": "s" }]).to_string(),
            ))
        });
        let store = new_store("cap");
        let names: HashSet<String> = ["Milton Dick".to_string()].into();
        // Two batches' worth of pending transcript divisions, cap of one call.
        let divisions: Vec<Division> = (1..=BATCH_SIZE as i64 + 2)
            .map(|n| {
                division(
                    &format!("representatives/2026-08-12/{n}"),
                    n,
                    "Rearrangement",
                    Some("Milton Dick\n\nQ."),
                )
            })
            .collect();
        let mut capped = gemini(&server);
        capped.request_cap = 1;

        division_summaries(&store, &capped, &divisions, &names, &IndexMap::new())
            .await
            .expect("capped run");
        assert_eq!(server.hits(), 1, "the cap is a hard stop, not a target");
    }

    #[tokio::test]
    async fn a_failing_batch_is_skipped_until_five_of_them_abort_the_run() {
        let store = new_store("failures");
        let names: HashSet<String> = ["Milton Dick".to_string()].into();
        let divisions: Vec<Division> = (1..=BATCH_SIZE as i64 * 6)
            .map(|n| {
                division(
                    &format!("representatives/2026-08-12/{n}"),
                    n,
                    "Rearrangement",
                    Some("Milton Dick\n\nQ."),
                )
            })
            .collect();

        // Every call fails, so the fifth failure aborts rather than grinding on.
        // A 400 is what a rejected request actually returns, and unlike a 5xx it
        // is not retried, so the test does not sit through the backoffs.
        let server = TestServer::start(|_| Response::status(400, "bad request"));
        let err = division_summaries(
            &store,
            &gemini(&server),
            &divisions,
            &names,
            &IndexMap::new(),
        )
        .await
        .expect_err("five failures abort");
        assert!(err.to_string().contains("too many failed batches"));

        // A single bad batch among good ones is survivable: the division simply
        // stays pending for the next run.
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let flaky = TestServer::start(move |_| match counter.fetch_add(1, Ordering::SeqCst) {
            0 => Response::status(400, "one bad batch"),
            _ => Response::json(candidate(
                &serde_json::json!([{ "id": "representatives/2026-08-12/9", "summary": "ok" }])
                    .to_string(),
            )),
        });
        let store = new_store("flaky");
        division_summaries(
            &store,
            &gemini(&flaky),
            &divisions,
            &names,
            &IndexMap::new(),
        )
        .await
        .expect("one bad batch is not fatal");
        let saved: Option<AiSummary> = store
            .get_json(&ai_key("representatives/2026-08-12/9"))
            .await
            .unwrap();
        assert!(saved.is_some(), "later batches still land");
    }

    #[tokio::test]
    async fn an_empty_bill_note_is_stored_so_the_bill_is_never_retried() {
        let server = TestServer::start(|_| {
            Response::json(candidate(
                &serde_json::json!([
                    { "id": "r1", "summary": "" },
                    { "id": "r2", "summary": "  Plain-English note.  " }
                ])
                .to_string(),
            ))
        });
        let store = new_store("billnotes");
        let plain = bill(
            "r1",
            "Short Plain Bill 2026",
            Some("Amends the start date."),
        );
        let dense = bill("r2", "Dense Bill 2026", Some("Standing appropriations."));

        bill_notes(&store, &gemini(&server), &[&plain, &dense])
            .await
            .expect("bill notes");

        // The empty text is the model's judgement that no note is needed. It is
        // recorded, not discarded, or every run would ask again.
        let skipped: AiSummary = store
            .get_json(&bill_note_key("r1"))
            .await
            .unwrap()
            .expect("empty note recorded");
        assert_eq!(skipped.text, "");
        assert_eq!(skipped.prompt_version, Some(BILL_NOTE_PROMPT_VERSION));
        let written: AiSummary = store
            .get_json(&bill_note_key("r2"))
            .await
            .unwrap()
            .expect("note written");
        assert_eq!(written.text, "Plain-English note.", "text is trimmed");

        // Second pass: both are at the current version, so nothing is sent.
        bill_notes(&store, &gemini(&server), &[&plain, &dense])
            .await
            .expect("second pass");
        assert_eq!(server.hits(), 1, "recorded notes are not regenerated");
    }

    #[tokio::test]
    async fn person_notes_need_ten_votes_and_regenerate_only_when_the_record_moves() {
        let server = TestServer::start(|req| {
            assert!(req.body.contains("Alex Paterson"), "prompt lost the member");
            Response::json(candidate(
                &serde_json::json!({ "note": "Voted aye on the appropriation bills." }).to_string(),
            ))
        });
        let store = new_store("notes");
        let member = person("alex-paterson", "Alex Paterson");
        let quiet = person("quiet-member", "Quiet Member");

        // Nine divisions is under the floor; the tenth crosses it.
        let mut divisions: Vec<Division> = (1..=9)
            .map(|n| {
                let mut d = division(
                    &format!("representatives/2026-08-12/{n}"),
                    n,
                    "Second reading",
                    None,
                );
                d.votes = vec![vote("alex-paterson", false), vote("quiet-member", false)];
                d
            })
            .collect();
        person_notes(
            &store,
            &gemini(&server),
            std::slice::from_ref(&member),
            &divisions,
        )
        .await
        .expect("under the floor");
        assert_eq!(server.hits(), 0, "nine votes is not enough to describe");

        let mut tenth = division("representatives/2026-08-12/10", 10, "Censure motion", None);
        tenth.votes = vec![vote("alex-paterson", true)];
        divisions.push(tenth);
        person_notes(
            &store,
            &gemini(&server),
            &[member.clone(), quiet.clone()],
            &divisions,
        )
        .await
        .expect("note written");
        let note: AiPersonNote = store
            .get_json(&note_key("alex-paterson"))
            .await
            .unwrap()
            .expect("note stored");
        assert_eq!(note.text, "Voted aye on the appropriation bills.");
        assert_eq!(note.votes_considered, 10);
        assert_eq!(note.prompt_version, Some(NOTE_PROMPT_VERSION));
        assert!(
            store
                .get_json::<AiPersonNote>(&note_key("quiet-member"))
                .await
                .unwrap()
                .is_none(),
            "nine votes still earns no note"
        );
        let after_first = server.hits();

        // Same vote count and prompt version: nothing to say that is new.
        person_notes(
            &store,
            &gemini(&server),
            std::slice::from_ref(&member),
            &divisions,
        )
        .await
        .expect("no churn");
        assert_eq!(
            server.hits(),
            after_first,
            "an unchanged record is left alone"
        );

        // One more vote and the note is rewritten.
        let mut eleventh = division("representatives/2026-08-12/11", 11, "Third reading", None);
        eleventh.votes = vec![vote("alex-paterson", false)];
        divisions.push(eleventh);
        person_notes(&store, &gemini(&server), &[member], &divisions)
            .await
            .expect("regen");
        assert_eq!(
            server.hits(),
            after_first + 1,
            "a moved record is rewritten"
        );
    }

    #[tokio::test]
    async fn a_model_that_answers_with_nothing_usable_is_an_error() {
        let store = new_store("garbage");
        let member = person("alex-paterson", "Alex Paterson");
        let divisions: Vec<Division> = (1..=10)
            .map(|n| {
                let mut d = division(
                    &format!("representatives/2026-08-12/{n}"),
                    n,
                    "Second reading",
                    None,
                );
                d.votes = vec![vote("alex-paterson", false)];
                d
            })
            .collect();

        // An envelope with no candidate text at all.
        let empty = TestServer::start(|_| Response::json("{\"candidates\":[]}"));
        let g = gemini(&empty);
        assert!(
            g.generate("prompt").await.is_err(),
            "no candidate is an error"
        );

        // A candidate whose text is not the JSON the prompt asked for: the note
        // is skipped, and the person stays pending.
        let prose = TestServer::start(|_| Response::json(candidate("Sorry, I cannot help.")));
        person_notes(
            &store,
            &gemini(&prose),
            std::slice::from_ref(&member),
            &divisions,
        )
        .await
        .expect("unparseable answers are skipped, not fatal");
        assert!(
            store
                .get_json::<AiPersonNote>(&note_key("alex-paterson"))
                .await
                .unwrap()
                .is_none(),
            "nothing is stored from an unusable answer"
        );

        // JSON, but the note field is blank.
        let blank = TestServer::start(|_| {
            Response::json(candidate(&serde_json::json!({ "note": "   " }).to_string()))
        });
        person_notes(&store, &gemini(&blank), &[member], &divisions)
            .await
            .expect("blank note skipped");
        assert!(store
            .get_json::<AiPersonNote>(&note_key("alex-paterson"))
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn the_request_carries_the_key_the_model_and_the_prompt() {
        let server = TestServer::start(|_| Response::json(candidate("[]")));
        let g = gemini(&server);
        g.generate("the prompt text").await.expect("generate");

        let sent = server.requests();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].method, "POST");
        assert!(
            sent[0].path.contains("/models/test-model:generateContent"),
            "model belongs in the path: {}",
            sent[0].path
        );
        assert_eq!(sent[0].header("x-goog-api-key"), Some("test-key"));
        let body: Value = serde_json::from_str(&sent[0].body).expect("json body");
        assert_eq!(
            body.pointer("/contents/0/parts/0/text")
                .and_then(Value::as_str),
            Some("the prompt text")
        );
        assert_eq!(
            body.pointer("/generationConfig/responseMimeType")
                .and_then(Value::as_str),
            Some("application/json"),
            "the prompts all ask for JSON back"
        );
    }

    #[tokio::test]
    async fn the_whole_flow_reads_canonical_records_and_writes_all_three_layers() {
        let server = TestServer::start(|req| {
            let answer = if req.body.contains("voting records") {
                serde_json::json!({ "note": "Voted aye on the appropriation bills." }).to_string()
            } else if req.body.contains("explain bills") {
                serde_json::json!([{ "id": "r1", "summary": "What the bill does." }]).to_string()
            } else {
                serde_json::json!([{
                    "id": "representatives/2026-08-12/1",
                    "summary": "Context for the motion."
                }])
                .to_string()
            };
            Response::json(candidate(&answer))
        });
        let store = new_store("wholeflow");
        let member = person("alex-paterson", "Alex Paterson");

        let mut transcript = division(
            "representatives/2026-08-12/1",
            1,
            "Rearrangement",
            Some("Alex Paterson\n\nThe question is that the motion be agreed to."),
        );
        transcript.votes = vec![vote("alex-paterson", false)];
        store
            .put_json(
                "canonical/divisions/representatives-2026-08-12-1.json",
                &transcript,
            )
            .await
            .unwrap();
        for n in 2..=10 {
            let mut d = division(
                &format!("representatives/2026-08-12/{n}"),
                n,
                "Second reading",
                None,
            );
            d.votes = vec![vote("alex-paterson", false)];
            store
                .put_json(
                    &format!("canonical/divisions/representatives-2026-08-12-{n}.json"),
                    &d,
                )
                .await
                .unwrap();
        }
        store
            .put_json(
                "canonical/bills/r1.json",
                &bill(
                    "r1",
                    "Appropriation Bill 2026",
                    Some("Standing appropriations."),
                ),
            )
            .await
            .unwrap();

        summarise_with(&store, &[member], &gemini(&server))
            .await
            .expect("whole flow");

        assert!(
            store
                .get_json::<AiSummary>(&ai_key("representatives/2026-08-12/1"))
                .await
                .unwrap()
                .is_some(),
            "division summary written"
        );
        assert!(
            store
                .get_json::<AiSummary>(&bill_note_key("r1"))
                .await
                .unwrap()
                .is_some(),
            "bill note written"
        );
        assert!(
            store
                .get_json::<AiPersonNote>(&note_key("alex-paterson"))
                .await
                .unwrap()
                .is_some(),
            "person note written"
        );
    }
}
