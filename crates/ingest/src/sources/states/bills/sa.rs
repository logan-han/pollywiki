//! South Australia. The Parliament's legislative tracker, SALT, answers from
//! an API of its own: the sessions it knows, every bill and motion in a
//! session, and a summary of each bill with its readings and the Act it
//! became. A bill that has reached the second house has a record in each
//! house, so the two are put back together here as one bill.
//!
//! A summary costs a request a bill, so each is kept in the store with the
//! status the search gave when it was read, and read again only once that
//! status moves. The bills' text lives on the legislation website, which
//! turns automated readers away, so none is fetched or linked.

use super::{BillRoll, RollBill};
use crate::endpoints::Endpoints;
use crate::http::fetch_json;
use crate::js_url::encode_uri_component;
use crate::store::Store;
use anyhow::{bail, Context, Result};
use chrono::{Datelike, NaiveDate};
use indexmap::IndexMap;
use pollywiki_schema::{slugify, StateCode, TimelineStep};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

/// The tracker as the Parliament's website presents it.
const TRACKER: &str = "https://www.parliament.sa.gov.au/en/Legislation/SALT";

/// One record's page in the tracker, by its id: where the tracker's own
/// search results link each bill.
const RECORD_PAGE: &str = "https://salt.parliament.sa.gov.au/bill/";

/// The tracker asks for every result on one page, and so does this reader.
/// A session that fills the page may have been cut short.
const RESULTS_PER_PAGE: usize = 4000;

/// One reading in a summary's list: "2nd Moved - 07/05/2026".
static READING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([^,]+?)\s+-\s+(\d{1,2}/\d{1,2}/\d{4})").unwrap());

/// A title's own words, then any "Bill" and year it already carries.
static TITLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(.+?)(?:\s+bill)?(?:\s+((?:18|19|20)\d{2}))?$").unwrap());

/// The house a status names: "Passed in HA", "Negatived in LC".
static NAMED_HOUSE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(HA|LC)\b").unwrap());

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Session {
    abbreviation: String,
    start_date: Option<String>,
}

/// One row of a session's search: a bill or a motion, as one house has it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    result_id: i64,
    full_house: Option<String>,
    house: Option<String>,
    session: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    title: Option<String>,
    date_introduced: Option<String>,
    introduced_by: Option<String>,
    status: Option<String>,
    bill_type: Option<i64>,
    bill_number: Option<i64>,
}

impl Record {
    fn is_bill(&self) -> bool {
        self.kind
            .as_deref()
            .is_some_and(|k| k.trim().eq_ignore_ascii_case("bill"))
    }

    fn title(&self) -> &str {
        self.title.as_deref().unwrap_or_default()
    }

    fn status(&self) -> String {
        plain(self.status.as_deref().unwrap_or_default())
    }

    /// The record's house by its full name.
    fn house(&self) -> String {
        let full = plain(self.full_house.as_deref().unwrap_or_default());
        if !full.is_empty() {
            return full;
        }
        self.short_house()
            .map(house_name)
            .unwrap_or_default()
            .to_string()
    }

    /// "HA" or "LC".
    fn short_house(&self) -> Option<&'static str> {
        [self.house.as_deref(), self.full_house.as_deref()]
            .into_iter()
            .flatten()
            .find_map(abbreviate)
    }
}

/// What this reader takes from a bill's summary. Only these facts are kept
/// in the store; the summary's précis and remarks are left behind.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Summary {
    full_bill_type: Option<String>,
    date_introduced: Option<String>,
    introduced_by: Option<String>,
    date_moved: Option<String>,
    date_of_second_reading: Option<String>,
    third_reading_date: Option<String>,
    date_of_third_reading: Option<String>,
    date_completed: Option<String>,
    act_number: Option<i64>,
    year_of_act: Option<i64>,
    final_status_date: Option<String>,
    final_status_text: Option<String>,
    /// Set on the copy the second house makes of a bill it receives.
    origin_house: Option<String>,
    readings_text: Option<String>,
}

/// A summary as kept in the store, with the record's status in the search
/// when it was read: a different status since means the bill has moved and
/// the summary is out of date.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeptSummary {
    status: String,
    summary: Summary,
}

pub async fn read(store: &Store, endpoints: &Endpoints, since: &str) -> Result<BillRoll> {
    let opts = endpoints.opts(1500);
    let sessions: Vec<Session> = fetch_json(&format!("{}/GetSessions", endpoints.sa_salt), &opts)
        .await
        .context("reading SALT's sessions")?;
    let sessions = current_sessions(sessions, since);
    if sessions.is_empty() {
        bail!("SALT lists no session begun on or after {since}");
    }

    let mut records: Vec<Record> = Vec::new();
    let mut seen: HashSet<i64> = HashSet::new();
    for session in &sessions {
        let name = &session.abbreviation;
        let url = format!(
            "{}/Search/?resultsPerPage={RESULTS_PER_PAGE}&parliamentSession={}",
            endpoints.sa_salt,
            query_value(name)
        );
        let raw: Value = fetch_json(&url, &opts)
            .await
            .with_context(|| format!("searching SALT for session {name}"))?;
        store
            .put_json(&format!("raw/states/sa/bills-{}.json", slugify(name)), &raw)
            .await?;
        let listed: Vec<Record> = serde_json::from_value(raw)
            .with_context(|| format!("reading SALT's search for session {name}"))?;
        if listed.len() >= RESULTS_PER_PAGE {
            bail!("SALT's search for session {name} filled its page and may be cut short");
        }
        // Motions share the list, and a record can be listed twice.
        records.extend(
            listed
                .into_iter()
                .filter(|r| r.is_bill() && seen.insert(r.result_id)),
        );
    }

    let summaries = summaries(store, endpoints, &records).await?;
    Ok(BillRoll {
        state: StateCode::SA,
        listed_at: crate::now_iso(),
        publisher: "Parliament of South Australia".to_string(),
        source: TRACKER.to_string(),
        bills: bills(&records, &summaries),
    })
}

/// The sessions of the current parliament: those begun on or after the day
/// it was elected, oldest first. A parliament can sit for more than one.
fn current_sessions(mut sessions: Vec<Session>, since: &str) -> Vec<Session> {
    sessions.retain(|s| {
        s.start_date
            .as_deref()
            .and_then(iso_date)
            .is_some_and(|start| start.as_str() >= since)
    });
    sessions.sort_by_key(|s| s.start_date.clone());
    sessions
}

/// A query value as the tracker's own requests send it: encoded, but with
/// the slash in a session's name ("56th/1st") left bare.
fn query_value(value: &str) -> String {
    encode_uri_component(value).replace("%2F", "/")
}

/// Each record's summary: the one kept in the store while the record's
/// status is still the one it was read at, else a fresh one. A summary that
/// cannot be read leaves the last one kept, where there is one, so a failed
/// request costs a night's news rather than a bill's readings.
async fn summaries(
    store: &Store,
    endpoints: &Endpoints,
    records: &[Record],
) -> Result<HashMap<i64, Summary>> {
    let opts = endpoints.opts(1500);
    let mut out = HashMap::new();
    let mut fetched = 0;
    for record in records {
        let id = record.result_id;
        let key = format!("raw/states/sa/bill-summaries/{id}.json");
        let status = record.status();
        // One kept in a shape this reader no longer reads is read again.
        let kept: Option<KeptSummary> = store
            .get_raw(&key)
            .await?
            .and_then(|raw| serde_json::from_str(&raw).ok());
        if let Some(kept) = kept.as_ref().filter(|k| k.status == status) {
            out.insert(id, kept.summary.clone());
            continue;
        }
        let url = format!("{}/GetBillSummary?billId={id}", endpoints.sa_salt);
        match fetch_json::<Summary>(&url, &opts).await {
            Ok(summary) => {
                store
                    .put_json(
                        &key,
                        &KeptSummary {
                            status,
                            summary: summary.clone(),
                        },
                    )
                    .await?;
                out.insert(id, summary);
                fetched += 1;
            }
            Err(err) => {
                eprintln!("sa-bills: the summary of record {id} could not be read - {err}");
                if let Some(kept) = kept {
                    out.insert(id, kept.summary);
                }
            }
        }
    }
    println!(
        "sa-bills: {} records, {fetched} summaries read afresh",
        records.len()
    );
    Ok(out)
}

/// Records made into bills: grouped by session and title, each second
/// house's copy then put with the bill it was received from.
fn bills(records: &[Record], summaries: &HashMap<i64, Summary>) -> Vec<RollBill> {
    let mut groups: IndexMap<(String, String), Vec<&Record>> = IndexMap::new();
    for record in records {
        let session = plain(record.session.as_deref().unwrap_or_default());
        groups
            .entry((session, title_key(record.title())))
            .or_default()
            .push(record);
    }
    let mut out: Vec<(i64, RollBill)> = Vec::new();
    for group in groups.values() {
        let entries: Vec<Entry> = group
            .iter()
            .map(|r| Entry::new(r, summaries.get(&r.result_id), group))
            .collect();
        for records in pairs(&entries) {
            out.push((records[0].record.result_id, bill(&records)));
        }
    }
    out.sort_by_key(|(first, _)| *first);
    out.into_iter().map(|(_, bill)| bill).collect()
}

/// One record with what is known of it.
struct Entry<'r> {
    record: &'r Record,
    summary: Option<&'r Summary>,
    /// The house this record is in, by its full name.
    house: String,
    /// The house the bill began in, where this record is the copy the
    /// second house made on receiving it.
    received_from: Option<String>,
    /// The day the bill came into this house.
    introduced: Option<String>,
    /// This record's own dated steps.
    steps: Vec<TimelineStep>,
}

impl<'r> Entry<'r> {
    fn new(record: &'r Record, summary: Option<&'r Summary>, group: &[&Record]) -> Self {
        let house = record.house();
        let received_from = match summary {
            Some(summary) => summary
                .origin_house
                .as_deref()
                .map(plain)
                .filter(|from| !from.is_empty() && !from.eq_ignore_ascii_case(&house)),
            // Without its summary: a record nobody is named as bringing in,
            // beside a same-titled record in the other house, is this
            // house's copy of that one.
            None => other_house(&house)
                .filter(|other| {
                    record
                        .introduced_by
                        .as_deref()
                        .is_none_or(|by| by.trim().is_empty())
                        && group.iter().any(|r| r.house().eq_ignore_ascii_case(other))
                })
                .map(str::to_string),
        };
        let (introduced, steps) = steps(record, summary, &house, received_from.is_some());
        Entry {
            record,
            summary,
            house,
            received_from,
            introduced,
            steps,
        }
    }

    /// The latest of this record's dated steps on or before `day`.
    fn active_by(&self, day: Option<&str>) -> Option<&str> {
        self.steps
            .iter()
            .map(|s| s.date.as_str())
            .filter(|date| day.is_none_or(|day| *date <= day))
            .max()
    }

    fn says_assented(&self) -> bool {
        self.summary
            .and_then(|s| s.final_status_text.as_deref())
            .is_some_and(|text| text.trim().to_lowercase().starts_with("assented"))
    }

    /// The day this house finished with the bill.
    fn finished(&self) -> Option<String> {
        let summary = self.summary?;
        [&summary.date_completed, &summary.final_status_date]
            .into_iter()
            .find_map(|date| date.as_deref().and_then(iso_date))
    }

    fn act(&self) -> Option<String> {
        let summary = self.summary?;
        let (number, year) = (summary.act_number?, summary.year_of_act?);
        (number > 0 && year > 0).then(|| format!("Act No. {number} of {year}"))
    }

    /// "Government" or "Private member's", from the bill's type as the
    /// summary names it, else as the tracker labels the search's number for
    /// it.
    fn kind(&self) -> Option<String> {
        let label = self
            .summary
            .and_then(|s| s.full_bill_type.as_deref())
            .filter(|t| !t.trim().is_empty())
            .or(match self.record.bill_type {
                Some(1) => Some("Government Bill"),
                Some(2) => Some("Private Members Bill"),
                _ => None,
            })?
            .to_lowercase();
        if label.contains("government") {
            Some("Government".to_string())
        } else if label.contains("private") {
            Some("Private member's".to_string())
        } else {
            None
        }
    }
}

/// The bills among one title's records: each bill as the house it began in
/// has it, with the copy the other house made of it once it got there. A
/// title can recur within a session (a "Supply" bill each year), and a
/// negatived bill can share its title with a later one, so a copy goes with
/// the same-titled bill of the house it came from that was most recently
/// active on or before the day the copy arrived: a bill reaches the other
/// house the day it is read a third time. A copy that matches no bill
/// stands on its own.
fn pairs<'e, 'r>(entries: &'e [Entry<'r>]) -> Vec<Vec<&'e Entry<'r>>> {
    let (mut copies, origins): (Vec<&Entry>, Vec<&Entry>) =
        entries.iter().partition(|e| e.received_from.is_some());
    copies.sort_by_key(|c| (c.introduced.is_none(), c.introduced.clone()));
    let mut partners: Vec<Option<&Entry>> = vec![None; origins.len()];
    let mut lone: Vec<&Entry> = Vec::new();
    for copy in copies {
        let arrived = copy.introduced.as_deref();
        let from = copy.received_from.as_deref().unwrap_or_default();
        let best = origins
            .iter()
            .enumerate()
            .filter(|(i, origin)| {
                partners[*i].is_none()
                    && origin.house.eq_ignore_ascii_case(from)
                    && match (origin.introduced.as_deref(), arrived) {
                        (Some(began), Some(arrived)) => began <= arrived,
                        _ => true,
                    }
            })
            .map(|(i, origin)| ((origin.active_by(arrived), origin.record.result_id), i))
            .max()
            .map(|(_, i)| i);
        match best {
            Some(i) => partners[i] = Some(copy),
            None => lone.push(copy),
        }
    }
    origins
        .into_iter()
        .zip(partners)
        .map(|(origin, copy)| std::iter::once(origin).chain(copy).collect())
        .chain(lone.into_iter().map(|copy| vec![copy]))
        .collect()
}

/// One bill from its records: the house it began in first, then the copy
/// the other house made of it, if any.
fn bill(records: &[&Entry]) -> RollBill {
    let origin = records.iter().copied().find(|e| e.received_from.is_none());
    let copy = records.iter().copied().find(|e| e.received_from.is_some());
    let began_in = match origin {
        Some(origin) => Some(origin.house.clone()),
        None => copy.and_then(|c| c.received_from.clone()),
    };
    let began = began_in.as_deref().and_then(abbreviate);

    let standing: Vec<u8> = records
        .iter()
        .map(|e| progress(&e.record.status(), began, e.received_from.is_some()))
        .collect();
    let mut stage = standing.iter().max().map_or(1, |p| p / 10);
    if copy.is_some() {
        stage = stage.max(2);
    }
    // Either house's record can carry the end of a bill that went no
    // further: one that lapsed at prorogation while before the Council can
    // still read "Adj 2nd Reading" there, its Assembly record "Lapsed in HA
    // (Prorogation)".
    let stopped = stage < 4 && records.iter().any(|e| is_stopped(&e.record.status()));
    // Otherwise it stands where the record furthest along says, the second
    // house's on a tie, as that is where the bill went next.
    let furthest = records
        .iter()
        .zip(&standing)
        .filter(|(e, _)| !stopped || is_stopped(&e.record.status()))
        .max_by_key(|(e, p)| (**p, e.received_from.is_some()))
        .map_or(records[0], |(e, _)| *e);

    // Both houses' records can say Assented, and need not agree: SALT has
    // the Council's record of the Return to Work (Presumptive Firefighter
    // Injuries) Amendment Bill 2026 as Act No. 7 of 2026 on 2 July, and the
    // Assembly's as Act No. 6 on 25 June, the day the Assembly read it a
    // third time. The record of the house that finished with the bill last
    // is taken at its word. Its date stands for assent only because it says
    // Assented: SALT has no assent date of its own.
    let mut assented: Vec<&Entry> = records
        .iter()
        .copied()
        .filter(|e| e.says_assented())
        .collect();
    assented.sort_by_key(|e| Reverse((e.finished(), e.received_from.is_none())));
    let act = assented.iter().find_map(|e| e.act());
    let assent = assented.iter().find_map(|e| {
        e.summary
            .and_then(|s| s.final_status_date.as_deref())
            .and_then(iso_date)
    });

    let mut timeline: Vec<TimelineStep> = records
        .iter()
        .flat_map(|e| e.steps.iter().cloned())
        .collect();
    if let Some(date) = assent {
        timeline.push(TimelineStep {
            date,
            event: "Assented".to_string(),
        });
    }
    timeline.sort_by(|a, b| a.date.cmp(&b.date));

    // The year it was introduced, or, where SALT gives that no date, the
    // year of its earliest step.
    let year = origin
        .and_then(|o| o.introduced.as_deref())
        .or_else(|| timeline.first().map(|s| s.date.as_str()))
        .and_then(|date| date.get(..4)?.parse::<i32>().ok());
    let sponsor = records.iter().find_map(|e| {
        [
            e.record.introduced_by.as_deref(),
            e.summary.and_then(|s| s.introduced_by.as_deref()),
        ]
        .into_iter()
        .flatten()
        .map(plain)
        .find(|name| !name.is_empty())
    });

    RollBill {
        title: bill_title(records[0].record.title(), year),
        number: number(origin, copy),
        origin: began_in,
        sponsor: sponsor.map(|raw| sponsor_name(&raw)),
        kind: records.iter().find_map(|e| e.kind()),
        status: plain_status(&furthest.record.status(), &furthest.record.house()),
        stage,
        stopped,
        timeline,
        source: format!("{RECORD_PAGE}{}", records[0].record.result_id),
        text: None,
        explanatory: None,
        act,
    }
}

/// A record's dated steps in its own house: the readings its summary
/// lists, each filled from the summary's own date fields where the list
/// lacks it, and the day it came in from the search where the summary has
/// none. That day comes back on its own as well, for pairing the houses'
/// records.
fn steps(
    record: &Record,
    summary: Option<&Summary>,
    house: &str,
    received: bool,
) -> (Option<String>, Vec<TimelineStep>) {
    let listed = summary
        .and_then(|s| s.readings_text.as_deref())
        .map(readings)
        .unwrap_or_default();
    let field = |pick: fn(&Summary) -> Option<&str>| summary.and_then(pick).and_then(iso_date);
    let first = listed
        .first
        .or_else(|| field(|s| s.date_introduced.as_deref()))
        .or_else(|| record.date_introduced.as_deref().and_then(iso_date));
    let second_moved = listed
        .second_moved
        .or_else(|| field(|s| s.date_moved.as_deref()));
    let second_read = listed
        .second_read
        .or_else(|| field(|s| s.date_of_second_reading.as_deref()));
    let third = listed
        .third
        .or_else(|| field(|s| s.third_reading_date.as_deref()))
        .or_else(|| field(|s| s.date_of_third_reading.as_deref()));
    let came_in = if received { "Received" } else { "Introduced" };
    let mut steps: Vec<TimelineStep> = [
        (first.clone(), format!("{came_in} in the {house}")),
        (second_moved, format!("Second reading moved in the {house}")),
        (second_read, format!("Read a second time in the {house}")),
        (third, format!("Read a third time in the {house}")),
    ]
    .into_iter()
    .filter_map(|(date, event)| Some(TimelineStep { date: date?, event }))
    .collect();
    steps.extend(listed.other.into_iter().map(|(label, date)| TimelineStep {
        date,
        event: format!("{label} in the {house}"),
    }));
    (first, steps)
}

/// The readings a summary lists, in its own words: "1st - 07/05/2026,
/// 2nd Moved - 07/05/2026, 2nd Read - 19/05/2026, 3rd Moved -  19/05/2026".
/// The tracker shows the day the third reading was moved as the day the
/// bill was read a third time.
#[derive(Debug, Default)]
struct Readings {
    first: Option<String>,
    second_moved: Option<String>,
    second_read: Option<String>,
    third: Option<String>,
    /// Steps listed in words this reader does not know, as listed.
    other: Vec<(String, String)>,
}

fn readings(text: &str) -> Readings {
    let mut out = Readings::default();
    for reading in READING.captures_iter(text) {
        let Some(date) = NaiveDate::parse_from_str(&reading[2], "%d/%m/%Y")
            .ok()
            .and_then(real_date)
        else {
            continue;
        };
        let label = plain(&reading[1]);
        let words = label.to_lowercase();
        let slot = if words.starts_with("1st") {
            &mut out.first
        } else if words.starts_with("2nd") && words.contains("moved") {
            &mut out.second_moved
        } else if words.starts_with("2nd") {
            &mut out.second_read
        } else if words.starts_with("3rd") {
            &mut out.third
        } else {
            out.other.push((label, date));
            continue;
        };
        slot.get_or_insert(date);
    }
    out
}

/// How far one record's status takes its bill, given the house the bill
/// began in and whether the record is the second house's copy. The tens are
/// the stage; the units tell apart steps within one, so the record furthest
/// along can be found: a bill awaiting assent is further on than one the
/// second house has just passed.
fn progress(status: &str, began: Option<&str>, received: bool) -> u8 {
    let words = status.to_lowercase();
    // The house a status speaks of where it names one ("Passed in HA",
    // "Lapsed in LC"), else the house of the record itself.
    let second_house = match (NAMED_HOUSE.captures(status), began) {
        (Some(named), Some(began)) => &named[1] != began,
        _ => received,
    };
    if words.contains("awaiting assent") {
        33
    } else if words.contains("assent") {
        40
    } else if words.contains("ha/lc") || words.contains("lc/ha") {
        32
    } else if words.contains("consideration of") {
        // The house it began in weighing the other's amendments: both have
        // passed it.
        31
    } else if is_stopped(&words) {
        if second_house {
            22
        } else {
            11
        }
    } else if words.starts_with("passed") {
        if second_house {
            30
        } else {
            20
        }
    } else if received {
        21
    } else {
        10
    }
}

/// It went no further: negatived, lapsed, withdrawn, discharged or laid
/// aside.
fn is_stopped(status: &str) -> bool {
    let words = status.to_lowercase();
    [
        "negatived",
        "lapsed",
        "withdrawn",
        "discharged",
        "laid aside",
    ]
    .iter()
    .any(|word| words.contains(word))
}

/// The bill's number in the house it began in, "No. 4", with its number in
/// the other house beside it once it has one there: "No. 4 (HA No. 6)".
fn number(origin: Option<&Entry>, copy: Option<&Entry>) -> Option<String> {
    let first = origin
        .and_then(|o| o.record.bill_number)
        .filter(|n| *n > 0)
        .map(|n| format!("No. {n}"));
    let second = copy.and_then(|c| {
        let n = c.record.bill_number.filter(|n| *n > 0)?;
        Some(match c.record.short_house() {
            Some(house) => format!("{house} No. {n}"),
            None => format!("No. {n}"),
        })
    });
    match (first, second) {
        (Some(first), Some(second)) => Some(format!("{first} ({second})")),
        (first, second) => first.or(second),
    }
}

/// A title as other parliaments write theirs, "... Bill 2026": SALT lists
/// most with neither "Bill" nor the year, and a few with one or the other
/// ("Supply 2023"). The year is the one the bill was introduced in.
fn bill_title(listed: &str, year: Option<i32>) -> String {
    let listed = plain(listed);
    let Some(parts) = TITLE.captures(&listed) else {
        return listed;
    };
    let year = parts
        .get(2)
        .map(|y| y.as_str().to_string())
        .or_else(|| year.map(|y| y.to_string()));
    match year {
        Some(year) => format!("{} Bill {year}", &parts[1]),
        None => format!("{} Bill", &parts[1]),
    }
}

/// What a bill's records share in their titles. The two houses' records of
/// one bill can differ in capitals, spacing and punctuation ("Pre-trial" and
/// "Pre-Trial"), and a title that recurs each year can carry its year in one
/// house and not the other ("Supply 2023" and "Supply").
fn title_key(title: &str) -> String {
    let title = plain(title);
    let words = TITLE
        .captures(&title)
        .and_then(|parts| parts.get(1))
        .map_or(title.as_str(), |words| words.as_str());
    words
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// "HA" or "LC", for a house named either way.
fn abbreviate(house: &str) -> Option<&'static str> {
    let house = house.trim().to_lowercase();
    if house == "ha" || house.contains("assembly") {
        Some("HA")
    } else if house == "lc" || house.contains("council") {
        Some("LC")
    } else {
        None
    }
}

/// The tracker's status in words a reader need not decode: "Adj 2nd
/// Reading" is the second reading adjourned, "HA" the House of Assembly, and
/// a stage that names no house says which one it is in. Stage and whether a
/// bill stopped are still read from the tracker's own words.
fn plain_status(raw: &str, house: &str) -> String {
    let mut status = raw.trim().to_string();
    for (from, to) in [
        ("Adj 2nd Reading", "Second reading adjourned"),
        ("1st Reading", "First reading"),
        ("2nd Reading", "Second reading"),
        ("3rd Reading", "Third reading"),
        ("In Committee", "In committee"),
        ("Awaiting Assent", "Awaiting assent"),
        ("Amendments", "amendments"),
        ("(Prorogation)", "(prorogation)"),
        ("HA/LC", "both houses"),
        (" in HA", " in the House of Assembly"),
        (" in LC", " in the Legislative Council"),
        ("Passed HA", "Passed the House of Assembly"),
        ("Passed LC", "Passed the Legislative Council"),
    ] {
        status = status.replace(from, to);
    }
    let status = NAMED_HOUSE
        .replace_all(&status, |c: &regex::Captures| house_name(&c[1]))
        .to_string();
    let names_a_house = ["House of Assembly", "Legislative Council", "both houses"]
        .iter()
        .any(|h| status.contains(h));
    let a_stage = [
        "First reading",
        "Second reading",
        "Third reading",
        "In committee",
    ]
    .iter()
    .any(|w| status.starts_with(w));
    if a_stage && !names_a_house && !house.is_empty() {
        format!("{status} in the {house}")
    } else {
        status
    }
}

/// Who brought a bill in, without the tracker's wrapping: it writes a
/// minister as "Deputy Premier (Hon K J Maher (MLC))" and a private member
/// as "Hon N J Centofanti (Hon N J Centofanti (MLC))".
fn sponsor_name(raw: &str) -> String {
    let Some((role, rest)) = raw.split_once(" (") else {
        return raw.to_string();
    };
    let inner = rest.strip_suffix(')').unwrap_or(rest);
    let name = inner
        .trim_end_matches(" (MLC)")
        .trim_end_matches(" (MP)")
        .trim();
    if name.is_empty() {
        raw.to_string()
    } else if name == role.trim() {
        name.to_string()
    } else {
        format!("{name} ({})", role.trim())
    }
}

fn house_name(short: &str) -> &'static str {
    if short == "HA" {
        "House of Assembly"
    } else {
        "Legislative Council"
    }
}

fn other_house(house: &str) -> Option<&'static str> {
    Some(match abbreviate(house)? {
        "HA" => "Legislative Council",
        _ => "House of Assembly",
    })
}

/// "2026-05-07T00:00:00" as "2026-05-07".
fn iso_date(text: &str) -> Option<String> {
    NaiveDate::parse_from_str(text.get(..10)?, "%Y-%m-%d")
        .ok()
        .and_then(real_date)
}

/// SALT fills a date it does not have with 0001-01-01, which is no date.
fn real_date(date: NaiveDate) -> Option<String> {
    (date.year() > 1).then(|| date.format("%Y-%m-%d").to_string())
}

/// Text trimmed, with runs of space collapsed: SALT's records carry strays
/// ("Hon  R A Simms", " Andrews", "Supply ").
fn plain(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::LocalStore;
    use crate::test_http::{Request, Response, TestServer};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn scratch(name: &str) -> Store {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/state-bills-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        Store::Local(LocalStore::new(dir))
    }

    /// The path without its query.
    fn route(req: &Request) -> &str {
        req.path.split('?').next().unwrap_or_default()
    }

    fn events(bill: &RollBill) -> Vec<(&str, &str)> {
        bill.timeline
            .iter()
            .map(|s| (s.date.as_str(), s.event.as_str()))
            .collect()
    }

    fn record(
        id: i64,
        short: &str,
        title: &str,
        introduced: &str,
        by: &str,
        status: &str,
    ) -> Record {
        serde_json::from_value(json!({
            "resultId": id, "fullHouse": house_name(short), "session": "56th/1st",
            "house": short, "type": "Bill", "title": title,
            "dateIntroduced": format!("{introduced}T00:00:00"), "introducedBy": by,
            "status": status, "billType": 1, "billNumber": id, "motionType": -1
        }))
        .unwrap()
    }

    /// What the second house's copy of a bill begun in `short` says of it.
    fn received_from(short: &str) -> Summary {
        Summary {
            origin_house: Some(house_name(short).to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn a_summary_s_readings_are_dated_and_the_placeholder_is_no_date() {
        let listed = readings(
            "1st - 07/05/2026, 2nd Moved - 07/05/2026, 2nd Read - 19/05/2026, 3rd Moved -  19/05/2026",
        );
        assert_eq!(listed.first.as_deref(), Some("2026-05-07"));
        assert_eq!(listed.second_moved.as_deref(), Some("2026-05-07"));
        assert_eq!(listed.second_read.as_deref(), Some("2026-05-19"));
        assert_eq!(listed.third.as_deref(), Some("2026-05-19"));
        assert!(listed.other.is_empty());

        // A reading with no date, or SALT's stand-in for one, is no step;
        // words this reader does not know are kept as they are.
        let partial = readings("1st - 01/01/0001, 2nd Moved - , Restored - 02/02/2027");
        assert!(partial.first.is_none() && partial.second_moved.is_none());
        assert_eq!(
            partial.other,
            [("Restored".to_string(), "2027-02-02".to_string())]
        );

        assert_eq!(
            iso_date("2026-05-07T00:00:00").as_deref(),
            Some("2026-05-07")
        );
        assert_eq!(iso_date("0001-01-01T00:00:00"), None);
        assert_eq!(iso_date(""), None);
    }

    #[test]
    fn titles_read_as_bills_and_match_across_the_houses() {
        assert_eq!(bill_title("Supply", Some(2026)), "Supply Bill 2026");
        assert_eq!(
            bill_title(
                "Rail Safety National Law (South Australia) (Miscellaneous)  Amendment ",
                Some(2026)
            ),
            "Rail Safety National Law (South Australia) (Miscellaneous) Amendment Bill 2026"
        );
        // A title that already carries its year or "Bill" keeps them.
        assert_eq!(bill_title("Supply 2023", Some(2024)), "Supply Bill 2023");
        assert_eq!(
            bill_title(
                "Labour Hire Licensing (Scope of Act) Amendment Bill",
                Some(2025)
            ),
            "Labour Hire Licensing (Scope of Act) Amendment Bill 2025"
        );
        assert_eq!(
            bill_title(
                "Statutes Amendment (Attorney-General's Portfolio) (No 2)",
                None
            ),
            "Statutes Amendment (Attorney-General's Portfolio) (No 2) Bill"
        );

        for (one, other) in [
            (
                "Evidence (Pre-trial Special Hearings) Amendment",
                "Evidence (Pre-Trial Special Hearings) Amendment",
            ),
            (
                "Statutes Amendment (National Energy Laws)(Regulatory Sandboxing)",
                "Statutes Amendment (National Energy Laws) (Regulatory Sandboxing)",
            ),
            ("Supply 2023", "Supply"),
        ] {
            assert_eq!(title_key(one), title_key(other), "{one} / {other}");
        }
        assert_ne!(
            title_key("Statutes Amendment (Attorney-General's Portfolio)"),
            title_key("Statutes Amendment (Attorney-General's Portfolio) (No 2)")
        );
    }

    #[test]
    fn a_sponsor_is_named_without_the_tracker_s_wrapping() {
        assert_eq!(sponsor_name("Mrs Hurn"), "Mrs Hurn");
        assert_eq!(
            sponsor_name("Hon N J Centofanti (Hon N J Centofanti (MLC))"),
            "Hon N J Centofanti"
        );
        assert_eq!(
            sponsor_name("Local Government (Hon R K Pearce)"),
            "Hon R K Pearce (Local Government)"
        );
    }

    #[test]
    fn statuses_set_how_far_a_bill_has_come_and_whether_it_stopped() {
        // A bill's record in the house it began in and, once it has got
        // there, the other house's copy.
        let one_bill = |first: (&str, &str), second: Option<(&str, &str)>| {
            let mut records = vec![record(
                1,
                first.0,
                "Example",
                "2026-05-06",
                "Mrs Hurn",
                first.1,
            )];
            let mut summaries = HashMap::new();
            if let Some((short, status)) = second {
                records.push(record(2, short, "Example", "2026-06-03", "", status));
                summaries.insert(2, received_from(first.0));
            }
            let mut bills = bills(&records, &summaries);
            assert_eq!(bills.len(), 1, "{first:?} then {second:?}");
            bills.remove(0)
        };
        for (first, second, stage, stopped, status) in [
            (
                ("HA", "1st Reading"),
                None,
                1,
                false,
                "First reading in the House of Assembly",
            ),
            (
                ("LC", "Adj 2nd Reading"),
                None,
                1,
                false,
                "Second reading adjourned in the Legislative Council",
            ),
            (
                ("LC", "In Committee"),
                None,
                1,
                false,
                "In committee in the Legislative Council",
            ),
            (
                ("HA", "Passed in HA"),
                None,
                2,
                false,
                "Passed in the House of Assembly",
            ),
            (
                ("LC", "Passed in LC with amendments"),
                None,
                2,
                false,
                "Passed in the Legislative Council with amendments",
            ),
            (
                ("LC", "Negatived in LC"),
                None,
                1,
                true,
                "Negatived in the Legislative Council",
            ),
            (
                ("HA", "Withdrawn in HA"),
                None,
                1,
                true,
                "Withdrawn in the House of Assembly",
            ),
            (("HA", "Awaiting Assent"), None, 3, false, "Awaiting assent"),
            (("HA", "Assented"), None, 4, false, "Assented"),
            // In the second house, that house's words, unless the first
            // house's have gone further.
            (
                ("LC", "Passed in LC"),
                Some(("HA", "Adj 2nd Reading")),
                2,
                false,
                "Second reading adjourned in the House of Assembly",
            ),
            (
                ("HA", "Passed in HA"),
                Some(("LC", "Passed in LC")),
                3,
                false,
                "Passed in the Legislative Council",
            ),
            (
                ("HA", "Awaiting Assent"),
                Some(("LC", "Passed HA/LC")),
                3,
                false,
                "Awaiting assent",
            ),
            (
                ("HA", "Consideration of LC Amendments"),
                Some(("LC", "Passed in LC with amendments")),
                3,
                false,
                "Consideration of Legislative Council amendments",
            ),
            (
                ("LC", "Assented"),
                Some(("HA", "Passed HA/LC")),
                4,
                false,
                "Assented",
            ),
            // Its end can be on either record.
            (
                ("LC", "Passed in LC with amendments"),
                Some(("HA", "Negatived in HA")),
                2,
                true,
                "Negatived in the House of Assembly",
            ),
            (
                ("HA", "Lapsed in HA (Prorogation)"),
                Some(("LC", "Adj 2nd Reading")),
                2,
                true,
                "Lapsed in the House of Assembly (prorogation)",
            ),
        ] {
            let bill = one_bill(first, second);
            assert_eq!(
                (bill.stage, bill.stopped, bill.status.as_str()),
                (stage, stopped, status),
                "{first:?} then {second:?}"
            );
        }
    }

    #[test]
    fn a_recurring_title_pairs_each_copy_with_its_own_bill() {
        let records = [
            record(10, "HA", "Supply", "2026-05-05", "Treasurer", "Assented"),
            record(11, "LC", "Supply", "2026-06-03", "", "Assented"),
            record(
                12,
                "HA",
                "Supply",
                "2026-08-01",
                "Mr Example",
                "Negatived in HA",
            ),
            record(
                13,
                "HA",
                "Supply 2027",
                "2027-04-01",
                "Treasurer",
                "Passed in HA",
            ),
            record(14, "LC", "Supply", "2027-04-02", "", "1st Reading"),
        ];
        // The copy received in 2026 lost its summary to a failed request:
        // nobody is named as bringing it in, and the Assembly has a bill of
        // its title, so it is still taken for the Assembly's.
        let summaries = HashMap::from([(14, received_from("HA"))]);
        let bills = bills(&records, &summaries);
        let listed: Vec<(&str, Option<&str>, &str, u8)> = bills
            .iter()
            .map(|b| {
                (
                    b.title.as_str(),
                    b.number.as_deref(),
                    b.status.as_str(),
                    b.stage,
                )
            })
            .collect();
        assert_eq!(
            listed,
            [
                (
                    "Supply Bill 2026",
                    Some("No. 10 (LC No. 11)"),
                    "Assented",
                    4
                ),
                (
                    "Supply Bill 2026",
                    Some("No. 12"),
                    "Negatived in the House of Assembly",
                    1
                ),
                (
                    "Supply Bill 2027",
                    Some("No. 13 (LC No. 14)"),
                    "First reading in the Legislative Council",
                    2
                ),
            ]
        );
    }

    const SESSIONS: &str = r#"[
      {"sessionId":183,"parliamentId":56.0,"sessionNumber":2.0,"startDate":"2027-02-02T00:00:00","endDate":null,"abbreviation":"56th/2nd","title":"56th Parliament 2nd Session (02/02/2027 - )"},
      {"sessionId":182,"parliamentId":56.0,"sessionNumber":1.0,"startDate":"2026-05-05T00:00:00","endDate":"2027-01-20T00:00:00","abbreviation":"56th/1st","title":"56th Parliament 1st Session (05/05/2026 - 20/01/2027)"},
      {"sessionId":181,"parliamentId":55.0,"sessionNumber":1.0,"startDate":"2022-05-03T00:00:00","endDate":"2026-02-21T00:00:00","abbreviation":"55th/1st","title":"55th Parliament 1st Session (03/05/2022 - 21/02/2026)"},
      {"sessionId":13,"parliamentId":1.0,"sessionNumber":1.0,"startDate":null,"endDate":null,"abbreviation":"1st/1st","title":"Undated"}
    ]"#;

    /// The first session's search, as SALT gives it: the two houses'
    /// records of one bill, a motion among the bills, and a record listed
    /// twice.
    const FIRST_SESSION: &str = r#"[
      {"resultId":5622,"fullHouse":"Legislative Council","session":"56th/1st","house":"LC","type":"Bill","title":"First Nations Voice Repeal","dateIntroduced":"2026-05-06T00:00:00","introducedBy":"Hon N J Centofanti (Hon N J Centofanti (MLC))","status":"Negatived in LC","billType":2,"billNumber":1,"motionType":-1},
      {"resultId":5625,"fullHouse":"Legislative Council","session":"56th/1st","house":"LC","type":"Bill","title":"Return to Work (Presumptive Firefighter Injuries) Amendment","dateIntroduced":"2026-05-07T00:00:00","introducedBy":"Deputy Premier (Hon K J Maher (MLC))","status":"Assented","billType":1,"billNumber":4,"motionType":-1},
      {"resultId":5627,"fullHouse":"Legislative Council","session":"56th/1st","house":"LC","type":"Bill","title":"Evidence (Pre-trial Special Hearings) Amendment","dateIntroduced":"2026-05-07T00:00:00","introducedBy":"Deputy Premier (Hon K J Maher (MLC))","status":"Passed in LC","billType":1,"billNumber":6,"motionType":-1},
      {"resultId":5629,"fullHouse":"House of Assembly","session":"56th/1st","house":"HA","type":"Bill","title":"Return to Work (Presumptive Firefighter Injuries) Amendment","dateIntroduced":"2026-05-19T00:00:00","introducedBy":"","status":"Assented","billType":1,"billNumber":6,"motionType":-1},
      {"resultId":5649,"fullHouse":"House of Assembly","session":"56th/1st","house":"HA","type":"Bill","title":"Evidence (Pre-Trial Special Hearings) Amendment","dateIntroduced":"2026-06-17T00:00:00","introducedBy":"","status":"Adj 2nd Reading","billType":1,"billNumber":13,"motionType":-1},
      {"resultId":5678,"fullHouse":"House of Assembly","session":"56th/1st","house":"HA","type":"Bill","title":"Local Government (Transition Out of Administration) Amendment","dateIntroduced":"2026-09-16T00:00:00","introducedBy":"Local Government (Hon R K Pearce)","status":"Awaiting Assent","billType":1,"billNumber":26,"motionType":-1},
      {"resultId":5684,"fullHouse":"Legislative Council","session":"56th/1st","house":"LC","type":"Bill","title":"Local Government (Transition Out of Administration) Amendment","dateIntroduced":"0001-01-01T00:00:00","introducedBy":"","status":"Passed HA/LC","billType":1,"billNumber":37,"motionType":-1},
      {"resultId":9001,"fullHouse":"House of Assembly","session":"56th/1st","house":"HA","type":"Motion","title":"Address in Reply","dateIntroduced":"2026-05-05T00:00:00","introducedBy":"Mr Example","status":"Agreed","billType":-1,"billNumber":0,"motionType":3},
      {"resultId":5625,"fullHouse":"Legislative Council","session":"56th/1st","house":"LC","type":"Bill","title":"Return to Work (Presumptive Firefighter Injuries) Amendment","dateIntroduced":"2026-05-07T00:00:00","introducedBy":"Deputy Premier (Hon K J Maher (MLC))","status":"Assented","billType":1,"billNumber":4,"motionType":-1}
    ]"#;

    const SECOND_SESSION: &str = r#"[
      {"resultId":5700,"fullHouse":"House of Assembly","session":"56th/2nd","house":"HA","type":"Bill","title":"Supply ","dateIntroduced":"2027-02-03T00:00:00","introducedBy":"Treasurer (Hon A Koutsantonis)","status":"1st Reading","billType":1,"billNumber":1,"motionType":-1}
    ]"#;

    /// Summaries as SALT gives them, the two Return to Work records as the
    /// research read them, cut to what is read here and a field or two that
    /// is not.
    fn summary(id: &str) -> Option<&'static str> {
        Some(match id {
            "5622" => {
                r#"{"billID":5622,"fullHouse":"Legislative Council","fullBillType":"Private Members Bill","dateIntroduced":"2026-05-06T00:00:00","introducedBy":"Hon N J Centofanti (Hon N J Centofanti (MLC))","originHouse":null,"readingsText":"1st - 06/05/2026, 2nd Moved - 06/05/2026"}"#
            }
            "5625" => {
                r#"{"billID":5625,"billNumber":4,"sessionOfParliament":"56th/1st","fullHouse":"Legislative Council","fullBillType":"Government Bill","title":"Return to Work (Presumptive Firefighter Injuries) Amendment","statusText":"Assented","dateIntroduced":"2026-05-07T00:00:00","introducedBy":"Deputy Premier (Hon K J Maher (MLC))","dateMoved":"2026-05-07T00:00:00","dateOfSecondReading":"2026-05-19T00:00:00","dateOfThirdReading":null,"dateCompleted":"2026-07-02T00:00:00","remarks":"<p></p>","actNumber":7,"yearOfAct":2026,"precis":"","thirdReadingDate":"2026-05-19T00:00:00","originHouse":null,"hansardDate":"0001-01-01T00:00:00","finalStatusDate":"2026-07-02T00:00:00","readingsText":"1st - 07/05/2026, 2nd Moved - 07/05/2026, 2nd Read - 19/05/2026, 3rd Moved -  19/05/2026","headingText":"Introduced in the Legislative Council","finalStatusText":"Assented 7 2026"}"#
            }
            "5629" => {
                r#"{"billID":5629,"billNumber":6,"sessionOfParliament":"56th/1st","fullHouse":"House of Assembly","fullBillType":"Government Bill","title":"Return to Work (Presumptive Firefighter Injuries) Amendment","statusText":"Assented","dateIntroduced":"2026-05-19T00:00:00","introducedBy":"","dateMoved":"2026-06-04T00:00:00","dateOfSecondReading":"2026-06-25T00:00:00","dateOfThirdReading":null,"movedBy":"Emergency Services (Hon R K Pearce)","dateCompleted":"2026-06-25T00:00:00","actNumber":6,"yearOfAct":2026,"precis":"<p></p>","thirdReadingDate":"2026-06-25T00:00:00","originHouse":"Legislative Council","finalStatusDate":"2026-06-25T00:00:00","readingsText":"1st - 19/05/2026, 2nd Moved - 04/06/2026, 2nd Read - 25/06/2026, 3rd Moved -  25/06/2026","headingText":"Received in the House of Assembly","finalStatusText":"Assented 6 2026"}"#
            }
            "5627" => {
                r#"{"billID":5627,"fullHouse":"Legislative Council","fullBillType":"Government Bill","dateIntroduced":"2026-05-07T00:00:00","introducedBy":"Deputy Premier (Hon K J Maher (MLC))","originHouse":null,"readingsText":"1st - 07/05/2026, 2nd Moved - 07/05/2026, 2nd Read - 16/06/2026, 3rd Moved - 16/06/2026"}"#
            }
            "5649" => {
                r#"{"billID":5649,"fullHouse":"House of Assembly","fullBillType":"Government Bill","dateIntroduced":"2026-06-17T00:00:00","introducedBy":"","originHouse":"Legislative Council","readingsText":"1st - 17/06/2026, 2nd Moved - 25/06/2026"}"#
            }
            "5678" => {
                r#"{"billID":5678,"fullHouse":"House of Assembly","fullBillType":"Government Bill","dateIntroduced":"2026-09-16T00:00:00","introducedBy":"Local Government (Hon R K Pearce)","originHouse":null,"readingsText":"1st - 16/09/2026, 2nd Moved - 16/09/2026, 2nd Read - 14/10/2026, 3rd Moved - 14/10/2026"}"#
            }
            "5684" => {
                r#"{"billID":5684,"fullHouse":"Legislative Council","fullBillType":"Government Bill","dateIntroduced":"0001-01-01T00:00:00","introducedBy":"","originHouse":"House of Assembly","dateMoved":"0001-01-01T00:00:00","finalStatusDate":"0001-01-01T00:00:00","readingsText":"1st - 01/01/0001, 2nd Moved - 15/10/2026, 2nd Read - 29/10/2026, 3rd Moved - 29/10/2026"}"#
            }
            "5700" => {
                r#"{"billID":5700,"fullHouse":"House of Assembly","fullBillType":"Government Bill","dateIntroduced":"2027-02-03T00:00:00","introducedBy":"Treasurer (Hon A Koutsantonis)","originHouse":null,"readingsText":"1st - 03/02/2027"}"#
            }
            _ => return None,
        })
    }

    #[tokio::test]
    async fn the_parliament_s_sessions_are_read_and_each_bill_is_one_entry() {
        let server = TestServer::start(|req| match route(req) {
            "/sa-salt/GetSessions" => Response::json(SESSIONS),
            "/sa-salt/Search/" => match req.query("parliamentSession").as_deref() {
                Some("56th/1st") => Response::json(FIRST_SESSION),
                Some("56th/2nd") => Response::json(SECOND_SESSION),
                _ => Response::status(404, "no such session"),
            },
            "/sa-salt/GetBillSummary" => match req.query("billId").as_deref().and_then(summary) {
                Some(body) => Response::json(body),
                None => Response::status(404, "no such bill"),
            },
            _ => Response::status(404, "unexpected path"),
        });
        let store = scratch("sa-read");
        let roll = read(&store, &Endpoints::at(&server.base), "2026-03-21")
            .await
            .expect("read");

        let sent = server.requests();
        let searched: Vec<String> = sent
            .iter()
            .filter(|r| route(r) == "/sa-salt/Search/")
            .inspect(|r| assert_eq!(r.query("resultsPerPage").as_deref(), Some("4000")))
            .filter_map(|r| r.query("parliamentSession"))
            .collect();
        assert_eq!(
            searched,
            ["56th/1st", "56th/2nd"],
            "the sessions since the election, oldest first"
        );
        let mut summarised: Vec<String> = sent
            .iter()
            .filter(|r| route(r) == "/sa-salt/GetBillSummary")
            .filter_map(|r| r.query("billId"))
            .collect();
        summarised.sort();
        assert_eq!(
            summarised,
            ["5622", "5625", "5627", "5629", "5649", "5678", "5684", "5700"],
            "a summary for each bill's record, once, and none for the motion"
        );

        assert_eq!(roll.state, StateCode::SA);
        assert_eq!(roll.publisher, "Parliament of South Australia");
        assert_eq!(roll.source, TRACKER);
        let titles: Vec<&str> = roll.bills.iter().map(|b| b.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "First Nations Voice Repeal Bill 2026",
                "Return to Work (Presumptive Firefighter Injuries) Amendment Bill 2026",
                "Evidence (Pre-trial Special Hearings) Amendment Bill 2026",
                "Local Government (Transition Out of Administration) Amendment Bill 2026",
                "Supply Bill 2027",
            ],
            "one entry a bill, however many houses it has reached"
        );

        let repeal = &roll.bills[0];
        assert_eq!(
            (repeal.status.as_str(), repeal.stage, repeal.stopped),
            ("Negatived in the Legislative Council", 1, true)
        );
        assert_eq!(repeal.kind.as_deref(), Some("Private member's"));
        assert_eq!(repeal.number.as_deref(), Some("No. 1"));

        // Begun in the Council, received by the Assembly: one bill, with
        // the Council's sponsor and both houses' readings.
        let firefighters = &roll.bills[1];
        assert_eq!(firefighters.origin.as_deref(), Some("Legislative Council"));
        assert_eq!(
            firefighters.sponsor.as_deref(),
            Some("Hon K J Maher (Deputy Premier)"),
            "the tracker's wrapping undone"
        );
        assert_eq!(firefighters.kind.as_deref(), Some("Government"));
        assert_eq!(firefighters.number.as_deref(), Some("No. 4 (HA No. 6)"));
        assert_eq!(
            (
                firefighters.status.as_str(),
                firefighters.stage,
                firefighters.stopped
            ),
            ("Assented", 4, false)
        );
        assert_eq!(
            firefighters.act.as_deref(),
            Some("Act No. 7 of 2026"),
            "the Council finished with it last"
        );
        assert_eq!(
            events(firefighters),
            [
                ("2026-05-07", "Introduced in the Legislative Council"),
                (
                    "2026-05-07",
                    "Second reading moved in the Legislative Council"
                ),
                (
                    "2026-05-19",
                    "Read a second time in the Legislative Council"
                ),
                ("2026-05-19", "Read a third time in the Legislative Council"),
                ("2026-05-19", "Received in the House of Assembly"),
                (
                    "2026-06-04",
                    "Second reading moved in the House of Assembly"
                ),
                ("2026-06-25", "Read a second time in the House of Assembly"),
                ("2026-06-25", "Read a third time in the House of Assembly"),
                ("2026-07-02", "Assented"),
            ]
        );
        assert_eq!(
            firefighters.source,
            "https://salt.parliament.sa.gov.au/bill/5625"
        );
        assert!(firefighters.text.is_none() && firefighters.explanatory.is_none());

        // The houses differ on a capital; it is still one bill, now before
        // the Assembly.
        let evidence = &roll.bills[2];
        assert_eq!(evidence.origin.as_deref(), Some("Legislative Council"));
        assert_eq!(
            (evidence.status.as_str(), evidence.stage),
            ("Second reading adjourned in the House of Assembly", 2)
        );
        assert_eq!(evidence.number.as_deref(), Some("No. 6 (HA No. 13)"));

        // The Council gives no real day for receiving it, so that step is
        // left out rather than dated 0001-01-01.
        let administration = &roll.bills[3];
        assert_eq!(
            (administration.status.as_str(), administration.stage),
            ("Awaiting assent", 3)
        );
        assert_eq!(
            administration.sponsor.as_deref(),
            Some("Hon R K Pearce (Local Government)")
        );
        assert_eq!(
            events(administration),
            [
                ("2026-09-16", "Introduced in the House of Assembly"),
                (
                    "2026-09-16",
                    "Second reading moved in the House of Assembly"
                ),
                ("2026-10-14", "Read a second time in the House of Assembly"),
                ("2026-10-14", "Read a third time in the House of Assembly"),
                (
                    "2026-10-15",
                    "Second reading moved in the Legislative Council"
                ),
                (
                    "2026-10-29",
                    "Read a second time in the Legislative Council"
                ),
                ("2026-10-29", "Read a third time in the Legislative Council"),
            ]
        );
        assert!(administration.act.is_none());

        let supply = &roll.bills[4];
        assert_eq!(supply.origin.as_deref(), Some("House of Assembly"));
        assert_eq!(
            (supply.status.as_str(), supply.stage),
            ("First reading in the House of Assembly", 1)
        );

        // What was read is kept: the search for replay, and each summary
        // with the status it was read at and only the facts used.
        assert!(store
            .get_json::<Value>("raw/states/sa/bills-56th-1st.json")
            .await
            .unwrap()
            .is_some());
        let kept: Value = store
            .get_json("raw/states/sa/bill-summaries/5625.json")
            .await
            .unwrap()
            .expect("kept");
        assert_eq!(kept["status"], "Assented");
        assert_eq!(kept["summary"]["actNumber"], 7);
        assert!(kept["summary"].get("precis").is_none());
    }

    #[tokio::test]
    async fn a_summary_is_read_again_only_once_its_status_moves() {
        let searches = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&searches);
        let server = TestServer::start(move |req| match route(req) {
            "/sa-salt/GetSessions" => Response::json(
                r#"[{"sessionId":182,"startDate":"2026-05-05T00:00:00","endDate":null,"abbreviation":"56th/1st"}]"#,
            ),
            "/sa-salt/Search/" => {
                // The private member's bill moves on after the first read.
                let moved = counter.fetch_add(1, Ordering::SeqCst) > 0;
                let status = if moved {
                    "2nd Reading"
                } else {
                    "Adj 2nd Reading"
                };
                Response::json(
                    json!([
                        {"resultId": 5625, "fullHouse": "Legislative Council", "session": "56th/1st",
                         "house": "LC", "type": "Bill",
                         "title": "Return to Work (Presumptive Firefighter Injuries) Amendment",
                         "dateIntroduced": "2026-05-07T00:00:00",
                         "introducedBy": "Deputy Premier (Hon K J Maher (MLC))",
                         "status": "Assented", "billType": 1, "billNumber": 4, "motionType": -1},
                        {"resultId": 5644, "fullHouse": "House of Assembly", "session": "56th/1st",
                         "house": "HA", "type": "Bill", "title": "Public Finance Transparency",
                         "dateIntroduced": "2026-06-17T00:00:00", "introducedBy": "Mrs Hurn",
                         "status": status, "billType": 2, "billNumber": 12, "motionType": -1}
                    ])
                    .to_string(),
                )
            }
            "/sa-salt/GetBillSummary" => Response::json(
                json!({"billID": req.query("billId"), "originHouse": null,
                       "readingsText": "1st - 17/06/2026"})
                .to_string(),
            ),
            _ => Response::status(404, "unexpected path"),
        });
        let store = scratch("sa-kept");
        let endpoints = Endpoints::at(&server.base);
        let summaries_read = || -> Vec<String> {
            server
                .requests()
                .iter()
                .filter(|r| route(r) == "/sa-salt/GetBillSummary")
                .filter_map(|r| r.query("billId"))
                .collect()
        };

        read(&store, &endpoints, "2026-03-21")
            .await
            .expect("first read");
        assert_eq!(summaries_read(), ["5625", "5644"]);

        let roll = read(&store, &endpoints, "2026-03-21")
            .await
            .expect("second read");
        assert_eq!(
            summaries_read(),
            ["5625", "5644", "5644"],
            "only the bill whose status moved is read again"
        );
        assert_eq!(
            roll.bills[1].status,
            "Second reading in the House of Assembly"
        );

        read(&store, &endpoints, "2026-03-21")
            .await
            .expect("third read");
        assert_eq!(summaries_read().len(), 3, "nothing moved, nothing read");
        assert_eq!(
            searches.load(Ordering::SeqCst),
            3,
            "the list is read each time"
        );
    }
}
