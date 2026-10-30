//! Queensland. The Parliament's own site turns automated readers away; the
//! legislation register lists every bill as introduced, as passed where it
//! was amended on the way, and as the Act it became. Each bill also has a
//! history page there with what the lists leave out: whose bill it is, the
//! third reading of a bill passed as introduced, and where a bill went no
//! further.

use super::{BillRoll, RollBill};
use crate::endpoints::Endpoints;
use crate::http::{fetch_json, fetch_text};
use crate::js_url::encode_uri_component;
use crate::store::Store;
use anyhow::{anyhow, bail, Context, Result};
use chrono::NaiveDate;
use pollywiki_schema::{StateCode, TimelineStep};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

/// Every bill as introduced, asked for as the register's own Browse > Bills
/// page asks. A non-government introduction is a print type of its own.
const INTRODUCED: &str = r#"Repealed=N AND PrintType=("bill.first" OR "bill.firstnongovintro")"#;

/// Every bill as passed, and every Act. A bill is printed as passed only
/// when it was amended on the way; its Act shares its version series.
const PASSED: &str = r#"PrintType=("bill.third" OR "act.new")"#;

/// Records asked for in one call. A parliament's bills and Acts run to a few
/// hundred, so each list is one call; the bound on pages stops a filter the
/// register no longer honours from walking its whole catalogue.
const PAGE: usize = 500;
const MAX_PAGES: usize = 4;

/// Spacing between requests to the register.
const SPACING_MS: u64 = 2000;

/// Each bill's history page as last read, by the register's id for the bill.
const HISTORY_PREFIX: &str = "raw/states/qld/bills/";

/// The parliament's number, with the `since` it was found for.
const PARLIAMENT_KEY: &str = "raw/states/qld/bills-parliament.json";

pub async fn read(store: &Store, endpoints: &Endpoints, since: &str) -> Result<BillRoll> {
    let parliament = parliament_number(store, endpoints, since).await?;
    let introduced = list(
        endpoints,
        &format!("{INTRODUCED} AND ParliamentNo={parliament}"),
    )
    .await?;
    let later = list(
        endpoints,
        &format!("{PASSED} AND ParliamentNo={parliament}"),
    )
    .await?;

    let mut as_passed: HashMap<&str, &Record> = HashMap::new();
    let mut acts: HashMap<&str, &Record> = HashMap::new();
    for record in later.iter().filter(|r| !r.series.is_empty()) {
        let by_series = match record.print_type.as_str() {
            "bill.third" => &mut as_passed,
            "act.new" => &mut acts,
            _ => continue,
        };
        by_series.entry(record.series.as_str()).or_insert(record);
    }

    let mut tally = Tally::default();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut bills = Vec::new();
    for bill in &introduced {
        // The register applies the filter; a record from another parliament
        // that slips through it, or one repeated across pages, is left out.
        let elsewhere = !bill.parliament.is_empty() && bill.parliament != parliament;
        if !bill.print_type.starts_with("bill.first") || elsewhere || !seen.insert(&bill.id) {
            continue;
        }
        let act = acts.get(bill.series.as_str()).copied();
        let history = history(store, endpoints, bill, act.is_some(), &mut tally).await?;
        bills.push(roll_bill(
            endpoints,
            bill,
            as_passed.get(bill.series.as_str()).copied(),
            act,
            history.as_ref(),
        ));
    }
    println!(
        "qld-bills: Parliament {parliament}, {} history pages read, {} settled ones kept, {} unreadable",
        tally.read, tally.kept, tally.failed
    );
    Ok(BillRoll {
        state: StateCode::QLD,
        listed_at: crate::now_iso(),
        publisher: "Queensland Legislation".to_string(),
        source: format!("{}/browse/bills", endpoints.qld_legislation),
        bills,
    })
}

/// The parliament's number, which both lists are filtered by. It is not
/// written down here, so a new parliament needs only a new `since`. The
/// register's own by-year browse, without its parliament term, lists the
/// bills introduced in `since`'s year, and those introduced on or after
/// `since` carry the number of the parliament elected that day; the next
/// year is asked when none was introduced in the year of the election. The
/// lists are filtered by the number rather than by dates because the
/// register files every print under its parliament, and an Act assented
/// after an election can still be the last parliament's. It is found once
/// for each `since` and kept, so a night is the two list calls.
async fn parliament_number(store: &Store, endpoints: &Endpoints, since: &str) -> Result<String> {
    let known = store
        .get_json::<Parliament>(PARLIAMENT_KEY)
        .await
        .ok()
        .flatten();
    if let Some(known) = known.filter(|k| k.since == since) {
        return Ok(known.number);
    }
    let first: i32 = since
        .get(..4)
        .and_then(|year| year.parse().ok())
        .ok_or_else(|| anyhow!("qld-bills: since {since} is not a date"))?;
    let mut unnumbered = false;
    // A parliament sits four years at most, and a year with no bills at all
    // has not come yet.
    for year in first..=first + 4 {
        let bills = list(endpoints, &format!("{INTRODUCED} AND Year={year}?")).await?;
        if bills.is_empty() {
            break;
        }
        let since_then: Vec<&Record> = bills
            .iter()
            .filter(|bill| bill.published.as_deref().is_some_and(|day| day >= since))
            .collect();
        let number = since_then
            .iter()
            .filter_map(|bill| bill.parliament.parse::<u32>().ok())
            .max();
        if let Some(number) = number {
            let number = number.to_string();
            let found = Parliament {
                since: since.to_string(),
                number: number.clone(),
            };
            store.put_json(PARLIAMENT_KEY, &found).await?;
            return Ok(number);
        }
        unnumbered |= !since_then.is_empty();
    }
    if unnumbered {
        bail!("qld-bills: the bills introduced since {since} carry no parliament number");
    }
    bail!("qld-bills: the register lists no bill introduced since {since}")
}

/// The parliament's number as found, against the `since` it was found for.
#[derive(Debug, Serialize, Deserialize)]
struct Parliament {
    since: String,
    number: String,
}

/// One of the register's browse lists: the call its Browse > Bills page
/// makes, sorted the same way, every record's wrappers taken off.
async fn list(endpoints: &Endpoints, expression: &str) -> Result<Vec<Record>> {
    let mut records = Vec::new();
    let mut seen = 0;
    for page in 0..MAX_PAGES {
        let url = format!(
            "{}/projectdata?ds=OQPC-BrowseDataSource&subset=browse&start={}&count={PAGE}&sortField=sort.title&sortDirection=asc&expression={}",
            endpoints.qld_legislation,
            page * PAGE + 1,
            query_value(expression),
        );
        let body: Value = fetch_json(&url, &endpoints.opts(SPACING_MS))
            .await
            .with_context(|| format!("qld-bills: listing {expression}"))?;
        let body = unwrapped(body);
        let data = body
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("qld-bills: no data in the list for {expression}"))?;
        seen += data.len();
        records.extend(data.iter().filter_map(Record::read));
        let total = body.get("totalCount").and_then(Value::as_u64).unwrap_or(0);
        if data.len() < PAGE || seen as u64 >= total {
            return Ok(records);
        }
    }
    bail!(
        "qld-bills: more than {} records for {expression}",
        PAGE * MAX_PAGES
    )
}

/// A query value as the register's own page sends it, by jQuery's rules:
/// percent-encoded, with each space a plus sign.
fn query_value(value: &str) -> String {
    encode_uri_component(value).replace("%20", "+")
}

/// The register wraps each value with its type, {"__type__": "UniString",
/// "__value__": "58"}, and leaves a few bare. This is the one place the
/// wrappers come off, at any depth, so everything after reads plain JSON.
fn unwrapped(value: Value) -> Value {
    match value {
        Value::Object(mut map) => match map.remove("__value__") {
            Some(inner) => unwrapped(inner),
            None => Value::Object(map.into_iter().map(|(k, v)| (k, unwrapped(v))).collect()),
        },
        Value::Array(items) => Value::Array(items.into_iter().map(unwrapped).collect()),
        bare => bare,
    }
}

/// One record from a list, read once its wrappers are off.
#[derive(Debug)]
struct Record {
    /// The register's id: "bill-2026-024", "act-2026-026".
    id: String,
    title: String,
    /// The register shows a private member's bill's title with an asterisk
    /// after it, as its Browse > Bills page explains.
    starred: bool,
    year: String,
    no: String,
    parliament: String,
    print_type: String,
    /// The day it was published, YYYY-MM-DD: a bill's introduction, an
    /// Act's assent.
    published: Option<String>,
    /// Shared by a bill's prints and the Act it became.
    series: String,
    /// What was filed with it: "bill.first.exp" is its explanatory note.
    docs: Vec<String>,
}

impl Record {
    fn read(value: &Value) -> Option<Record> {
        let text = |key: &str| match value.get(key) {
            Some(Value::String(text)) => text.trim().to_string(),
            Some(Value::Number(number)) => number.to_string(),
            _ => String::new(),
        };
        let id = text("id");
        let shown = text("title_v");
        let title = Some(text("title"))
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| shown.clone());
        let title = title.trim_end_matches('*').trim().to_string();
        if id.is_empty() || title.is_empty() {
            return None;
        }
        // A list of one comes as the one value.
        let docs = match value.get("associated.docs") {
            Some(Value::String(doc)) => vec![doc.trim().to_string()],
            Some(Value::Array(docs)) => docs
                .iter()
                .filter_map(Value::as_str)
                .map(|doc| doc.trim().to_string())
                .collect(),
            _ => Vec::new(),
        };
        Some(Record {
            starred: shown.ends_with('*'),
            year: text("year"),
            no: text("no"),
            parliament: text("parliament.no"),
            print_type: text("print.type"),
            published: iso_date(&text("publication.date")),
            series: text("version.series.id"),
            docs,
            id,
            title,
        })
    }
}

/// What a bill's history page says, as kept between nights.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct History {
    /// "Government" or "Private member's".
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    introduced: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    third_reading: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assent: Option<String>,
    /// The Act it became, as the page numbers it: "Act 2026, No. 26".
    #[serde(skip_serializing_if = "Option::is_none")]
    act: Option<String>,
    /// Where it went no further, in the page's words: "Discharged".
    #[serde(skip_serializing_if = "Option::is_none")]
    end: Option<TimelineStep>,
    /// Read once the bill had settled, as an Act or at an end, so its page
    /// will not change again.
    #[serde(default)]
    settled: bool,
}

/// What a night's history pages came to, for its log line.
#[derive(Default)]
struct Tally {
    read: usize,
    kept: usize,
    failed: usize,
}

/// A bill's history page, as read. A settled bill keeps the copy read once
/// it had settled, as an Act or at an end, since its page will not change
/// again. Any other bill's page is read each night: the third reading of a
/// bill passed as introduced, or a discharge, shows nowhere else. So a bill
/// whose page was last read before its Act was listed is read once more,
/// for a third reading it may not have had then. A page that cannot be read
/// leaves the copy from before where there is one, and the lists alone
/// where not.
async fn history(
    store: &Store,
    endpoints: &Endpoints,
    bill: &Record,
    has_act: bool,
    tally: &mut Tally,
) -> Result<Option<History>> {
    let key = format!("{HISTORY_PREFIX}{}.json", bill.id);
    // A copy this reader can no longer parse is as good as none.
    let kept = match store.get_raw(&key).await? {
        Some(raw) => serde_json::from_str::<History>(&raw).ok(),
        None => None,
    };
    if kept.as_ref().is_some_and(|history| history.settled) {
        tally.kept += 1;
        return Ok(kept);
    }
    let fresh = fetch_text(&history_url(endpoints, bill), &endpoints.opts(SPACING_MS))
        .await
        .and_then(|page| parse_history(&page));
    match fresh {
        Ok(mut fresh) => {
            fresh.settled = has_act || fresh.assent.is_some() || fresh.end.is_some();
            store.put_json(&key, &fresh).await?;
            tally.read += 1;
            Ok(Some(fresh))
        }
        Err(err) => {
            eprintln!("qld-bills: no history read for {} - {err:#}", bill.id);
            tally.failed += 1;
            Ok(kept)
        }
    }
}

/// A bill's history page on the register: what the roll links it to, and
/// what is read for it.
fn history_url(endpoints: &Endpoints, bill: &Record) -> String {
    format!(
        "{}/view/html/{}/{}/lh",
        endpoints.qld_legislation, bill.print_type, bill.id
    )
}

/// The history table follows an anchor of its own: an assented bill's page
/// leads with the Act's reprint table.
static HISTORY_TABLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)id="creationhistory".*?<table[^>]*>(.*?)</table>"#).unwrap()
});

static ROW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<tr[^>]*>(.*?)</tr>").unwrap());

/// A row's first cell: a step and its day, "Assent: 22 October 2026", or
/// whose bill it is. An empty cell after it is closed with a stray `</t>`,
/// so only the first is taken whole.
static FIRST_CELL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<td[^>]*>(.*?)</td>").unwrap());

/// The Act as the assent row links it: "Act 2026, No. 26".
static ACT_LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Act\s+(\d{4}),\s*No\.\s*(\d+)").unwrap());

/// The words a history page ends a bill with.
const ENDS: [&str; 4] = ["discharged", "withdrawn", "negatived", "lapsed"];

/// A history page's table, row by row: "Government Bill" or "Private
/// Member's Bill", then the steps with their days. Steps the roll does not
/// carry, such as an indicative reprint, are passed over. A page without the
/// table, such as one a challenge stood in for, is an error, so it is never
/// kept in place of the real one.
fn parse_history(page: &str) -> Result<History> {
    let table = HISTORY_TABLE
        .captures(page)
        .ok_or_else(|| anyhow!("no history table on the page"))?;
    let mut history = History::default();
    for row in ROW.captures_iter(&table[1]) {
        let Some(cell) = FIRST_CELL.captures(&row[1]) else {
            continue;
        };
        let text = cell_text(&cell[1]);
        let Some((label, day)) = text.split_once(':') else {
            let lower = text.to_lowercase();
            if lower.contains("private member") || lower.contains("non-government") {
                history.kind = Some("Private member's".to_string());
            } else if lower.contains("government") {
                history.kind = Some("Government".to_string());
            }
            continue;
        };
        let Some(date) = long_date(day) else {
            continue;
        };
        let step = plain_words(label);
        match step.to_lowercase().as_str() {
            "introduced" => {
                history.introduced.get_or_insert(date);
            }
            "third reading" => {
                history.third_reading.get_or_insert(date);
            }
            "assent" | "assented" => {
                history.assent.get_or_insert(date);
                if history.act.is_none() {
                    history.act = ACT_LINK
                        .captures(&cell_text(&row[1]))
                        .map(|act| format!("Act {}, No. {}", &act[1], &act[2]));
                }
            }
            lower if history.end.is_none() && ENDS.iter().any(|end| lower.contains(end)) => {
                history.end = Some(TimelineStep { date, event: step });
            }
            _ => {}
        }
    }
    if history.kind.is_none() && history.introduced.is_none() {
        bail!("the history table names neither the bill's kind nor its introduction");
    }
    Ok(history)
}

/// The text of a cell: tags gone, the register's entities decoded, and runs
/// of space and line breaks as one space.
fn cell_text(fragment: &str) -> String {
    static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]*>").unwrap());
    let text = TAG
        .replace_all(fragment, " ")
        .replace("&#160;", " ")
        .replace("&nbsp;", " ")
        .replace("&#39;", "'")
        .replace("&#8217;", "\u{2019}")
        .replace("&rsquo;", "\u{2019}")
        .replace("&#38;", "&")
        .replace("&amp;", "&");
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A history page's label in plain words: "3rd Reading" is "Third reading".
fn plain_words(label: &str) -> String {
    let words: Vec<String> = label
        .split_whitespace()
        .map(|word| match word.to_lowercase().as_str() {
            "1st" => "first".to_string(),
            "2nd" => "second".to_string(),
            "3rd" => "third".to_string(),
            lower => lower.to_string(),
        })
        .collect();
    let words = words.join(" ");
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// "26 August 2026", as a history page dates its steps, as 2026-08-26.
fn long_date(text: &str) -> Option<String> {
    NaiveDate::parse_from_str(text.trim(), "%d %B %Y")
        .ok()
        .map(|day| day.format("%Y-%m-%d").to_string())
}

/// "2026-08-26T00:00:00", as the lists date a print, as the day alone.
fn iso_date(text: &str) -> Option<String> {
    let day = text.get(..10)?;
    NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    Some(day.to_string())
}

/// One bill on the roll, from its records in the two lists and its history
/// page. The page dates the steps themselves, so its days stand over a
/// print's publication day where both give one.
fn roll_bill(
    endpoints: &Endpoints,
    bill: &Record,
    as_passed: Option<&Record>,
    act: Option<&Record>,
    history: Option<&History>,
) -> RollBill {
    let page = history.cloned().unwrap_or_default();
    let introduced = page.introduced.or_else(|| bill.published.clone());
    let third_reading = page
        .third_reading
        .or_else(|| as_passed.and_then(|print| print.published.clone()));
    let assent = page
        .assent
        .or_else(|| act.and_then(|act| act.published.clone()));
    let passed = third_reading.is_some() || as_passed.is_some();
    let assented = assent.is_some() || act.is_some();
    let (status, stage, stopped) = match &page.end {
        _ if assented => ("Assented".to_string(), 4, false),
        Some(end) => (end.event.clone(), if passed { 2 } else { 1 }, true),
        None if passed => ("Passed".to_string(), 2, false),
        None => ("Before the Assembly".to_string(), 1, false),
    };
    let mut timeline: Vec<TimelineStep> = [
        (introduced, "Introduced"),
        (third_reading, "Third reading"),
        (assent, "Assent"),
    ]
    .into_iter()
    .filter_map(|(date, event)| {
        Some(TimelineStep {
            date: date?,
            event: event.to_string(),
        })
    })
    .chain(page.end)
    .collect();
    // Oldest first; steps taken the same day keep the order they came in.
    timeline.sort_by(|a, b| a.date.cmp(&b.date));
    let pdf = |print: &str| format!("{}/view/pdf/{print}/{}", endpoints.qld_legislation, bill.id);
    RollBill {
        title: bill.title.clone(),
        number: drafting_number(&bill.id),
        origin: Some("Legislative Assembly".to_string()),
        sponsor: None,
        kind: Some(page.kind.unwrap_or_else(|| listed_kind(bill).to_string())),
        status,
        stage,
        stopped,
        timeline,
        source: history_url(endpoints, bill),
        text: Some(pdf(&bill.print_type)),
        explanatory: bill
            .docs
            .iter()
            .find(|doc| doc.ends_with(".exp"))
            .map(|doc| pdf(doc.as_str())),
        act: act.and_then(act_name).or(page.act),
    }
}

/// Whose bill it is by the list alone: a non-government print, or the
/// asterisk the register puts after a private member's bill's title. The
/// history page says it in words, and is read first.
fn listed_kind(bill: &Record) -> &'static str {
    if bill.print_type == "bill.firstnongovintro" || bill.starred {
        "Private member's"
    } else {
        "Government"
    }
}

/// The Act a bill became, numbered as the register numbers it: "Act 2026,
/// No. 26".
fn act_name(act: &Record) -> Option<String> {
    let no: u32 = act.no.parse().ok()?;
    let year: u32 = act.year.parse().ok().filter(|year| *year >= 1000)?;
    Some(format!("Act {year}, No. {no}"))
}

/// The drafting office's number for a bill, from the register's id for it:
/// bill-2026-024 is Bill No. 24 of 2026. The office numbers bills in yearly
/// series that a bill can carry into the next year, as the Transport and
/// Other Legislation Amendment Bill 2026 is bill-2025-020; the list's own
/// year is the title's, and would give it the number of bill-2026-020, the
/// Youth Justice (Circuit Breaker) Amendment Bill 2026. A zero is no number.
fn drafting_number(id: &str) -> Option<String> {
    static ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^bill-(\d{4})-(\d+)$").unwrap());
    let parts = ID.captures(id)?;
    let no: u32 = parts[2].parse().ok().filter(|no| *no > 0)?;
    Some(format!("Bill No. {no} of {}", &parts[1]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::LocalStore;
    use crate::test_http::{Request, Response, TestServer};
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// The history part of the register's page for the Queensland Protection
    /// Commission Bill 2026 as it was served, with most of its links cut. The
    /// Act's reprint table comes first.
    const PROTECTION_COMMISSION: &str = r##"<hr/><div id="lhview"><a name="0"></a><div id="parsewrapper"><div class="content">
<h1 class="title">Queensland Protection Commission Act 2026</h1>
<div id="reprintlist" class="collapse">
<div class="lhtable">
<table class="table table-condensed table-reprints">
<thead>
<tr>
<th>Effective Date</th>
<th>Publication Date</th>
<th>Amendments Included</th>
<th>Notes</th>
</tr>
</thead>
<tr>
<td><a href="/view/html/2026-10-22/act-2026-026">22 October
2026</a></td>
<td>22 October 2026</td>
<td></t>
<td>
<p>provs comm, RA ss 40, 42A</p>
</td>
</tr>
</table>
</div>
</div>
<a name="creationhistory" id="creationhistory"></a>
<h1 class="title">Queensland Protection Commission Bill 2026</h1>
<div class="lhtable">
<table class="table table-striped">
<tr>
<td>Government Bill</td>
<td></t>
</tr>
<tr>
<td>Introduced: 26 August 2026</td>
<td><img src="/images/pdficon_small.png" alt="PDF icon"/> <a href="/view/pdf/bill.first/bill-2026-024">Bill (Introduction)</a>
<p/>
<img src="/images/pdficon_small.png" alt="PDF icon"/> <a href="/view/pdf/bill.first.exp/bill-2026-024">Explanatory Note</a>
<p/>
</td>
</tr>
<tr>
<td>Indicative Reprint: 04 September 2026</td>
<td><img src="/images/pdficon_small.png" alt="PDF icon"/> <a href="/view/pdf/speciallabel/bill-2026-024/act-2024-049">Child Safe
Organisations Act 2024 (as amended by the Queensland Protection
Commission Bill 2026)</a>
<p/>
</td>
</tr>
<tr>
<td>3rd Reading: 15 October 2026</td>
<td><img src="/images/pdficon_small.png" alt="PDF icon"/> <a href="/view/pdf/bill.third/bill-2026-024">Bill (3rd Reading)</a>
<p/>
</td>
</tr>
<tr>
<td>Assent: 22 October 2026</td>
<td><img src="/images/pdficon_small.png" alt="PDF icon"/> <a href="/view/pdf/asmade/act-2026-026">Act 2026, No. 26</a>
<p/>
<img src="/images/html.gif" alt="HTML icon"/> <a href="/view/html/asmade/act-2026-026">Act 2026, No. 26</a>
<p/>
</td>
</tr>
</table>
</div>
</div>"##;

    /// The same for a private member's bill that was discharged.
    const FIFTY_CENT_FARES: &str = r##"<hr/><div id="lhview"><a name="0"></a><div id="parsewrapper"><div class="content">
<h1 class="title">Locking in Cost of Living Support (50 Cent Fares
Forever) Amendment Bill 2025*</h1>
<a name="creationhistory" id="creationhistory"></a>
<h1 class="title">Locking in Cost of Living Support (50 Cent Fares
Forever) Amendment Bill 2025*</h1>
<div class="lhtable">
<table class="table table-striped">
<tr>
<td>Private Member's Bill</td>
<td></t>
</tr>
<tr>
<td>Introduced: 19 February 2025</td>
<td><img src="/images/pdficon_small.png" alt="PDF icon"/> <a href="/view/pdf/bill.first/bill-2025-002">Bill (Introduction)</a>
<p/>
</td>
</tr>
<tr>
<td>Discharged: 14 March 2025</td>
<td></t>
</tr>
</table>
</div>
</div>"##;

    const SINCE: &str = "2024-10-26";
    const PROTECTION: &str = "Queensland Protection Commission Bill 2026";
    const TRANSPORT: &str = "Transport and Other Legislation Amendment Bill 2026";
    const FARES: &str =
        "Locking in Cost of Living Support (50 Cent Fares Forever) Amendment Bill 2025";
    const BAIL: &str = "Breach Bail, Go to Jail and Other Legislation Amendment Bill 2026";
    const STRANGULATION: &str =
        "Criminal Code (Non-fatal Strangulation) and Other Legislation Amendment Bill 2026";
    const NON_GOVERNMENT: &str = "Example Non-government Introduction Bill 2026";

    fn new_store(name: &str) -> Store {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/states-tests/qld-bills")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Store::Local(LocalStore::new(dir))
    }

    fn wrapped(value: &str) -> Value {
        json!({ "__type__": "UniString", "__value__": value })
    }

    /// A record as the register sends it: every value wrapped but the day,
    /// its number and year those of its id, in the 58th Parliament.
    fn print(id: &str, print_type: &str, title: &str, series: &str, published: &str) -> Value {
        let mut parts = id.rsplitn(3, '-');
        let no = parts.next().unwrap().trim_start_matches('0');
        let year = parts.next().unwrap();
        json!({
            "version.series.id": wrapped(series),
            "parliament.no": wrapped("58"),
            "type": wrapped("act"),
            "id": wrapped(id),
            "publication.date": format!("{published}T00:00:00"),
            "title": wrapped(title),
            "title_v": wrapped(title),
            "year": wrapped(year),
            "no": wrapped(no),
            "print.type": wrapped(print_type),
            "repealed": wrapped("N"),
        })
    }

    /// The asterisk the register shows after a private member's bill's title.
    fn starred(mut record: Value) -> Value {
        let title = record["title"]["__value__"].as_str().unwrap().to_string();
        record["title_v"] = wrapped(&format!("{title}*"));
        record
    }

    fn listing(records: Vec<Value>) -> String {
        let count = json!({ "__type__": "Integer", "__value__": records.len() });
        json!({ "data": records, "totalCount": count, "filteredCount": count }).to_string()
    }

    /// A history page marked up as the register marks it: a reprint table
    /// first, then the anchor, the title and the table, its kind row closed
    /// with a stray `</t>`.
    fn history_page(title: &str, kind: &str, rows: &[(&str, &str)]) -> String {
        let mut page = String::from(
            r#"<div id="lhview"><div class="lhtable"><table class="table table-reprints"><tr><td>Introduced: 1 January 2000</td><td></t></tr></table></div>"#,
        );
        page.push_str(&format!(
            r#"<a name="creationhistory" id="creationhistory"></a><h1 class="title">{title}</h1><div class="lhtable"><table class="table table-striped"><tr>
<td>{kind}</td>
<td></t>
</tr>"#
        ));
        for (step, links) in rows {
            page.push_str(&format!("<tr>\n<td>{step}</td>\n<td>{links}</td>\n</tr>\n"));
        }
        page.push_str("</table></div></div>");
        page
    }

    /// The expression a list call asked for, decoded.
    fn expression(req: &Request) -> String {
        let raw = req
            .query("expression")
            .unwrap_or_default()
            .replace('+', " ");
        percent_encoding::percent_decode_str(&raw)
            .decode_utf8_lossy()
            .into_owned()
    }

    fn step(date: &str, event: &str) -> TimelineStep {
        TimelineStep {
            date: date.to_string(),
            event: event.to_string(),
        }
    }

    fn bill<'a>(roll: &'a BillRoll, title: &str) -> &'a RollBill {
        roll.bills
            .iter()
            .find(|bill| bill.title == title)
            .unwrap_or_else(|| panic!("no {title} on the roll"))
    }

    /// The history pages a run of requests asked for, by print and id.
    fn pages_read(requests: &[Request]) -> Vec<String> {
        requests
            .iter()
            .filter_map(|req| {
                req.path
                    .strip_prefix("/qld-legislation/view/html/")?
                    .strip_suffix("/lh")
                    .map(str::to_string)
            })
            .collect()
    }

    fn lists_asked(requests: &[Request]) -> Vec<String> {
        requests
            .iter()
            .filter(|req| req.path.starts_with("/qld-legislation/projectdata?"))
            .map(expression)
            .collect()
    }

    /// The bills introduced in 2024: the last parliament's before the
    /// election, the new one's after.
    fn introduced_2024() -> Vec<Value> {
        let mut before = print(
            "bill-2024-030",
            "bill.first",
            "Last Parliament's Bill 2024",
            "s-030",
            "2024-08-20",
        );
        before["parliament.no"] = wrapped("57");
        let after = print(
            "bill-2024-042",
            "bill.first",
            "Brisbane Olympic and Paralympic Games Arrangements Bill 2024",
            "s-042",
            "2024-11-28",
        );
        vec![before, after]
    }

    fn introduced() -> Vec<Value> {
        let mut protection = print(
            "bill-2026-024",
            "bill.first",
            PROTECTION,
            "s-024",
            "2026-08-26",
        );
        protection["associated.docs"] = json!(["bill.first.exp", "bill.first.hrc"]);
        // Filed in 2025's series under a 2026 title: the list's year is the
        // title's.
        let mut transport = print(
            "bill-2025-020",
            "bill.first",
            TRANSPORT,
            "s-020",
            "2026-06-26",
        );
        transport["year"] = wrapped("2026");
        transport["associated.docs"] = wrapped("bill.first.exp");
        let mut fares = starred(print(
            "bill-2025-002",
            "bill.first",
            FARES,
            "s-002",
            "2025-02-19",
        ));
        fares["associated.docs"] = json!(["bill.first.hrc"]);
        let bail = print("bill-2026-036", "bill.first", BAIL, "s-036", "2026-08-27");
        let strangulation = starred(print(
            "bill-2026-050",
            "bill.first",
            STRANGULATION,
            "s-050",
            "2026-10-28",
        ));
        let mut non_government = print(
            "bill-2026-060",
            "bill.firstnongovintro",
            NON_GOVERNMENT,
            "s-060",
            "2026-09-01",
        );
        non_government["associated.docs"] = wrapped("bill.firstnongovintro.exp");
        let mut stray = print(
            "bill-2024-031",
            "bill.first",
            "Last Parliament's Other Bill 2024",
            "s-031",
            "2024-05-01",
        );
        stray["parliament.no"] = wrapped("57");
        vec![
            protection,
            transport,
            fares,
            bail,
            strangulation,
            non_government,
            stray,
        ]
    }

    /// The prints as passed and the Acts; from the third night, the bail
    /// bill's Act too.
    fn passed(night: usize) -> Vec<Value> {
        let mut protection = print(
            "bill-2026-024",
            "bill.third",
            PROTECTION,
            "s-024",
            "2026-10-15",
        );
        protection["associated.docs"] = json!(["bill.third.exp", "bill.third.hrc"]);
        let mut records = vec![
            protection,
            print(
                "act-2026-026",
                "act.new",
                "Queensland Protection Commission Act 2026",
                "s-024",
                "2026-10-22",
            ),
            print(
                "act-2026-023",
                "act.new",
                "Transport and Other Legislation Amendment Act 2026",
                "s-020",
                "2026-09-21",
            ),
            print(
                "act-2025-022",
                "act.new",
                "Coroners (Mining and Resources Coroner) Amendment Act 2025",
                "s-coroners",
                "2025-10-20",
            ),
        ];
        if night >= 2 {
            records.push(print(
                "act-2026-030",
                "act.new",
                "Breach Bail, Go to Jail and Other Legislation Amendment Act 2026",
                "s-036",
                "2026-11-05",
            ));
        }
        records
    }

    /// History pages by print and id; the bail bill is read a third time on
    /// the second night and assented on the third. The strangulation bill's
    /// page and the non-government one's are never there.
    fn page_for(print: &str, night: usize) -> Option<String> {
        let page = match print {
            "bill.first/bill-2026-024" => PROTECTION_COMMISSION.to_string(),
            "bill.first/bill-2025-002" => FIFTY_CENT_FARES.to_string(),
            "bill.first/bill-2025-020" => history_page(
                TRANSPORT,
                "Government Bill",
                &[
                    ("Introduced: 26 June 2026", ""),
                    ("3rd Reading: 17 September 2026", ""),
                    (
                        "Assent: 21 September 2026",
                        r#"<a href="/view/pdf/asmade/act-2026-023">Act 2026, No. 23</a>"#,
                    ),
                ],
            ),
            "bill.first/bill-2026-036" => {
                let mut rows = vec![("Introduced: 27 August 2026", "")];
                if night >= 1 {
                    rows.push(("3rd Reading: 28 October 2026", ""));
                }
                if night >= 2 {
                    rows.push((
                        "Assent: 5 November 2026",
                        r#"<a href="/view/pdf/asmade/act-2026-030">Act 2026, No. 30</a>"#,
                    ));
                }
                history_page(BAIL, "Government Bill", &rows)
            }
            _ => return None,
        };
        Some(page)
    }

    /// The register as each night finds it, answering only the calls this
    /// reader should make.
    fn register(night: Arc<AtomicUsize>) -> TestServer {
        TestServer::start(move |req| {
            let night = night.load(Ordering::SeqCst);
            let path = req.path.split('?').next().unwrap_or_default();
            if path == "/qld-legislation/projectdata" {
                let records = match expression(req).as_str() {
                    r#"Repealed=N AND PrintType=("bill.first" OR "bill.firstnongovintro") AND Year=2024?"# => {
                        introduced_2024()
                    }
                    r#"Repealed=N AND PrintType=("bill.first" OR "bill.firstnongovintro") AND ParliamentNo=58"# => {
                        introduced()
                    }
                    r#"PrintType=("bill.third" OR "act.new") AND ParliamentNo=58"# => passed(night),
                    _ => return Response::status(400, "unexpected expression"),
                };
                return Response::json(listing(records));
            }
            let print = path
                .strip_prefix("/qld-legislation/view/html/")
                .and_then(|rest| rest.strip_suffix("/lh"));
            match print.and_then(|print| page_for(print, night)) {
                Some(page) => Response::html(page),
                None => Response::status(404, "no such page"),
            }
        })
    }

    async fn first_night(name: &str) -> (TestServer, BillRoll) {
        let server = register(Arc::new(AtomicUsize::new(0)));
        let roll = read(&new_store(name), &Endpoints::at(&server.base), SINCE)
            .await
            .expect("read");
        (server, roll)
    }

    #[test]
    fn the_register_s_wrappers_come_off_at_any_depth() {
        let raw = json!({
            "id": wrapped("bill-2026-024"),
            "publication.date": "2026-08-26T00:00:00",
            "otherRenditions": {
                "filename": "B26_0024_Queensland_Protection_Commission_Bill_2026.pdf",
                "filesize": { "__type__": "Integer", "__value__": 538702 }
            },
            "indicative.reprints": [wrapped("act-1980-035"), wrapped("act-1992-044")],
            "associated.docs": wrapped("bill.first.exp"),
        });
        assert_eq!(
            unwrapped(raw.clone()),
            json!({
                "id": "bill-2026-024",
                "publication.date": "2026-08-26T00:00:00",
                "otherRenditions": {
                    "filename": "B26_0024_Queensland_Protection_Commission_Bill_2026.pdf",
                    "filesize": 538702
                },
                "indicative.reprints": ["act-1980-035", "act-1992-044"],
                "associated.docs": "bill.first.exp",
            })
        );

        let mut raw = raw;
        raw["title"] = wrapped(PROTECTION);
        let record = Record::read(&unwrapped(raw)).expect("a record");
        assert_eq!(record.id, "bill-2026-024");
        assert_eq!(record.published.as_deref(), Some("2026-08-26"));
        assert_eq!(
            record.docs,
            ["bill.first.exp"],
            "a list of one is one value"
        );
        assert!(!record.starred);
        assert!(
            Record::read(&json!({ "title": "No Id Bill 2026" })).is_none(),
            "a record with no id is passed over"
        );
    }

    #[test]
    fn the_expression_goes_as_the_register_s_own_page_sends_it() {
        assert_eq!(
            query_value(&format!("{INTRODUCED} AND ParliamentNo=58")),
            "Repealed%3DN+AND+PrintType%3D(%22bill.first%22+OR+%22bill.firstnongovintro%22)+AND+ParliamentNo%3D58"
        );
    }

    #[test]
    fn a_history_page_gives_the_kind_the_steps_and_the_act() {
        assert_eq!(
            parse_history(PROTECTION_COMMISSION).expect("history"),
            History {
                kind: Some("Government".to_string()),
                introduced: Some("2026-08-26".to_string()),
                third_reading: Some("2026-10-15".to_string()),
                assent: Some("2026-10-22".to_string()),
                act: Some("Act 2026, No. 26".to_string()),
                end: None,
                settled: false,
            },
            "the reprint table before the anchor and the indicative reprint are passed over"
        );
        assert_eq!(
            parse_history(FIFTY_CENT_FARES).expect("history"),
            History {
                kind: Some("Private member's".to_string()),
                introduced: Some("2025-02-19".to_string()),
                end: Some(step("2025-03-14", "Discharged")),
                ..History::default()
            }
        );
    }

    #[test]
    fn a_page_without_the_history_table_is_refused() {
        let challenge =
            "<html><body><p>Request unsuccessful. Incapsula incident ID: 1</p></body></html>";
        assert!(parse_history(challenge).is_err());
        let empty = r#"<a id="creationhistory"></a><table><tr><td>Notes</td></tr></table>"#;
        assert!(
            parse_history(empty).is_err(),
            "a table naming no kind and no introduction is not taken for one"
        );
    }

    #[test]
    fn labels_and_days_read_in_plain_words() {
        assert_eq!(plain_words("3rd Reading"), "Third reading");
        assert_eq!(plain_words("Discharged"), "Discharged");
        assert_eq!(
            plain_words("2nd Reading Negatived"),
            "Second reading negatived"
        );
        assert_eq!(
            long_date("04 September 2026").as_deref(),
            Some("2026-09-04")
        );
        assert_eq!(
            long_date(" 4 September 2026").as_deref(),
            Some("2026-09-04")
        );
        assert_eq!(long_date("September 2026"), None);
        assert_eq!(
            iso_date("2026-10-22T00:00:00").as_deref(),
            Some("2026-10-22")
        );
        assert_eq!(iso_date(""), None);
    }

    #[test]
    fn the_number_is_the_drafting_office_s() {
        assert_eq!(
            drafting_number("bill-2026-024").as_deref(),
            Some("Bill No. 24 of 2026")
        );
        assert_eq!(
            drafting_number("bill-2025-020").as_deref(),
            Some("Bill No. 20 of 2025")
        );
        assert_eq!(
            drafting_number("bill-2025-000"),
            None,
            "a zero is no number"
        );
        assert_eq!(drafting_number("act-2026-026"), None);
    }

    #[tokio::test]
    async fn the_parliament_is_found_from_the_bills_introduced_since_the_election() {
        let server = register(Arc::new(AtomicUsize::new(0)));
        let store = new_store("parliament");
        let endpoints = Endpoints::at(&server.base);
        let roll = read(&store, &endpoints, SINCE).await.expect("read");

        assert_eq!(
            lists_asked(&server.requests()),
            [
                r#"Repealed=N AND PrintType=("bill.first" OR "bill.firstnongovintro") AND Year=2024?"#,
                r#"Repealed=N AND PrintType=("bill.first" OR "bill.firstnongovintro") AND ParliamentNo=58"#,
                r#"PrintType=("bill.third" OR "act.new") AND ParliamentNo=58"#,
            ],
            "the 58th from the bill introduced after the election, not the 57th's before it"
        );
        let first = &server.requests()[0];
        for (name, value) in [
            ("ds", "OQPC-BrowseDataSource"),
            ("subset", "browse"),
            ("start", "1"),
            ("count", "500"),
            ("sortField", "sort.title"),
            ("sortDirection", "asc"),
        ] {
            assert_eq!(first.query(name).as_deref(), Some(value), "{name}");
        }
        assert!(
            roll.bills
                .iter()
                .all(|b| !b.title.starts_with("Last Parliament")),
            "a record of another parliament in the list is left out"
        );

        let before = server.hits();
        read(&store, &endpoints, SINCE).await.expect("second read");
        assert_eq!(
            lists_asked(&server.requests()[before..]).len(),
            2,
            "the number is kept, so a night is the two lists"
        );
    }

    #[tokio::test]
    async fn a_parliament_that_introduced_nothing_in_its_first_year_is_found_in_its_second() {
        let server = TestServer::start(|req| {
            let mut last = print(
                "bill-2028-010",
                "bill.first",
                "Last Bill 2028",
                "s-10",
                "2028-05-01",
            );
            last["parliament.no"] = wrapped("58");
            let mut first = print(
                "bill-2028-090",
                "bill.first",
                "First Bill 2029",
                "s-90",
                "2029-02-10",
            );
            first["parliament.no"] = wrapped("59");
            if req.path.ends_with("/lh") {
                return Response::html(history_page(
                    "First Bill 2029",
                    "Government Bill",
                    &[("Introduced: 10 February 2029", "")],
                ));
            }
            let expression = expression(req);
            let records = if expression.ends_with("Year=2028?") {
                vec![last]
            } else if expression.ends_with("Year=2029?")
                || expression == format!("{INTRODUCED} AND ParliamentNo=59")
            {
                vec![first]
            } else {
                Vec::new()
            };
            Response::json(listing(records))
        });
        let store = new_store("second-year");
        let endpoints = Endpoints::at(&server.base);
        let roll = read(&store, &endpoints, "2028-10-28").await.expect("read");
        assert_eq!(roll.bills.len(), 1);
        assert_eq!(roll.bills[0].title, "First Bill 2029");
        let asked = lists_asked(&server.requests());
        assert!(asked[0].ends_with("Year=2028?") && asked[1].ends_with("Year=2029?"));
        assert!(asked[2..].iter().all(|e| e.ends_with("ParliamentNo=59")));

        let err = read(&store, &endpoints, "2032-10-30")
            .await
            .expect_err("nothing introduced since");
        assert!(
            err.to_string()
                .contains("no bill introduced since 2032-10-30"),
            "got {err}"
        );
    }

    #[tokio::test]
    async fn bills_join_their_passed_prints_and_acts() {
        let (server, roll) = first_night("join").await;
        assert_eq!(roll.state, StateCode::QLD);
        assert_eq!(roll.publisher, "Queensland Legislation");
        assert_eq!(
            roll.source,
            format!("{}/qld-legislation/browse/bills", server.base)
        );
        assert_eq!(roll.bills.len(), 6);
        assert!(
            roll.bills.iter().all(|b| !b.title.starts_with("Coroners")),
            "an Act whose bill is not listed is not made into one"
        );

        // Amended on the way: printed as passed, then the Act.
        let protection = bill(&roll, PROTECTION);
        assert_eq!(protection.number.as_deref(), Some("Bill No. 24 of 2026"));
        assert_eq!(protection.origin.as_deref(), Some("Legislative Assembly"));
        assert_eq!(protection.sponsor, None);
        assert_eq!(protection.status, "Assented");
        assert_eq!(protection.stage, 4);
        assert!(!protection.stopped);
        assert_eq!(
            protection.timeline,
            [
                step("2026-08-26", "Introduced"),
                step("2026-10-15", "Third reading"),
                step("2026-10-22", "Assent"),
            ]
        );
        assert_eq!(protection.act.as_deref(), Some("Act 2026, No. 26"));

        // Passed as introduced: nothing printed as passed, so the third
        // reading is the history page's.
        let transport = bill(&roll, TRANSPORT);
        assert_eq!(
            transport.number.as_deref(),
            Some("Bill No. 20 of 2025"),
            "the id's year, not the title's, which bill-2026-020 also has"
        );
        assert_eq!(transport.status, "Assented");
        assert_eq!(
            transport.timeline,
            [
                step("2026-06-26", "Introduced"),
                step("2026-09-17", "Third reading"),
                step("2026-09-21", "Assent"),
            ]
        );
        assert_eq!(transport.act.as_deref(), Some("Act 2026, No. 23"));
    }

    #[tokio::test]
    async fn a_discharged_bill_stops_where_its_history_page_says() {
        let (_server, roll) = first_night("discharged").await;
        let fares = bill(&roll, FARES);
        assert_eq!(fares.status, "Discharged");
        assert!(fares.stopped);
        assert_eq!(fares.stage, 1);
        assert_eq!(
            fares.timeline,
            [
                step("2025-02-19", "Introduced"),
                step("2025-03-14", "Discharged"),
            ]
        );
        assert_eq!(fares.act, None);
    }

    #[tokio::test]
    async fn a_bill_still_before_the_assembly() {
        let (_server, roll) = first_night("before").await;
        let bail = bill(&roll, BAIL);
        assert_eq!(bail.status, "Before the Assembly");
        assert_eq!(bail.stage, 1);
        assert!(!bail.stopped);
        assert_eq!(bail.timeline, [step("2026-08-27", "Introduced")]);
        assert_eq!(bail.act, None);

        // Its page could not be read, so the lists alone place it.
        let strangulation = bill(&roll, STRANGULATION);
        assert_eq!(strangulation.status, "Before the Assembly");
        assert_eq!(strangulation.timeline, [step("2026-10-28", "Introduced")]);
    }

    #[tokio::test]
    async fn kinds_come_from_the_history_page_else_the_list() {
        let (_server, roll) = first_night("kinds").await;
        let kind = |title: &str| bill(&roll, title).kind.clone();
        assert_eq!(kind(PROTECTION).as_deref(), Some("Government"));
        assert_eq!(kind(BAIL).as_deref(), Some("Government"));
        assert_eq!(kind(FARES).as_deref(), Some("Private member's"));
        assert_eq!(
            kind(STRANGULATION).as_deref(),
            Some("Private member's"),
            "no page, so the asterisk after its title"
        );
        assert_eq!(
            kind(NON_GOVERNMENT).as_deref(),
            Some("Private member's"),
            "no page, so its print type"
        );
    }

    #[tokio::test]
    async fn documents_link_the_bill_and_its_explanatory_note() {
        let (server, roll) = first_night("documents").await;
        let base = format!("{}/qld-legislation", server.base);
        let protection = bill(&roll, PROTECTION);
        assert_eq!(
            protection.source,
            format!("{base}/view/html/bill.first/bill-2026-024/lh")
        );
        assert_eq!(
            protection.text,
            Some(format!("{base}/view/pdf/bill.first/bill-2026-024"))
        );
        assert_eq!(
            protection.explanatory,
            Some(format!("{base}/view/pdf/bill.first.exp/bill-2026-024"))
        );
        assert_eq!(
            bill(&roll, TRANSPORT).explanatory,
            Some(format!("{base}/view/pdf/bill.first.exp/bill-2025-020")),
            "a list of one document comes as the one value"
        );
        assert_eq!(
            bill(&roll, FARES).explanatory,
            None,
            "a human rights statement is no explanatory note"
        );
        assert_eq!(bill(&roll, BAIL).explanatory, None);
        let non_government = bill(&roll, NON_GOVERNMENT);
        assert_eq!(
            non_government.text,
            Some(format!(
                "{base}/view/pdf/bill.firstnongovintro/bill-2026-060"
            ))
        );
        assert_eq!(
            non_government.explanatory,
            Some(format!(
                "{base}/view/pdf/bill.firstnongovintro.exp/bill-2026-060"
            ))
        );
    }

    #[tokio::test]
    async fn settled_bills_are_not_read_again() {
        let night = Arc::new(AtomicUsize::new(0));
        let server = register(Arc::clone(&night));
        let store = new_store("settled");
        let endpoints = Endpoints::at(&server.base);
        let unreadable = [
            "bill.first/bill-2026-050",
            "bill.firstnongovintro/bill-2026-060",
        ];

        read(&store, &endpoints, SINCE).await.expect("first night");
        assert_eq!(
            pages_read(&server.requests()),
            [
                "bill.first/bill-2026-024",
                "bill.first/bill-2025-020",
                "bill.first/bill-2025-002",
                "bill.first/bill-2026-036",
                unreadable[0],
                unreadable[1],
            ],
            "every bill's page the first night"
        );
        let kept: History = store
            .get_json("raw/states/qld/bills/bill-2026-024.json")
            .await
            .unwrap()
            .expect("kept");
        assert!(kept.settled);

        // Its third reading: still read, as it waits on assent.
        night.store(1, Ordering::SeqCst);
        let before = server.hits();
        let roll = read(&store, &endpoints, SINCE).await.expect("second night");
        let asked = &server.requests()[before..];
        assert_eq!(lists_asked(asked).len(), 2);
        assert_eq!(
            pages_read(asked),
            ["bill.first/bill-2026-036", unreadable[0], unreadable[1]],
            "neither the Acts' pages nor the discharged bill's are read again"
        );
        let bail = bill(&roll, BAIL);
        assert_eq!(bail.status, "Passed");
        assert_eq!(bail.stage, 2);
        assert_eq!(
            bail.timeline,
            [
                step("2026-08-27", "Introduced"),
                step("2026-10-28", "Third reading"),
            ]
        );

        // Its Act is listed: its page, last read before, is read once more.
        night.store(2, Ordering::SeqCst);
        let before = server.hits();
        let roll = read(&store, &endpoints, SINCE).await.expect("third night");
        assert_eq!(
            pages_read(&server.requests()[before..]),
            ["bill.first/bill-2026-036", unreadable[0], unreadable[1]]
        );
        let bail = bill(&roll, BAIL);
        assert_eq!(bail.status, "Assented");
        assert_eq!(bail.stage, 4);
        assert_eq!(bail.act.as_deref(), Some("Act 2026, No. 30"));
        assert_eq!(bail.timeline.last(), Some(&step("2026-11-05", "Assent")));

        night.store(3, Ordering::SeqCst);
        let before = server.hits();
        read(&store, &endpoints, SINCE).await.expect("fourth night");
        assert_eq!(
            pages_read(&server.requests()[before..]),
            unreadable,
            "a bill with an Act is not read again; only pages never read are tried"
        );
    }
}
