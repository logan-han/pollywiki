use crate::http::{polite_fetch, FetchOpts};
use crate::sources::tvfy::key_for;
use crate::store::Store;
use crate::tally::{self, Tally, VoteRow};
use crate::telemetry::{CallStatus, Langfuse, ModelCall, Usage};
use anyhow::{anyhow, Result};
use indexmap::IndexMap;
use pollywiki_schema::{Bill, Division, House, Person};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::time::SystemTime;

const DEFAULT_MODEL: &str = "gemini-3.1-flash-lite";
const BATCH_SIZE: usize = 8;
const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Failures in a row before a job gives up for the night. In a row, not in
/// total: an overloaded model answers 503 to every third call for a few
/// minutes and then recovers, and the night that happened five scattered
/// failures among eight successes stopped the person notes at the letter A.
const MAX_FAILURES_IN_A_ROW: usize = 5;

/// Free-tier headroom: nightly needs a handful; the backfill spans a few runs.
fn request_cap() -> u32 {
    std::env::var("GEMINI_REQUEST_CAP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(80)
}

/// A prompt as the code holds it: the template with its `{{variables}}`,
/// the name it is traced and mirrored under, and the version that decides
/// when text written with it is regenerated. Langfuse prompt management
/// keeps a copy of every template that has run (see
/// `Gemini::with_prompts_registered`), so each generation there links to
/// the exact text that produced it.
pub struct Prompt {
    pub name: &'static str,
    /// Bump to regenerate everything written with this prompt.
    pub version: i64,
    pub template: &'static str,
}

impl Prompt {
    fn fill(&self, vars: &[(&str, &str)]) -> String {
        vars.iter()
            .fold(self.template.to_string(), |text, (name, value)| {
                text.replace(&format!("{{{{{name}}}}}"), value)
            })
    }
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
    /// Langfuse tracing of every call, when its keys are set.
    pub telemetry: Option<Langfuse>,
    /// The Langfuse prompt-management version of each prompt's template, by
    /// prompt name, once registered; a generation links to it.
    pub prompt_versions: HashMap<&'static str, i64>,
}

/// What a parser made of the model's text, plus anything it had to leave
/// out, which the trace records as a warning.
pub struct Parsed<T> {
    pub value: T,
    pub warning: Option<String>,
}

impl<T> Parsed<T> {
    fn clean(value: T) -> Self {
        Parsed {
            value,
            warning: None,
        }
    }
}

/// A reply the API gave: the text, what it cost and why it stopped.
struct Reply {
    text: String,
    usage: Option<Usage>,
    /// Gemini's finishReason: STOP for a complete answer, otherwise why the
    /// text ends where it does (MAX_TOKENS, SAFETY, ...).
    finish_reason: Option<String>,
    /// The concrete model version that served the call, when reported.
    model_version: Option<String>,
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
            telemetry: Langfuse::from_env().inspect(|_| {
                println!("summarise: Langfuse tracing on");
            }),
            prompt_versions: HashMap::new(),
        })
    }

    /// Mirrors each prompt's template into Langfuse prompt management and
    /// remembers the version there whose text it is, so every generation
    /// links to the exact text that produced it. Code stays the source of
    /// truth: a template changed here becomes a new version there on the
    /// next run, and an edit made there is superseded the same way. Without
    /// tracing, or when Langfuse cannot be reached, calls simply go unlinked.
    pub async fn with_prompts_registered(&self) -> Gemini {
        let mut gemini = self.clone();
        let Some(telemetry) = &self.telemetry else {
            return gemini;
        };
        for prompt in PROMPTS {
            match telemetry
                .register_prompt(prompt.name, prompt.template)
                .await
            {
                Ok(version) => {
                    gemini.prompt_versions.insert(prompt.name, version);
                }
                Err(err) => eprintln!(
                    "telemetry: {} not registered in Langfuse prompt management: {err}",
                    prompt.name
                ),
            }
        }
        if !gemini.prompt_versions.is_empty() {
            let listed = PROMPTS
                .iter()
                .filter_map(|p| {
                    gemini
                        .prompt_versions
                        .get(p.name)
                        .map(|v| format!("{} {v}", p.name))
                })
                .collect::<Vec<_>>()
                .join(", ");
            println!("summarise: prompts registered in Langfuse ({listed})");
        }
        gemini
    }

    /// One call to the model, parsed and traced as a unit under the prompt's
    /// name. The parse runs inside the trace so that a reply the job could
    /// not use is an error on the generation in Langfuse, with the text that
    /// was unusable, and a reply that left items out is a warning there,
    /// rather than either being a success with a line in the workflow log.
    async fn generate<T>(
        &self,
        prompt: &Prompt,
        text: &str,
        metadata: &[(&str, String)],
        parse: impl FnOnce(&str) -> Result<Parsed<T>>,
    ) -> Result<T> {
        let parameters =
            serde_json::json!({ "responseMimeType": "application/json", "temperature": 0.2 });
        let started = SystemTime::now();
        let (reply, result) = match self.call(text, &parameters).await {
            Ok(reply) => {
                let result = parse(&reply.text);
                (Some(reply), result)
            }
            Err(err) => (None, Err(err)),
        };
        if let Some(telemetry) = &self.telemetry {
            let mut metadata: Vec<(&str, String)> = metadata.to_vec();
            if let Some(reason) = reply.as_ref().and_then(|r| r.finish_reason.clone()) {
                metadata.push(("finish_reason", reason));
            }
            if let Some(model_version) = reply.as_ref().and_then(|r| r.model_version.clone()) {
                metadata.push(("model_version", model_version));
            }
            let status = match (&reply, &result) {
                (
                    _,
                    Ok(Parsed {
                        warning: Some(warning),
                        ..
                    }),
                ) => CallStatus::Warning(warning.clone()),
                (_, Ok(_)) => CallStatus::Ok,
                (Some(_), Err(err)) => CallStatus::Error(format!("unusable reply: {err}")),
                (None, Err(err)) => CallStatus::Error(err.to_string()),
            };
            let version = format!("v{}", prompt.version);
            telemetry
                .record(&ModelCall {
                    name: prompt.name,
                    version: &version,
                    prompt_version: self.prompt_versions.get(prompt.name).copied(),
                    model: &self.model,
                    parameters: &parameters,
                    prompt: text,
                    output: reply.as_ref().map(|r| r.text.as_str()),
                    status,
                    usage: reply.as_ref().and_then(|r| r.usage),
                    started,
                    ended: SystemTime::now(),
                    metadata: &metadata,
                })
                .await;
        }
        result.map(|parsed| parsed.value)
    }

    /// politeFetch paces requests per host and retries 429/5xx with backoff,
    /// which keeps the run inside the free-tier per-minute limits.
    async fn call(&self, prompt: &str, parameters: &Value) -> Result<Reply> {
        let body = serde_json::json!({
            "contents": [{ "parts": [{ "text": prompt }] }],
            "generationConfig": parameters,
        });
        let mut opts = FetchOpts::min_interval(self.min_interval_ms)
            .with_header("x-goog-api-key", &self.api_key);
        opts.post_json = Some(serde_json::to_string(&body)?);
        // An overloaded model answers 503 for minutes at a time, and the
        // default two-second steps burn every attempt before it clears.
        // Twenty-second steps (20, 40, then 60 seconds) ride out most windows.
        opts.backoff_ms = Some(20_000);
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
        let text_at = |pointer: &str| {
            data.pointer(pointer)
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let finish_reason = text_at("/candidates/0/finishReason");
        let model_version = text_at("/modelVersion");
        let Some(text) = text_at("/candidates/0/content/parts/0/text").filter(|t| !t.is_empty())
        else {
            // Why there is no text is the useful part: a reply cut off at the
            // token limit and one blocked for safety call for different fixes.
            let why = finish_reason
                .clone()
                .or_else(|| text_at("/promptFeedback/blockReason"))
                .map(|reason| format!(" ({reason})"))
                .unwrap_or_default();
            return Err(anyhow!("gemini: empty response{why}"));
        };
        Ok(Reply {
            text,
            usage: Usage::from_gemini(&data),
            finish_reason,
            model_version,
        })
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
    /// Votes against the member's grouping when the note was written; one
    /// more is a reason to rewrite at once.
    #[serde(default)]
    pub crossed_considered: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_version: Option<i64>,
}

/// Bump to regenerate every division summary after a prompt change.
const SUMMARY_PROMPT_VERSION: i64 = 3;

/// Bump to regenerate every bill note after a prompt change.
const BILL_NOTE_PROMPT_VERSION: i64 = 3;

/// Bump to regenerate every note after a prompt change.
const NOTE_PROMPT_VERSION: i64 = 4;

/// Plain-English context per bill. Unlike division summaries, general
/// knowledge of Australian parliamentary practice is allowed to explain the
/// MECHANISM (what an appropriation bill is); anything specific to this bill
/// must come from the official summary alone.
const BILL_NOTES: Prompt = Prompt {
    name: "pollywiki.bill-notes",
    version: BILL_NOTE_PROMPT_VERSION,
    template: r#"You explain bills before the Australian federal parliament to ordinary readers.
For EACH bill below, decide whether an explanation adds anything, then write accordingly:
- Return an EMPTY string ONLY when the official summary is short AND every term in it would be understood by an ordinary reader with no knowledge of parliament or law. A note that merely rephrases such a summary is worse than none.
- If the summary is short but contains ANY technical, legal or institutional language — terms like "Consolidated Revenue Fund", "standing appropriations", "delegated legislation", "consequential amendments", or the mechanics of a named act — write ONE sentence that translates the jargon and mechanism into plain terms. Do not restate what is already plain. When unsure, write the sentence.
- If the summary is long or dense, write two to three sentences of plain-English context: what this KIND of bill does in the Australian system where that helps (for example, appropriation bills are the routine budget bills that authorise the government to spend money on its ordinary operations) — general knowledge of parliamentary practice is allowed for the mechanism — and what THIS bill specifically changes, using ONLY the official summary provided; never invent specifics.
Never evaluate the bill or any politician; no opinions; do not restate the bill's status. Plain Australian English.
Return JSON: an array of {"id": "<bill id>", "summary": "<text or empty string>"} covering every bill.

{{items}}"#,
};

/// Neutral context for a division whose only They Vote For You context is a
/// Hansard transcript.
const DIVISION_CONTEXT: Prompt = Prompt {
    name: "pollywiki.division-context",
    version: SUMMARY_PROMPT_VERSION,
    template: r#"You write neutral context notes for recorded votes (divisions) in the Australian federal parliament.
For EACH division below, write one to two sentences of CONTEXT: what the bill, amendment or motion is about in substance, and what question was actually being decided (for example, a second reading decides whether to agree with a bill in principle; a rearrangement motion changes the order of business).
For procedural motions (rearrangement, suspension of standing orders, closure, adjournment), use the same-day division list to say what business the motion concerned when it is evident, for example which bill was brought on for debate or voted on soon after. Copy bill titles exactly as given.
The page already shows the result and the vote counts, so NEVER say whether it passed or failed, never mention the numbers, and do not describe the outcome at all.
Rules: use ONLY the provided material; no opinions, no speculation about motives; plain Australian English; do not evaluate any politician or law.
Return JSON: an array of {"id": "<division id>", "summary": "<text>"} covering every division.

{{items}}"#,
};

/// A member's voting record in brief, phrased from sums the code has already
/// done (see `tally`). The model never sees the vote list, so it cannot
/// round a mixed record into "consistently" or "all".
const MEMBER_NOTE: Prompt = Prompt {
    name: "pollywiki.member-note",
    version: NOTE_PROMPT_VERSION,
    template: r#"You describe voting records in the Australian federal parliament, strictly descriptively.

{{record}}
Every count above was computed from the record: "aye 3, no 11" means three aye votes and eleven no votes among those divisions.

The page already displays the two counts in the first line, so NEVER restate them, never say they always voted with their party, and never say they did not cross. Write two to three sentences that add what a reader cannot see by scanning a long table:
- the subjects that recur, and WHICH WAY they voted on each, with the numbers: write "no in eleven of the fourteen consideration in detail divisions on <bill>", never "consistently no";
- if they crossed, name the specific division(s) and how they voted;
- any other countable pattern, such as every division on one bill voted the same way.

Use ONLY the counts above and copy them exactly: never add, estimate or round a number, and never describe a vote the counts do not show. Use "all", "every", "always", "never" or "consistently" ONLY where the count shows zero votes the other way, and give the number even then ("aye in all six divisions on ...").

Describe motions by their formal type (second reading, consideration in detail amendment, suspension of standing orders, censure motion, order for the production of documents) and subject. NEVER characterise a motion by political direction or origin: "critical of the government", "opposition-led" and "government-supported" are banned, because every member of a governing party opposes censure motions and every opposition member supports them, so such phrases carry no information. Copy bill titles EXACTLY as they appear in the data, letter for letter.

Strictly descriptive: never praise or criticise; no evaluative adjectives (loyal, rebellious, strong, poor, impressive); no motives, ideology or character; nothing not present in the data. Australian English spelling (favour, criticise, labour). Return JSON {"note": "..."}"#,
};

/// Every prompt, in the order the jobs run.
const PROMPTS: [&Prompt; 3] = [&DIVISION_CONTEXT, &BILL_NOTES, &MEMBER_NOTE];

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
    let gemini = gemini.with_prompts_registered().await;
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

    division_summaries(store, &gemini, &divisions, &names, &bills).await?;
    let bill_list: Vec<&Bill> = bills.values().collect();
    bill_notes(store, &gemini, &bill_list).await?;
    person_notes(store, &gemini, people, &divisions).await?;
    Ok(())
}

/// The rows a batch prompt asked for, keyed by id and limited to the ids it
/// asked about. Ids the model left out (or, when an empty text is not an
/// answer, answered with nothing) are reported for the trace; those items
/// stay pending and are asked again next run.
fn parse_batch(
    text: &str,
    expected: &[&str],
    what: &str,
    empty_is_an_answer: bool,
) -> Result<Parsed<IndexMap<String, String>>> {
    let rows: Vec<Value> = serde_json::from_str(text)?;
    let valid: HashSet<&str> = expected.iter().copied().collect();
    let mut out = IndexMap::new();
    for row in rows {
        let (Some(id), Some(summary)) = (
            row.get("id").and_then(Value::as_str),
            row.get("summary").and_then(Value::as_str),
        ) else {
            continue;
        };
        let summary = summary.trim();
        if valid.contains(id) && (empty_is_an_answer || !summary.is_empty()) {
            out.insert(id.to_string(), summary.to_string());
        }
    }
    let missing: Vec<&str> = expected
        .iter()
        .copied()
        .filter(|id| !out.contains_key(*id))
        .collect();
    let warning = (!missing.is_empty()).then(|| {
        format!(
            "{} of {} {what} missing from the reply: {}",
            missing.len(),
            expected.len(),
            missing.join(",")
        )
    });
    Ok(Parsed {
        value: out,
        warning,
    })
}

pub fn bill_note_key(id: &str) -> String {
    format!("derived/ai-bill-notes/{id}.json")
}

async fn bill_notes(store: &Store, gemini: &Gemini, bills: &[&Bill]) -> Result<()> {
    let mut pending: Vec<&Bill> = Vec::new();
    let mut unsummarised = 0;
    for bill in bills {
        let existing: Option<AiSummary> = store.get_json(&bill_note_key(&bill.id)).await?;
        // A bill with no official summary gets no note: with only a title to
        // go on, the model could but paraphrase it. The bill is looked at
        // again once APH publishes a summary, and a note written from a
        // title alone before this rule is removed.
        if bill
            .summary
            .as_deref()
            .map(str::trim)
            .is_none_or(str::is_empty)
        {
            unsummarised += 1;
            if existing.is_some() {
                store.delete(&bill_note_key(&bill.id)).await?;
            }
            continue;
        }
        if existing.map(|e| e.prompt_version) != Some(Some(BILL_NOTE_PROMPT_VERSION)) {
            pending.push(bill);
        }
    }
    if unsummarised > 0 {
        println!("summarise: {unsummarised} bills have no official summary yet, so no note");
    }
    if pending.is_empty() {
        println!("summarise: no bill notes pending");
        return Ok(());
    }

    let cap = gemini.request_cap;
    let mut requests = 0;
    let mut written = 0;
    let mut failed = 0;
    let mut in_a_row = 0;
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
                in_a_row = 0;
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
                in_a_row += 1;
                eprintln!("summarise: bill batch at {} failed - {err}", i * BATCH_SIZE);
                if in_a_row >= MAX_FAILURES_IN_A_ROW {
                    return Err(anyhow!(
                        "summarise: too many failed bill batches in a row, aborting"
                    ));
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
                b.summary.as_deref().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let ids: Vec<&str> = batch.iter().map(|b| b.id.as_str()).collect();
    gemini
        .generate(
            &BILL_NOTES,
            &BILL_NOTES.fill(&[("items", &items)]),
            &[
                ("bills", ids.join(",")),
                ("prompt_version", BILL_NOTE_PROMPT_VERSION.to_string()),
            ],
            |text| parse_batch(text, &ids, "bills", true),
        )
        .await
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
    let mut in_a_row = 0;
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
                in_a_row = 0;
                for division in batch {
                    let Some(text) = results.get(&division.id) else {
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
                in_a_row += 1;
                eprintln!("summarise: batch at {} failed - {err}", i * BATCH_SIZE);
                if in_a_row >= MAX_FAILURES_IN_A_ROW {
                    return Err(anyhow!(
                        "summarise: too many failed batches in a row, aborting run"
                    ));
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

pub fn note_key(slug: &str) -> String {
    format!("derived/ai-person-notes/{slug}.json")
}

const MIN_VOTES_FOR_NOTE: usize = 10;

/// New divisions a member must have voted in before their note is rewritten.
/// Rewriting after every sitting day churned the text of every note for
/// three nights at a time; ten divisions is about a sitting week.
const REWRITE_AFTER_VOTES: i64 = 10;

/// Whether a stored note still describes the record: written with the
/// current prompt, with the same crossings (one more is news), and before
/// the record grew by `REWRITE_AFTER_VOTES` divisions or shrank at all.
fn note_is_current(existing: &AiPersonNote, votes: usize, crossed: usize) -> bool {
    let grown = votes as i64 - existing.votes_considered;
    existing.prompt_version == Some(NOTE_PROMPT_VERSION)
        && existing.crossed_considered == crossed as i64
        && (0..REWRITE_AFTER_VOTES).contains(&grown)
}

async fn person_notes(
    store: &Store,
    gemini: &Gemini,
    people: &[Person],
    divisions: &[Division],
) -> Result<()> {
    let mut votes_by_slug: IndexMap<&str, Vec<VoteRow>> = IndexMap::new();
    for division in divisions {
        for vote in &division.votes {
            votes_by_slug
                .entry(vote.person_slug.as_str())
                .or_default()
                .push(VoteRow {
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
    let mut in_a_row = 0;
    for person in people {
        let empty = Vec::new();
        let votes = votes_by_slug.get(person.slug.as_str()).unwrap_or(&empty);
        if votes.len() < MIN_VOTES_FOR_NOTE {
            continue;
        }
        let crossed = votes.iter().filter(|v| v.crossed).count();
        let existing: Option<AiPersonNote> = store.get_json(&note_key(&person.slug)).await?;
        // A different model alone is not a reason to rewrite: it would churn
        // every note whenever a quota fallback model runs.
        if existing.is_some_and(|e| note_is_current(&e, votes.len(), crossed)) {
            continue;
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
                in_a_row = 0;
                store
                    .put_json(
                        &note_key(&person.slug),
                        &AiPersonNote {
                            text,
                            model: gemini.model.clone(),
                            generated_at: crate::now_iso(),
                            votes_considered: votes.len() as i64,
                            crossed_considered: crossed as i64,
                            prompt_version: Some(NOTE_PROMPT_VERSION),
                        },
                    )
                    .await?;
                written += 1;
            }
            Err(err) => {
                failed += 1;
                in_a_row += 1;
                eprintln!("summarise: note for {} failed - {err}", person.slug);
                if in_a_row >= MAX_FAILURES_IN_A_ROW {
                    return Err(anyhow!(
                        "summarise: too many failed person notes in a row, aborting"
                    ));
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

/// A number in the note that the record it was written from does not
/// contain. The model is told to copy counts exactly; a figure it made up,
/// or a vote count it worked out for itself, is the failure this catches.
fn unrecorded_number(note: &str, record: &str) -> Option<u64> {
    let recorded = tally::numbers_in(record);
    tally::numbers_in(note)
        .into_iter()
        .find(|n| !recorded.contains(n))
}

/// The note is phrased from sums the code has already done (see `tally`):
/// per bill and stage, per kind of motion, plus the divisions where the
/// member voted against their grouping. A note is only kept if every number
/// in it appears in those sums.
async fn generate_person_note(
    gemini: &Gemini,
    person: &Person,
    votes: &[VoteRow<'_>],
    chamber_divisions: usize,
) -> Result<String> {
    let crossed = votes.iter().filter(|v| v.crossed).count();
    let record = format!(
        "Data for {name} ({group}), {house}:\n\
         - Voted in {voted} of {total} recorded divisions; {crossed} vote{plural} against their party grouping's majority.\n\
         \n\
         {tally}",
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
        tally = Tally::of(votes).render(),
    );

    gemini
        .generate(
            &MEMBER_NOTE,
            &MEMBER_NOTE.fill(&[("record", &record)]),
            &[
                ("person", person.slug.clone()),
                ("prompt_version", NOTE_PROMPT_VERSION.to_string()),
            ],
            |text| {
                let parsed: Value = serde_json::from_str(text)?;
                let note = parsed
                    .get("note")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
                    .ok_or_else(|| anyhow!("gemini: missing note field"))?;
                if let Some(n) = unrecorded_number(note, &record) {
                    return Err(anyhow!("note cites {n}, which is not in the record"));
                }
                Ok(Parsed::clean(note.to_string()))
            },
        )
        .await
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
    let ids: Vec<&str> = batch.iter().map(|d| d.id.as_str()).collect();
    gemini
        .generate(
            &DIVISION_CONTEXT,
            &DIVISION_CONTEXT.fill(&[("items", &items)]),
            &[
                ("divisions", ids.join(",")),
                ("prompt_version", SUMMARY_PROMPT_VERSION.to_string()),
            ],
            // An empty context is no context: the division stays pending.
            |text| parse_batch(text, &ids, "divisions", false),
        )
        .await
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
            telemetry: None,
            prompt_versions: HashMap::new(),
        }
    }

    /// The model's text, taken as it is.
    fn text(t: &str) -> Result<Parsed<String>> {
        Ok(Parsed::clean(t.to_string()))
    }

    const FIRST: Prompt = Prompt {
        name: "pollywiki.first",
        version: 1,
        template: "{{text}}",
    };
    const TEST: Prompt = Prompt {
        name: "pollywiki.test",
        version: 1,
        template: "{{text}}",
    };

    #[tokio::test]
    async fn each_call_is_traced_to_langfuse_and_a_failed_export_changes_nothing() {
        let server = TestServer::start(|req| {
            if req.path.contains(":generateContent") {
                return Response::json(
                    serde_json::json!({
                        "candidates": [{ "content": { "parts": [{ "text": "{\"note\":\"n\"}" }] } }],
                        "usageMetadata": { "promptTokenCount": 120, "candidatesTokenCount": 30 }
                    })
                    .to_string(),
                );
            }
            if req.path == "/langfuse/api/public/otel/v1/traces" {
                // The first export is refused, as an outage would be.
                if req.body.contains("pollywiki.first") {
                    return Response::status(400, "rejected");
                }
                return Response::json("{}");
            }
            Response::status(404, "unexpected path")
        });
        let mut traced = gemini(&server);
        traced.telemetry = Some(crate::telemetry::Langfuse::at(&format!(
            "{}/langfuse",
            server.base
        )));
        traced.prompt_versions.insert(MEMBER_NOTE.name, 6);

        let refused = traced
            .generate(&FIRST, "prompt one", &[], text)
            .await
            .expect("an export failure never fails the call");
        assert_eq!(refused, "{\"note\":\"n\"}");
        traced
            .generate(
                &MEMBER_NOTE,
                "prompt two",
                &[("person", "alex".to_string())],
                text,
            )
            .await
            .expect("call");

        let exports: Vec<_> = server
            .requests()
            .into_iter()
            .filter(|r| r.path.contains("/otel/"))
            .collect();
        assert_eq!(exports.len(), 2, "one span per call");
        let export = &exports[1];
        assert_eq!(export.method, "POST");
        assert!(export
            .header("authorization")
            .is_some_and(|a| a.starts_with("Basic ")));
        assert_eq!(export.header("x-langfuse-ingestion-version"), Some("4"));
        let body: Value = serde_json::from_str(&export.body).expect("OTLP JSON");
        let span = &body["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        assert_eq!(span["name"], "pollywiki.member-note");
        let attrs = span["attributes"].to_string();
        assert!(attrs.contains("prompt two"));
        assert!(
            attrs.contains(r#"{\"input\":120,\"output\":30}"#),
            "usage: {attrs}"
        );
        assert!(attrs.contains("langfuse.observation.metadata.person"));
        assert!(
            attrs.contains(r#""key":"langfuse.version","value":{"stringValue":"v4"}"#),
            "the prompt version is the span's version: {attrs}"
        );
        assert!(
            attrs.contains(
                r#""key":"langfuse.observation.prompt.version","value":{"intValue":"6"}"#
            ),
            "the generation links to the registered prompt: {attrs}"
        );

        // Without keys nothing is sent anywhere.
        let before = server.hits();
        gemini(&server)
            .generate(&MEMBER_NOTE, "prompt three", &[], text)
            .await
            .expect("call");
        assert_eq!(server.hits(), before + 1, "only the model call");
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

        // One more vote is not yet worth rewriting the note for: rewriting
        // after every sitting day churned every note's text.
        let mut eleventh = division("representatives/2026-08-12/11", 11, "Third reading", None);
        eleventh.votes = vec![vote("alex-paterson", false)];
        divisions.push(eleventh);
        person_notes(
            &store,
            &gemini(&server),
            std::slice::from_ref(&member),
            &divisions,
        )
        .await
        .expect("one more vote");
        assert_eq!(server.hits(), after_first, "a note is kept for small moves");

        // Ten more since the note was written, and it is rewritten.
        for n in 12..=20 {
            let mut d = division(
                &format!("representatives/2026-08-12/{n}"),
                n,
                "Second reading",
                None,
            );
            d.votes = vec![vote("alex-paterson", false)];
            divisions.push(d);
        }
        person_notes(
            &store,
            &gemini(&server),
            std::slice::from_ref(&member),
            &divisions,
        )
        .await
        .expect("regen");
        assert_eq!(
            server.hits(),
            after_first + 1,
            "a record that grew by ten divisions is rewritten"
        );
        let note: AiPersonNote = store
            .get_json(&note_key("alex-paterson"))
            .await
            .unwrap()
            .expect("note stored");
        assert_eq!((note.votes_considered, note.crossed_considered), (20, 1));

        // A vote against the grouping is news: rewritten at once.
        let mut crossed = division("representatives/2026-08-12/21", 21, "Censure motion", None);
        crossed.votes = vec![vote("alex-paterson", true)];
        divisions.push(crossed);
        person_notes(&store, &gemini(&server), &[member], &divisions)
            .await
            .expect("regen on crossing");
        assert_eq!(
            server.hits(),
            after_first + 2,
            "one more crossing rewrites the note"
        );
    }

    #[tokio::test]
    async fn a_bill_with_no_official_summary_gets_no_note_and_loses_an_old_one() {
        let server = TestServer::start(|req| {
            assert!(
                !req.body.contains("r3") && !req.body.contains("Untitled"),
                "a bill without a summary is never sent: {}",
                req.body
            );
            Response::json(candidate(
                &serde_json::json!([{ "id": "r2", "summary": "Note." }]).to_string(),
            ))
        });
        let store = new_store("nosummary");
        let dense = bill("r2", "Dense Bill 2026", Some("Standing appropriations."));
        let bare = bill("r3", "Untitled Summary Bill 2026", None);
        let blank = bill("r4", "Blank Summary Bill 2026", Some("   "));
        // A note written from the title alone, before the rule.
        store
            .put_json(
                &bill_note_key("r3"),
                &AiSummary {
                    text: "This bill proposes changes to untitled summaries.".to_string(),
                    model: "old-model".to_string(),
                    generated_at: "2026-10-01T00:00:00.000Z".to_string(),
                    prompt_version: Some(BILL_NOTE_PROMPT_VERSION),
                },
            )
            .await
            .unwrap();

        bill_notes(&store, &gemini(&server), &[&dense, &bare, &blank])
            .await
            .expect("bill notes");
        assert_eq!(server.hits(), 1);
        assert!(
            store
                .get_json::<AiSummary>(&bill_note_key("r3"))
                .await
                .unwrap()
                .is_none(),
            "the title-only note is removed"
        );
        assert!(store
            .get_json::<AiSummary>(&bill_note_key("r4"))
            .await
            .unwrap()
            .is_none());
        assert!(store
            .get_json::<AiSummary>(&bill_note_key("r2"))
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn prompts_are_mirrored_into_langfuse_and_generations_link_to_their_version() {
        let server = TestServer::start(|req| {
            if req.method == "POST" && req.path == "/api/public/v2/prompts" {
                let body: Value = serde_json::from_str(&req.body).expect("json");
                return Response::json(
                    serde_json::json!({ "name": body["name"], "version": 9 }).to_string(),
                );
            }
            if req.path.starts_with("/api/public/v2/prompts/") {
                return if req.path.contains(BILL_NOTES.name) {
                    // Already mirrored, text unchanged.
                    Response::json(
                        serde_json::json!({ "version": 3, "prompt": BILL_NOTES.template })
                            .to_string(),
                    )
                } else if req.path.contains(DIVISION_CONTEXT.name) {
                    // Mirrored once, but the template has changed since.
                    Response::json(
                        serde_json::json!({ "version": 1, "prompt": "an older template" })
                            .to_string(),
                    )
                } else {
                    Response::status(404, "{}")
                };
            }
            if req.path.contains(":generateContent") {
                return Response::json(candidate("[]"));
            }
            Response::json("{}")
        });
        let mut traced = gemini(&server);
        traced.telemetry = Some(crate::telemetry::Langfuse::at(&server.base));

        let registered = traced.with_prompts_registered().await;
        assert_eq!(
            registered.prompt_versions,
            HashMap::from([
                (BILL_NOTES.name, 3),
                (DIVISION_CONTEXT.name, 9),
                (MEMBER_NOTE.name, 9),
            ])
        );
        let posted: Vec<_> = server
            .requests()
            .into_iter()
            .filter(|r| r.method == "POST" && r.path == "/api/public/v2/prompts")
            .collect();
        assert_eq!(posted.len(), 2, "only the changed and the new are written");
        assert!(posted[0].body.contains(DIVISION_CONTEXT.name));
        assert!(posted[1].body.contains(MEMBER_NOTE.name));

        // Without tracing nothing is registered, and the flow runs unlinked.
        assert!(gemini(&server)
            .with_prompts_registered()
            .await
            .prompt_versions
            .is_empty());

        let first = bill("r1", "First Bill 2026", Some("Dense."));
        generate_bill_batch(&registered, &[&first])
            .await
            .expect("call");
        let export = server
            .requests()
            .into_iter()
            .rev()
            .find(|r| r.path.contains("/otel/"))
            .expect("the call was traced");
        assert!(
            export.body.contains(
                r#""key":"langfuse.observation.prompt.version","value":{"intValue":"3"}"#
            ),
            "{}",
            export.body
        );
        assert!(export.body.contains(r#""key":"langfuse.observation.prompt.name","value":{"stringValue":"pollywiki.bill-notes"}"#));
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
            g.generate(&TEST, "prompt", &[], text).await.is_err(),
            "no candidate is an error"
        );

        // A candidate with no text says why, which is the part worth having.
        let cut_off = TestServer::start(|_| {
            Response::json(
                serde_json::json!({
                    "candidates": [{ "finishReason": "MAX_TOKENS", "content": { "parts": [] } }]
                })
                .to_string(),
            )
        });
        let err = gemini(&cut_off)
            .generate(&TEST, "prompt", &[], text)
            .await
            .expect_err("no text is an error");
        assert_eq!(err.to_string(), "gemini: empty response (MAX_TOKENS)");

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
        g.generate(&TEST, "the prompt text", &[], text)
            .await
            .expect("generate");

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

    /// Ten members with ten votes each, so every one of them needs a note.
    fn ten_members_with_votes() -> (Vec<Person>, Vec<Division>) {
        let members: Vec<Person> = (1..=10)
            .map(|n| person(&format!("member-{n}"), &format!("Member {n}")))
            .collect();
        let divisions: Vec<Division> = (1..=10)
            .map(|n| {
                let mut d = division(
                    &format!("representatives/2026-08-12/{n}"),
                    n,
                    "Bills — Example Bill 2026; Second Reading",
                    None,
                );
                d.votes = members.iter().map(|m| vote(&m.slug, false)).collect();
                d
            })
            .collect();
        (members, divisions)
    }

    #[tokio::test]
    async fn scattered_failures_are_survived_but_five_in_a_row_abort() {
        let (members, divisions) = ten_members_with_votes();

        // Every second call fails, as an overloaded model does: five failures
        // in all, never two in a row, and every other member still gets a note.
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let flaky = TestServer::start(move |_| match counter.fetch_add(1, Ordering::SeqCst) % 2 {
            0 => Response::status(400, "overloaded"),
            _ => Response::json(candidate(
                &serde_json::json!({ "note": "Voted aye on the bill." }).to_string(),
            )),
        });
        let store = new_store("scattered");
        person_notes(&store, &gemini(&flaky), &members, &divisions)
            .await
            .expect("scattered failures are not fatal");
        assert_eq!(flaky.hits(), 10, "every member was tried");
        let mut written = 0;
        for member in &members {
            if store
                .get_json::<AiPersonNote>(&note_key(&member.slug))
                .await
                .unwrap()
                .is_some()
            {
                written += 1;
            }
        }
        assert_eq!(written, 5);

        let down = TestServer::start(|_| Response::status(400, "down"));
        let store = new_store("down");
        let err = person_notes(&store, &gemini(&down), &members, &divisions)
            .await
            .expect_err("five in a row abort");
        assert!(err.to_string().contains("in a row"), "{err}");
        assert_eq!(down.hits(), 5, "the sixth member is not tried");
    }

    #[tokio::test]
    async fn a_note_is_written_from_counts_and_kept_only_if_its_numbers_are_in_them() {
        let (members, divisions) = ten_members_with_votes();
        let member = members[0].clone();

        let server = TestServer::start(|req| {
            // The model sees the sums, never the vote list.
            assert!(
                req.body
                    .contains("- Example Bill 2026: Second Reading: 10 divisions: aye 10, no 0"),
                "prompt lost the counts: {}",
                req.body
            );
            assert!(
                !req.body.contains("| voted aye"),
                "the vote list is not the model's to add up: {}",
                req.body
            );
            Response::json(candidate(
                &serde_json::json!({
                    "note": "Voted aye in all 10 second reading divisions on Example Bill 2026."
                })
                .to_string(),
            ))
        });
        let store = new_store("counted");
        person_notes(
            &store,
            &gemini(&server),
            std::slice::from_ref(&member),
            &divisions,
        )
        .await
        .expect("note");
        let note: AiPersonNote = store
            .get_json(&note_key(&member.slug))
            .await
            .unwrap()
            .expect("every number in the note is in the record");
        assert!(note.text.starts_with("Voted aye in all 10"));

        // A number the record does not contain is a number the model made up.
        let invented = TestServer::start(|_| {
            Response::json(candidate(
                &serde_json::json!({ "note": "Voted aye in 12 divisions on the bill." })
                    .to_string(),
            ))
        });
        let store = new_store("invented");
        person_notes(
            &store,
            &gemini(&invented),
            std::slice::from_ref(&member),
            &divisions,
        )
        .await
        .expect("a rejected note is skipped, not fatal");
        assert!(
            store
                .get_json::<AiPersonNote>(&note_key(&member.slug))
                .await
                .unwrap()
                .is_none(),
            "a note citing 12 of 10 divisions is not kept"
        );
    }

    #[tokio::test]
    async fn an_unusable_reply_is_an_error_in_the_trace_and_a_short_one_a_warning() {
        let server = TestServer::start(|req| {
            if req.path.contains(":generateContent") {
                let answer = if req.body.contains("explain bills") {
                    // One of the two bills asked about is missing.
                    serde_json::json!([{ "id": "r1", "summary": "What it does." }]).to_string()
                } else {
                    "Sorry, I cannot help.".to_string()
                };
                return Response::json(candidate(&answer));
            }
            Response::json("{}")
        });
        let mut traced = gemini(&server);
        traced.telemetry = Some(crate::telemetry::Langfuse::at(&server.base));

        let err = traced
            .generate(&MEMBER_NOTE, "prompt", &[], |t| {
                let _: Value = serde_json::from_str(t)?;
                Ok(Parsed::clean(()))
            })
            .await
            .expect_err("prose is not the JSON asked for");
        assert!(err.to_string().contains("expected"), "{err}");

        let first = bill("r1", "First Bill 2026", Some("Dense."));
        let second = bill("r2", "Second Bill 2026", Some("Dense."));
        let notes = generate_bill_batch(&traced, &[&first, &second])
            .await
            .expect("a short batch is still used");
        assert_eq!(notes.len(), 1);

        let exports: Vec<_> = server
            .requests()
            .into_iter()
            .filter(|r| r.path.contains("/otel/"))
            .collect();
        assert_eq!(exports.len(), 2);
        let unusable = exports[0].body.as_str();
        assert!(unusable.contains(r#""stringValue":"ERROR""#), "{unusable}");
        assert!(unusable.contains("unusable reply: expected"), "{unusable}");
        assert!(
            unusable.contains("Sorry, I cannot help."),
            "the text that could not be used is recorded: {unusable}"
        );
        let short = exports[1].body.as_str();
        assert!(short.contains(r#""stringValue":"WARNING""#), "{short}");
        assert!(
            short.contains("1 of 2 bills missing from the reply: r2"),
            "{short}"
        );
    }
}
