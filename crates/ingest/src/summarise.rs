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

/// Free-tier headroom: nightly needs a handful; the backfill spans a few runs.
fn request_cap() -> u32 {
    std::env::var("GEMINI_REQUEST_CAP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(80)
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
    let api_key = std::env::var("GEMINI_API_KEY")
        .map_err(|_| anyhow!("GEMINI_API_KEY not set; skipping AI summaries"))?;
    let model = std::env::var("GEMINI_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
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

    division_summaries(store, &api_key, &model, &divisions, &names, &bills).await?;
    let bill_list: Vec<&Bill> = bills.values().collect();
    bill_notes(store, &api_key, &model, &bill_list).await?;
    person_notes(store, &api_key, &model, people, &divisions).await?;
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
async fn bill_notes(store: &Store, api_key: &str, model: &str, bills: &[&Bill]) -> Result<()> {
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

    let cap = request_cap();
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
        match generate_bill_batch(api_key, model, batch).await {
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
                                model: model.to_string(),
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
    println!("summarise: wrote {written} bill notes (model {model}, {failed} failed batches)");
    Ok(())
}

async fn generate_bill_batch(
    api_key: &str,
    model: &str,
    batch: &[&Bill],
) -> Result<IndexMap<String, String>> {
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

    let text = gemini_generate(api_key, model, &prompt).await?;
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
    api_key: &str,
    model: &str,
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

    let cap = request_cap();
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
        match generate_batch(api_key, model, batch, bills, &by_day).await {
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
                                model: model.to_string(),
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
        "summarise: wrote {written} of {} pending (model {model}, {failed_batches} failed batches)",
        pending.len()
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
    api_key: &str,
    model: &str,
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

    let cap = request_cap();
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
        match generate_person_note(api_key, model, person, votes, chamber_divisions).await {
            Ok(text) => {
                store
                    .put_json(
                        &note_key(&person.slug),
                        &AiPersonNote {
                            text,
                            model: model.to_string(),
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
    println!("summarise: wrote {written} person notes (model {model}, {failed} failed)");
    Ok(())
}

async fn generate_person_note(
    api_key: &str,
    model: &str,
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

    let text = gemini_generate(api_key, model, &prompt).await?;
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
    api_key: &str,
    model: &str,
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

    let text = gemini_generate(api_key, model, &prompt).await?;
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

/// politeFetch paces requests per host and retries 429/5xx with backoff,
/// which keeps the run inside the free-tier per-minute limits.
async fn gemini_generate(api_key: &str, model: &str, prompt: &str) -> Result<String> {
    let body = serde_json::json!({
        "contents": [{ "parts": [{ "text": prompt }] }],
        "generationConfig": { "responseMimeType": "application/json", "temperature": 0.2 },
    });
    let mut opts = FetchOpts::min_interval(6500).with_header("x-goog-api-key", api_key);
    opts.post_json = Some(serde_json::to_string(&body)?);
    let res = polite_fetch(
        &format!("https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent"),
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

/// String.prototype.slice counts UTF-16 code units.
fn utf16_slice(input: &str, max_units: usize) -> String {
    let units: Vec<u16> = input.encode_utf16().take(max_units).collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

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
}
