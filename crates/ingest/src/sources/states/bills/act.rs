//! The ACT. The Assembly has one chamber, and its Bills List follows every
//! bill of the current Assembly: a Word file, replaced with each edition,
//! whose tables place each bill before the Assembly, passed, negatived, or
//! discharged or withdrawn, with the dates that got it there. The
//! Legislation Register keeps a page per bill with its type, sponsor,
//! documents and progress. Only the Assembly's Bills Volume links a bill to
//! that page, by its title; the Volume is kept by hand and lags, so a bill
//! it has not caught up with goes without.
//!
//! The Assembly's site asks crawlers to wait 20 seconds between requests and
//! the register 10. Three requests go to the Assembly each night; a register
//! page is read nightly only while its bill is before the Assembly, as a
//! bill that has passed or gone no further does not change. Each reading is
//! kept by the bill's register id, as facts and links only: the Assembly's
//! pages are CC BY-NC-ND and the register's CC BY.

use super::{source_name, BillRoll, RollBill};
use crate::endpoints::Endpoints;
use crate::http::{fetch_bytes, fetch_text};
use crate::store::Store;
use anyhow::{anyhow, bail, Context, Result};
use chrono::NaiveDate;
use pollywiki_schema::{StateCode, TimelineStep};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};
use std::sync::LazyLock;

const LIST_PAGE: &str = "/parliamentary-business/in-the-chamber/bills/bills_list";
const VOLUME_PAGE: &str = "/parliamentary-business/in-the-chamber/bills/bills_volume";
const PUBLISHER: &str = "ACT Legislative Assembly";
const CHAMBER: &str = "Legislative Assembly";
/// Register pages as last read, one per bill by its register id.
const READINGS: &str = "raw/states/act/register/";

// A bill's steps, in the Bills List's words where it has them.
const PRESENTED: &str = "Presented";
const REFERRED: &str = "Referred to a committee";
const REPORTED: &str = "Committee reported";
const PASSED: &str = "Passed";
const NOTIFIED: &str = "Act notified";
const NEGATIVED: &str = "Agreement in principle negatived";
const WITHDRAWN: &str = "Withdrawn";
const DISCHARGED: &str = "Discharged";
/// The Bills List runs discharged and withdrawn bills together; the
/// register tells them apart.
const ENDED: &str = "Discharged or withdrawn";
const DIVIDED: &str = "Divided";
const BEFORE: &str = "Before the Assembly";

/// Steps a bill takes once at most, so the register's record of one is the
/// Bills List's, whatever day each gives it.
const ONCE: [&str; 8] = [
    PRESENTED, PASSED, NOTIFIED, NEGATIVED, WITHDRAWN, DISCHARGED, ENDED, DIVIDED,
];

/// Every bill of the current Assembly. The Bills List says where each
/// stands; the register's page for it, where the Bills Volume links one,
/// says who brought it in, links its documents and dates any steps the list
/// leaves out.
pub async fn read(store: &Store, endpoints: &Endpoints, since: &str) -> Result<BillRoll> {
    let assembly = endpoints.opts(20_000);
    let source = format!("{}{LIST_PAGE}", endpoints.act_parliament);
    let page = fetch_text(&source, &assembly).await?;
    let file = docx_link(&page, &endpoints.act_parliament)
        .ok_or_else(|| anyhow!("no Word file of the Bills List linked from {source}"))?;
    let archive = fetch_bytes(&file, &assembly).await?;
    // The list is the current Assembly's own; a date before it began would
    // be the last Assembly's list still up after an election.
    let listed: Vec<Listed> = read_list(&archive)
        .with_context(|| format!("reading {file}"))?
        .into_iter()
        .filter(|bill| bill.presented().is_none_or(|date| date >= since))
        .collect();
    let volume_page = format!("{}{VOLUME_PAGE}", endpoints.act_parliament);
    let volume = volume_entries(&fetch_text(&volume_page, &assembly).await?);
    let titles: Vec<&str> = listed.iter().map(|bill| bill.title.as_str()).collect();
    let found = register_ids(&titles, &volume);

    let mut bills = Vec::with_capacity(listed.len());
    let mut requested = 0;
    for (bill, at) in listed.iter().zip(found) {
        let mut entry = at.map(|at| &volume[at]);
        let mut register = None;
        if let Some(entry) = entry {
            register =
                register_page(store, endpoints, &entry.id, bill.table, &mut requested).await?;
        }
        // The Volume is kept by hand; a link to another bill's page is not
        // taken on trust.
        if let Some(page) = &register {
            if !same_bill(&page.title, &bill.title) {
                eprintln!(
                    "{}: the Bills Volume links \"{}\" to the register's \"{}\"; left unlinked",
                    source_name(StateCode::ACT),
                    bill.title,
                    page.title
                );
                register = None;
                entry = None;
            }
        }
        bills.push(roll_bill(
            bill,
            entry,
            register.as_ref(),
            endpoints,
            &source,
        ));
    }
    println!(
        "{}: {} of {} bills on the register, {requested} of its pages requested",
        source_name(StateCode::ACT),
        bills.iter().filter(|bill| bill.source != source).count(),
        bills.len()
    );
    Ok(BillRoll {
        state: StateCode::ACT,
        listed_at: crate::now_iso(),
        publisher: PUBLISHER.to_string(),
        source,
        bills,
    })
}

/// The Bills List's tables, by what each says of the bills in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum Table {
    Before,
    Passed,
    /// Bills a committee inquired into; each is in one of the others too.
    Committee,
    Negatived,
    /// Discharged or withdrawn.
    Ended,
}

impl Table {
    /// The table a heading names: "Before the Assembly", "Bills Passed",
    /// "Inquired into by Committee", "Agreement in Principle Negatived" or
    /// "Discharged/Withdrawn".
    fn named(heading: &str) -> Option<Table> {
        let heading = heading.to_lowercase();
        let has = |words: &str| heading.contains(words);
        if has("before the assembly") {
            Some(Table::Before)
        } else if has("negatived") {
            Some(Table::Negatived)
        } else if has("discharged") || has("withdrawn") {
            Some(Table::Ended)
        } else if has("committee") {
            Some(Table::Committee)
        } else if has("passed") {
            Some(Table::Passed)
        } else {
            None
        }
    }
}

/// A bill as the Bills List has it.
#[derive(Debug, Clone, PartialEq)]
struct Listed {
    table: Table,
    title: String,
    /// Flagged PMB.
    private_member: bool,
    /// The dated steps its row gives, and the committee table's for it.
    steps: Vec<TimelineStep>,
    act: Option<String>,
    /// The Assembly divided it into bills the list carries in their own
    /// right, and footnotes it where it was, before the Assembly.
    divided: bool,
}

impl Listed {
    fn presented(&self) -> Option<&str> {
        self.steps
            .iter()
            .find(|step| step.event == PRESENTED)
            .map(|step| step.date.as_str())
    }
}

/// Every bill on the Bills List: one from each row of the tables that say
/// where a bill stands, with the dates the committee table gives added to
/// the bills it names. A bill in two of those tables is taken from the
/// later, where it has got further. A table the list did not have before is
/// refused rather than passed over, as a renamed one would take its bills
/// with it.
fn read_list(archive: &[u8]) -> Result<Vec<Listed>> {
    // Every bill's title says it is one; a row of "Nil" does not.
    static BILL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bBill\b").unwrap());
    let mut bills: Vec<Listed> = Vec::new();
    let mut inquiries: Vec<Listed> = Vec::new();
    for rows in docx_tables(archive)? {
        let Some((head, body)) = rows.split_first() else {
            continue;
        };
        let name = head.first().map_or("", |cell| cell.text.as_str());
        if name.is_empty() {
            continue;
        }
        let Some(table) = Table::named(name) else {
            bail!("the Bills List has a table headed \"{name}\", which it did not have before");
        };
        let column = |heading: &str| head.iter().position(|h| squash(&h.text) == squash(heading));
        let need = |heading: &str| {
            column(heading)
                .ok_or_else(|| anyhow!("the Bills List's \"{name}\" table has no {heading} column"))
        };
        let (dated, act): (Vec<(usize, &str)>, Option<usize>) = match table {
            Table::Before => (vec![(need("Introduced")?, PRESENTED)], None),
            Table::Passed => (
                vec![
                    (need("Introduced")?, PRESENTED),
                    (need("Passed")?, PASSED),
                    (need("LR")?, NOTIFIED),
                ],
                Some(need("Act No")?),
            ),
            Table::Negatived => (
                vec![
                    (need("Introduced")?, PRESENTED),
                    (need("Date Negatived")?, NEGATIVED),
                ],
                None,
            ),
            Table::Ended => (
                vec![
                    (need("Introduced")?, PRESENTED),
                    (need("Date Discharged/Withdrawn")?, ENDED),
                ],
                None,
            ),
            // It only adds dates to bills the other tables list, so a change
            // to it costs those dates and no more.
            Table::Committee => match (column("Date of Referral"), column("Report Presented")) {
                (Some(referred), Some(reported)) => {
                    (vec![(referred, REFERRED), (reported, REPORTED)], None)
                }
                _ => continue,
            },
        };
        let flags = column("PMB");
        for row in body {
            let text = |i: usize| row.get(i).map_or("", |cell| cell.text.as_str());
            let title = bill_title(text(0));
            if !BILL.is_match(&title) {
                continue;
            }
            let mut steps: Vec<TimelineStep> = dated
                .iter()
                .filter_map(|&(i, event)| {
                    Some(TimelineStep {
                        date: list_date(text(i))?,
                        event: event.to_string(),
                    })
                })
                .collect();
            let division = match (table, row.first()) {
                (Table::Before, Some(cell)) => cell
                    .notes
                    .iter()
                    .find(|note| note.to_lowercase().contains("divided")),
                _ => None,
            };
            if let Some(date) = division.and_then(|note| text_date(note)) {
                steps.push(TimelineStep {
                    date,
                    event: DIVIDED.to_string(),
                });
            }
            let bill = Listed {
                table,
                title,
                private_member: flags.is_some_and(|f| text(f).to_uppercase().contains("PMB")),
                steps,
                act: act.and_then(|a| act_number(text(a))),
                divided: division.is_some(),
            };
            if table == Table::Committee {
                inquiries.push(bill);
            } else {
                bills.retain(|b| title_key(&b.title) != title_key(&bill.title));
                bills.push(bill);
            }
        }
    }
    for inquiry in inquiries {
        match bills
            .iter_mut()
            .find(|b| title_key(&b.title) == title_key(&inquiry.title))
        {
            Some(bill) => bill.steps.extend(inquiry.steps),
            None => eprintln!(
                "{}: the committee table names \"{}\", which no other table lists",
                source_name(StateCode::ACT),
                inquiry.title
            ),
        }
    }
    Ok(bills)
}

/// A table cell's text, and the text of any footnotes it refers to.
#[derive(Debug, Clone, Default, PartialEq)]
struct Cell {
    text: String,
    notes: Vec<String>,
}

/// One table's rows, each a row of cells.
type Rows = Vec<Vec<Cell>>;

/// The tables of a Word document. A cell that spans columns is followed by
/// empty ones, so every row lines up with its table's headings.
fn docx_tables(archive: &[u8]) -> Result<Vec<Rows>> {
    static TABLE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)<w:tbl[ >].*?</w:tbl>").unwrap());
    static ROW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<w:tr[ >].*?</w:tr>").unwrap());
    static CELL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)<w:tc[ >].*?</w:tc>").unwrap());
    static SKIPPED: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"<w:gridBefore w:val="(\d+)""#).unwrap());
    static SPAN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"<w:gridSpan w:val="(\d+)""#).unwrap());
    static NOTE_MARK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"<w:footnoteReference [^>]*w:id="(-?\d+)""#).unwrap());
    static NOTE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?s)<w:footnote [^>]*w:id="(-?\d+)"[^>]*>(.*?)</w:footnote>"#).unwrap()
    });
    let count = |found: Option<regex::Captures>| {
        found
            .and_then(|n| n[1].parse::<usize>().ok())
            .map(|n| n.min(64))
    };

    let mut zip = zip::ZipArchive::new(Cursor::new(archive))
        .map_err(|e| anyhow!("the Bills List is not a Word document ({e})"))?;
    let xml = docx_part(&mut zip, "word/document.xml")?
        .ok_or_else(|| anyhow!("the Bills List has no document body"))?;
    let footnotes = docx_part(&mut zip, "word/footnotes.xml")?.unwrap_or_default();
    let notes: HashMap<&str, String> = NOTE
        .captures_iter(&footnotes)
        .filter_map(|note| Some((note.get(1)?.as_str(), run_text(&note[2]))))
        .collect();

    Ok(TABLE
        .find_iter(&xml)
        .map(|table| {
            ROW.find_iter(table.as_str())
                .map(|row| {
                    let mut cells =
                        vec![Cell::default(); count(SKIPPED.captures(row.as_str())).unwrap_or(0)];
                    for cell in CELL.find_iter(row.as_str()) {
                        let cell = cell.as_str();
                        cells.push(Cell {
                            text: run_text(cell),
                            notes: NOTE_MARK
                                .captures_iter(cell)
                                .filter_map(|mark| notes.get(&mark[1]).cloned())
                                .collect(),
                        });
                        let span = count(SPAN.captures(cell)).unwrap_or(1).max(1);
                        cells.resize(cells.len() + span - 1, Cell::default());
                    }
                    cells
                })
                .collect()
        })
        .collect())
}

/// One part of a Word file, if it has it.
fn docx_part(zip: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str) -> Result<Option<String>> {
    let Ok(mut part) = zip.by_name(name) else {
        return Ok(None);
    };
    let mut xml = String::new();
    part.read_to_string(&mut xml)
        .with_context(|| format!("reading {name} in the Bills List"))?;
    Ok(Some(xml))
}

/// The text of some Word XML: its runs, with breaks, tabs and paragraph ends
/// read as spaces.
fn run_text(xml: &str) -> String {
    static PIECE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)<w:t(?:\s[^>]*)?>(.*?)</w:t>|<w:br\s*/>|<w:tab\s*/>|</w:p>").unwrap()
    });
    let mut text = String::new();
    for piece in PIECE.captures_iter(xml) {
        match piece.get(1) {
            Some(run) => text.push_str(run.as_str()),
            None => text.push(' '),
        }
    }
    tidy(&decode(&text))
}

/// The current edition's Word file, linked from the Bills List page. The
/// file's address changes with each edition.
fn docx_link(page: &str, base: &str) -> Option<String> {
    static DOCX: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?i)href="([^"?#]+\.docx)[^"]*""#).unwrap());
    DOCX.captures(page)
        .map(|link| absolute(&decode(&link[1]), base))
}

/// A bill's title without the notes the Bills List adds in square brackets,
/// as in "[Act Citation: ... Act 2026]". A bracket without a colon, as in
/// "[No 2]", is how the list tells two bills apart, and stays.
fn bill_title(cell: &str) -> String {
    static NOTE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[[^\]]*:[^\]]*\]").unwrap());
    tidy(&NOTE.replace_all(cell, " "))
}

/// A Bills List date, "06-02-25", as 2025-02-06. The list is one Assembly's,
/// so a two-digit year is this century's.
fn list_date(cell: &str) -> Option<String> {
    static DATE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(\d{1,2})[-/.](\d{1,2})[-/.](\d{2}|\d{4})$").unwrap());
    let date = DATE.captures(cell.trim())?;
    let year: i32 = date[3].parse().ok()?;
    let year = if year < 100 { 2000 + year } else { year };
    day(year, date[2].parse().ok()?, date[1].parse().ok()?)
}

/// The first date written out in a stretch of text, "2 September 2025".
fn text_date(text: &str) -> Option<String> {
    static DATE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\b(\d{1,2}) ([A-Za-z]+) (\d{4})\b").unwrap());
    let date = DATE.captures(text)?;
    let parsed = NaiveDate::parse_from_str(
        &format!("{} {} {}", &date[1], &date[2], &date[3]),
        "%d %B %Y",
    )
    .ok()?;
    Some(parsed.format("%Y-%m-%d").to_string())
}

fn day(year: i32, month: u32, day: u32) -> Option<String> {
    NaiveDate::from_ymd_opt(year, month, day).map(|d| d.format("%Y-%m-%d").to_string())
}

/// An Act's number as the register writes it: the list's "A2026-07" is
/// A2026-7.
fn act_number(cell: &str) -> Option<String> {
    static NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^A?(\d{4})-0*(\d+)$").unwrap());
    let cell = cell.trim();
    if cell.is_empty() {
        return None;
    }
    Some(match NUMBER.captures(cell) {
        Some(number) => format!("A{}-{}", &number[1], &number[2]),
        None => cell.to_string(),
    })
}

/// A bill as the Bills Volume lists it: under a heading linking its
/// register page, with links to its documents.
#[derive(Debug, Clone, PartialEq)]
struct VolumeEntry {
    title: String,
    /// The register's id for it, the NNNNN of db_NNNNN.
    id: String,
    text: Option<String>,
    explanatory: Option<String>,
}

/// The bills the Bills Volume lists. Its document links are checked rather
/// than trusted: one of its explanatory statement links points at the bill.
fn volume_entries(page: &str) -> Vec<VolumeEntry> {
    static HEADING: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)<h4[^>]*>(.*?)</h4>").unwrap());
    static REGISTER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/b/db_(\d+)").unwrap());
    static LINK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?s)<a\s[^>]*?href="([^"]+)"[^>]*>(.*?)</a>"#).unwrap());
    let headings: Vec<_> = HEADING.captures_iter(page).collect();
    let mut entries = Vec::new();
    for (n, heading) in headings.iter().enumerate() {
        // Letters of the alphabet head the bills, linking nothing.
        let Some(id) = REGISTER.captures(&heading[1]) else {
            continue;
        };
        let id = id[1].to_string();
        let start = heading.get(0).map_or(0, |h| h.end());
        let end = headings
            .get(n + 1)
            .and_then(|next| next.get(0))
            .map_or(page.len(), |next| next.start());
        let links: Vec<(String, String)> = LINK
            .captures_iter(&page[start..end])
            .map(|link| (decode(&link[1]), html_text(&link[2])))
            .collect();
        let bill_text = format!("/b/db_{id}/");
        entries.push(VolumeEntry {
            title: html_text(&heading[1]),
            text: links
                .iter()
                .find(|(href, _)| href.contains(&bill_text))
                .map(|(href, _)| href.clone()),
            explanatory: links
                .iter()
                .find(|(href, label)| {
                    href.contains("/es/")
                        && label.to_lowercase().starts_with("explanatory statement")
                })
                .map(|(href, _)| href.clone()),
            id,
        });
    }
    entries
}

/// Each listed bill's entry in the Bills Volume, by title: the same title
/// first, then a near one, and never one register id for two bills. A
/// bill the Volume has not caught up with has none.
fn register_ids(titles: &[&str], volume: &[VolumeEntry]) -> Vec<Option<usize>> {
    let mut found: Vec<Option<usize>> = vec![None; titles.len()];
    let mut taken: HashSet<&str> = HashSet::new();
    for (i, title) in titles.iter().enumerate() {
        let key = title_key(title);
        if let Some(at) = volume
            .iter()
            .position(|v| !taken.contains(v.id.as_str()) && title_key(&v.title) == key)
        {
            taken.insert(&volume[at].id);
            found[i] = Some(at);
        }
    }
    for (i, title) in titles.iter().enumerate() {
        if found[i].is_some() {
            continue;
        }
        let near: Vec<usize> = (0..volume.len())
            .filter(|&at| {
                !taken.contains(volume[at].id.as_str()) && near_title(title, &volume[at].title)
            })
            .collect();
        // One bill, though the Volume may list it twice.
        let ids: HashSet<&str> = near.iter().map(|&at| volume[at].id.as_str()).collect();
        if ids.len() == 1 {
            taken.insert(&volume[near[0]].id);
            found[i] = Some(near[0]);
        }
    }
    found
}

/// Whether two titles are one bill's.
fn same_bill(a: &str, b: &str) -> bool {
    title_key(a) == title_key(b) || near_title(a, b)
}

/// A title as the same title is written anywhere: case, spacing, dashes and
/// curly quotes aside.
fn title_key(title: &str) -> String {
    tidy(
        &title
            .to_lowercase()
            .replace(['\u{2013}', '\u{2014}'], "-")
            .replace(['\u{2018}', '\u{2019}'], "'"),
    )
}

/// Whether two titles are one bill's but for a slip of a letter or two: the
/// Bills Volume has "Office of the Legislation Assembly" for "Legislative",
/// and "Firearm Prohibition Orders" for "Firearms". The numbers in them, the
/// years and any "No 2", must agree exactly.
fn near_title(a: &str, b: &str) -> bool {
    fn words(title: &str) -> String {
        title
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }
    fn numbers(title: &str) -> Vec<&str> {
        title
            .split(|c: char| !c.is_ascii_digit())
            .filter(|number| !number.is_empty())
            .collect()
    }
    numbers(a) == numbers(b) && edit_distance(&words(a), &words(b)) <= 2
}

/// The letters to change, add or drop to make one string the other.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, from) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, to) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (above + 1)
                .min(row[j] + 1)
                .min(diagonal + usize::from(from != *to));
            diagonal = above;
        }
    }
    row[b.len()]
}

/// What a bill's page on the Legislation Register says of it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegisterBill {
    title: String,
    /// "Government Bill", as the register types it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sponsor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    explanatory: Option<String>,
    /// The Act it was notified as, from the progress entry linking it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    act: Option<String>,
    /// Dated progress entries, in the register's own words.
    #[serde(default)]
    progress: Vec<TimelineStep>,
}

/// A register page as last read, and the Bills List table its bill was in
/// then.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Reading {
    listed_as: Table,
    page: RegisterBill,
}

fn register_url(endpoints: &Endpoints, id: &str) -> String {
    format!("{}/b/db_{id}/", endpoints.act_legislation)
}

/// A bill's register page, read afresh while the bill is before the
/// Assembly and once more when the Bills List moves it on; otherwise as last
/// read, since a bill that has passed, or gone no further, does not change.
/// A page that cannot be read is logged, and the last reading stands in.
/// Each request is counted in `requested`.
async fn register_page(
    store: &Store,
    endpoints: &Endpoints,
    id: &str,
    table: Table,
    requested: &mut usize,
) -> Result<Option<RegisterBill>> {
    let key = format!("{READINGS}db_{id}.json");
    // A reading that no longer parses is read again rather than trusted.
    let last = store.get_json::<Reading>(&key).await.ok().flatten();
    if let Some(last) = &last {
        if table != Table::Before && last.listed_as == table {
            return Ok(Some(last.page.clone()));
        }
    }
    let url = register_url(endpoints, id);
    *requested += 1;
    let read = fetch_text(&url, &endpoints.opts(10_000))
        .await
        .map(|page| register_bill(&page, &endpoints.act_legislation));
    match read {
        Ok(page) if !page.title.is_empty() => {
            let reading = Reading {
                listed_as: table,
                page,
            };
            store.put_json(&key, &reading).await?;
            Ok(Some(reading.page))
        }
        failed => {
            let why = match failed {
                Err(err) => err.to_string(),
                Ok(_) => "no bill on the page".to_string(),
            };
            eprintln!("{}: {url}: {why}", source_name(StateCode::ACT));
            Ok(last.map(|last| last.page))
        }
    }
}

/// A register bill page: its type, sponsor and status, the "as presented"
/// bill and explanatory statement, and its progress, each entry dated.
fn register_bill(page: &str, register: &str) -> RegisterBill {
    static TITLE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)<h2(?:\s[^>]*)?>(.*?)</h2>").unwrap());
    static FACT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)<dt(?:\s[^>]*)?>(.*?)</dt>\s*<dd(?:\s[^>]*)?>(.*?)</dd>").unwrap()
    });
    static HEADING: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)<h3(?:\s[^>]*)?>(.*?)</h3>").unwrap());
    static STEP: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)<tr(?:\s[^>]*)?>\s*<td(?:\s[^>]*)?>(.*?)</td>\s*<td(\s[^>]*)?>(.*?)</td>")
            .unwrap()
    });
    static ORDER: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"data-order="(\d{4})(\d{2})(\d{2})"#).unwrap());
    static ACT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"href="[^"]*/a/(\d{4}-\d+)/?""#).unwrap());

    // The bill is the page's article; the rest is the site around it.
    let article = page.split_once("<article").map_or(page, |(_, rest)| rest);
    let article = article
        .split_once("</article>")
        .map_or(article, |(bill, _)| bill);
    let mut bill = RegisterBill {
        title: TITLE
            .captures(article)
            .map(|title| html_text(&title[1]))
            .unwrap_or_default(),
        ..Default::default()
    };
    for fact in FACT.captures_iter(article) {
        let value = Some(html_text(&fact[2])).filter(|value| !value.is_empty());
        match html_text(&fact[1]).to_lowercase().as_str() {
            "type" => bill.kind = value,
            "sponsor" => bill.sponsor = value,
            "status" => bill.status = value,
            _ => {}
        }
    }
    let headings: Vec<_> = HEADING.captures_iter(article).collect();
    for (n, heading) in headings.iter().enumerate() {
        let start = heading.get(0).map_or(0, |h| h.end());
        let end = headings
            .get(n + 1)
            .and_then(|next| next.get(0))
            .map_or(article.len(), |next| next.start());
        let section = &article[start..end];
        let name = html_text(&heading[1]).to_lowercase();
        if name.starts_with("presented bill") {
            bill.text = version_link(section, register);
        } else if name.starts_with("explanatory statement") {
            bill.explanatory = version_link(section, register);
        } else if name.starts_with("bill progress") {
            for step in STEP.captures_iter(section) {
                let attributes = step.get(2).map_or("", |a| a.as_str());
                let date = ORDER
                    .captures(attributes)
                    .and_then(|d| day(d[1].parse().ok()?, d[2].parse().ok()?, d[3].parse().ok()?))
                    .or_else(|| text_date(&html_text(&step[3])));
                let Some(date) = date else {
                    continue;
                };
                if let Some(act) = ACT.captures(&step[1]) {
                    bill.act = act_number(&act[1]);
                }
                bill.progress.push(TimelineStep {
                    date,
                    event: html_text(&step[1]),
                });
            }
        }
    }
    bill
}

/// A register document's "as presented" version, or failing that its first:
/// the PDF where there is one, else the Word file, else the page to read it
/// online. Only the register's own links count; a row's notes link Hansard
/// and committee reports.
fn version_link(section: &str, register: &str) -> Option<String> {
    static ROW: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)<tr(?:\s[^>]*)?>(.*?)</tr>").unwrap());
    static FIRST_CELL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)<td(?:\s[^>]*)?>(.*?)</td>").unwrap());
    static HREF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"href="([^"]+)""#).unwrap());
    // Each version, by name, with its links on the register.
    let versions: Vec<(String, Vec<String>)> = ROW
        .captures_iter(section)
        .map(|row| {
            let name = FIRST_CELL
                .captures(&row[1])
                .map(|cell| html_text(&cell[1]).to_lowercase())
                .unwrap_or_default();
            let links = HREF
                .captures_iter(&row[1])
                .map(|href| decode(&href[1]))
                .filter(|href| href.contains("/DownloadFile/") || href.contains("/View/"))
                .collect();
            (name, links)
        })
        .filter(|(_, links): &(String, Vec<String>)| !links.is_empty())
        .collect();
    let (_, links) = versions
        .iter()
        .find(|(name, _)| name.starts_with("as presented"))
        .or(versions.first())?;
    let download = |link: &&String| link.contains("/DownloadFile/");
    let pick = links
        .iter()
        .find(|link| download(link) && link.to_lowercase().ends_with(".pdf"))
        .or_else(|| links.iter().find(download))
        .or(links.first())?;
    Some(absolute(pick, register))
}

/// One bill for the roll: its row on the Bills List, its entry in the Bills
/// Volume and what its register page says, where it has them.
fn roll_bill(
    listed: &Listed,
    entry: Option<&VolumeEntry>,
    register: Option<&RegisterBill>,
    endpoints: &Endpoints,
    list_page: &str,
) -> RollBill {
    let progress: Vec<TimelineStep> = register
        .map(|page| {
            page.progress
                .iter()
                .map(|step| TimelineStep {
                    date: step.date.clone(),
                    event: register_event(&step.event),
                })
                .collect()
        })
        .unwrap_or_default();
    let said = |event: &str| {
        progress.iter().any(|step| step.event == event)
            || register
                .and_then(|page| page.status.as_deref())
                .is_some_and(|status| register_event(status) == event)
    };
    let (status, stage, stopped) = standing(listed, &said);

    let mut timeline: Vec<TimelineStep> = Vec::new();
    let steps = listed.steps.iter().map(|step| TimelineStep {
        date: step.date.clone(),
        event: match step.event.as_str() {
            ENDED => status.to_string(),
            event => event.to_string(),
        },
    });
    for step in steps.chain(progress.iter().cloned()) {
        let seen = timeline.iter().any(|known| {
            known.event == step.event
                && (known.date == step.date || ONCE.contains(&known.event.as_str()))
        });
        if !seen {
            timeline.push(step);
        }
    }
    timeline.sort_by(|a, b| a.date.cmp(&b.date));

    RollBill {
        title: listed.title.clone(),
        number: None,
        origin: Some(CHAMBER.to_string()),
        sponsor: register.and_then(|page| page.sponsor.clone()),
        kind: kind(listed, register),
        status: status.to_string(),
        stage,
        stopped,
        timeline,
        source: entry
            .map(|entry| register_url(endpoints, &entry.id))
            .unwrap_or_else(|| list_page.to_string()),
        text: register
            .and_then(|page| page.text.clone())
            .or_else(|| entry.and_then(|entry| entry.text.clone())),
        explanatory: register
            .and_then(|page| page.explanatory.clone())
            .or_else(|| entry.and_then(|entry| entry.explanatory.clone())),
        act: listed
            .act
            .clone()
            .or_else(|| register.and_then(|page| page.act.clone())),
    }
}

/// Where a bill stands, as its status, its stage and whether it went no
/// further. The Bills List's table says, but the list comes out once a
/// sitting period and the register is kept day by day, so a step the
/// register has and the list has not caught up with moves a bill on. The
/// ACT has no assent: a passed bill becomes law when it is notified on the
/// register, stage 4.
fn standing(listed: &Listed, said: &dyn Fn(&str) -> bool) -> (&'static str, u8, bool) {
    let notified = listed.steps.iter().any(|step| step.event == NOTIFIED) || said(NOTIFIED);
    if listed.divided {
        return (DIVIDED, 1, true);
    }
    match listed.table {
        Table::Negatived => (NEGATIVED, 1, true),
        Table::Ended => (
            [WITHDRAWN, DISCHARGED]
                .into_iter()
                .find(|event| said(event))
                .unwrap_or(ENDED),
            1,
            true,
        ),
        // Notified on the register: it is an Act now, not merely passed.
        _ if notified => (NOTIFIED, 4, false),
        Table::Passed => (PASSED, 2, false),
        Table::Before | Table::Committee => {
            if said(PASSED) {
                (PASSED, 2, false)
            } else if let Some(stop) = [NEGATIVED, WITHDRAWN, DISCHARGED]
                .into_iter()
                .find(|event| said(event))
            {
                (stop, 1, true)
            } else {
                (BEFORE, 1, false)
            }
        }
    }
}

/// A register progress entry in the words of the Bills List's steps, where
/// it is one of them: "Presented by Treasurer" is the bill presented, "Act
/// notified, see ..." the Act notified. Anything else stands as written.
fn register_event(event: &str) -> String {
    let lower = event.to_lowercase();
    let step = if lower.starts_with("presented") {
        PRESENTED
    } else if lower.starts_with("passed") {
        PASSED
    } else if lower.starts_with("act notified") {
        NOTIFIED
    } else if lower.contains("negatived") {
        NEGATIVED
    } else if lower.starts_with("withdrawn") {
        WITHDRAWN
    } else if lower.starts_with("discharged") {
        DISCHARGED
    } else {
        return event.to_string();
    };
    step.to_string()
}

/// "Private member's" for a bill the list flags PMB, else the register's
/// type in the words the other parliaments' kinds use.
fn kind(listed: &Listed, register: Option<&RegisterBill>) -> Option<String> {
    let typed = register
        .and_then(|page| page.kind.as_deref())
        .unwrap_or("")
        .to_lowercase();
    if listed.private_member || typed.contains("private member") {
        Some("Private member's".to_string())
    } else if typed.contains("government") {
        Some("Government".to_string())
    } else {
        None
    }
}

/// A heading as its letters and digits alone, so the list's "Date
/// Discharged/ Withdrawn" is found by "Date Discharged/Withdrawn".
fn squash(heading: &str) -> String {
    heading
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// A link as an address: as it is with a host, else under the site's root.
fn absolute(href: &str, base: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        href.to_string()
    } else {
        format!("{base}/{}", href.trim_start_matches('/'))
    }
}

/// The words of some HTML: tags gone, entities decoded, spacing tidied.
fn html_text(fragment: &str) -> String {
    static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]*>").unwrap());
    tidy(&decode(&TAG.replace_all(fragment, " ")))
}

/// Character references decoded, as Word and the two sites write them.
fn decode(text: &str) -> String {
    static ENTITY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"&(#[0-9]+|#[xX][0-9a-fA-F]+|[a-zA-Z]+);").unwrap());
    ENTITY
        .replace_all(text, |entity: &regex::Captures| {
            let name = &entity[1];
            let code = match name.strip_prefix('#') {
                Some(hex) if hex.starts_with(['x', 'X']) => u32::from_str_radix(&hex[1..], 16).ok(),
                Some(decimal) => decimal.parse().ok(),
                None => None,
            };
            if let Some(c) = code.and_then(char::from_u32) {
                return c.to_string();
            }
            match name {
                "amp" => "&",
                "lt" => "<",
                "gt" => ">",
                "quot" => "\"",
                "apos" => "'",
                "nbsp" => " ",
                "lsquo" => "\u{2018}",
                "rsquo" => "\u{2019}",
                "ldquo" => "\u{201c}",
                "rdquo" => "\u{201d}",
                "ndash" => "\u{2013}",
                "mdash" => "\u{2014}",
                _ => return entity[0].to_string(),
            }
            .to_string()
        })
        .into_owned()
}

fn tidy(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::LocalStore;
    use crate::test_http::{Response, TestServer};
    use std::io::Write;

    fn step(date: &str, event: &str) -> TimelineStep {
        TimelineStep {
            date: date.to_string(),
            event: event.to_string(),
        }
    }

    /// A Word file of tables and footnotes. In a cell, "^n" refers to
    /// footnote n, and a closing "|n" spans n columns.
    fn docx(tables: &[&[&[&str]]], footnotes: &[(u32, &str)]) -> Vec<u8> {
        let cell = |text: &str| {
            let (text, span) = match text.rsplit_once('|') {
                Some((text, n)) => (text, format!("<w:gridSpan w:val=\"{n}\"/>")),
                None => (text, String::new()),
            };
            let (text, note) = match text.split_once('^') {
                Some((text, n)) => (
                    text,
                    format!("<w:r><w:rPr/><w:footnoteReference w:id=\"{n}\"/></w:r>"),
                ),
                None => (text, String::new()),
            };
            format!(
                "<w:tc><w:tcPr><w:tcW w:w=\"1\"/>{span}</w:tcPr><w:p><w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r>{note}</w:p></w:tc>"
            )
        };
        let body: String = tables
            .iter()
            .map(|rows| {
                let rows: String = rows
                    .iter()
                    .map(|row| {
                        format!(
                            "<w:tr w:rsidR=\"1\"><w:trPr><w:tblHeader/></w:trPr>{}</w:tr>",
                            row.iter().map(|c| cell(c)).collect::<String>()
                        )
                    })
                    .collect();
                format!("<w:tbl><w:tblPr/><w:tblGrid/>{rows}</w:tbl><w:p/>")
            })
            .collect();
        let notes: String = footnotes
            .iter()
            .map(|(id, text)| {
                format!("<w:footnote w:id=\"{id}\"><w:p><w:r><w:footnoteRef/></w:r><w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r></w:p></w:footnote>")
            })
            .collect();
        let mut out = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut out);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (name, xml) in [
                (
                    "word/document.xml",
                    format!("<w:document><w:body>{body}</w:body></w:document>"),
                ),
                (
                    "word/footnotes.xml",
                    format!("<w:footnotes><w:footnote w:type=\"separator\" w:id=\"-1\"><w:p/></w:footnote>{notes}</w:footnotes>"),
                ),
            ] {
                zip.start_file(name, options).expect("start");
                zip.write_all(xml.as_bytes()).expect("write");
            }
            zip.finish().expect("finish");
        }
        out.into_inner()
    }

    const DIVISION: &str = "This bill was divided by Assembly motion on 2 September 2025 (see Minutes of Proceedings) into Workplace Legislation Amendment Bill 2025 (No 2) and Workplace Legislation Amendment Bill 2025 (No 3).";

    #[test]
    fn list_dates_and_act_numbers_read_as_the_register_writes_them() {
        assert_eq!(list_date("06-02-25").as_deref(), Some("2025-02-06"));
        assert_eq!(list_date(" 2-9-26 ").as_deref(), Some("2026-09-02"));
        assert_eq!(list_date("02-10-2026").as_deref(), Some("2026-10-02"));
        assert_eq!(list_date("30-02-26"), None, "no such day");
        assert_eq!(list_date(""), None);
        assert_eq!(text_date(DIVISION).as_deref(), Some("2025-09-02"));
        assert_eq!(
            text_date("27 March 2025 (11th Assembly)").as_deref(),
            Some("2025-03-27")
        );
        assert_eq!(act_number("A2026-07").as_deref(), Some("A2026-7"));
        assert_eq!(act_number(" A2025-26 ").as_deref(), Some("A2025-26"));
        assert_eq!(act_number(""), None);
    }

    #[test]
    fn the_word_file_is_found_on_the_bills_list_page() {
        let page = r#"<td><a href="https://example.org/__data/assets/pdf_file/0009/1/Report-Bills-List-18-30-September-2026.pdf">PDF (325 KB)</a></td>
            <td><a href="https://example.org/__data/assets/word_doc/0010/2/Report-Bills-List-18-1-October-2026.docx">Word (264KB)</a></td>"#;
        assert_eq!(
            docx_link(page, "https://example.org").as_deref(),
            Some("https://example.org/__data/assets/word_doc/0010/2/Report-Bills-List-18-1-October-2026.docx")
        );
        assert_eq!(
            docx_link(
                r#"<a href="/__data/assets/word_doc/1/List.DOCX?v=2">Word</a>"#,
                "https://example.org"
            )
            .as_deref(),
            Some("https://example.org/__data/assets/word_doc/1/List.DOCX"),
            "a link from the root, its query dropped"
        );
        assert_eq!(docx_link("<p>No list yet</p>", "https://example.org"), None);
    }

    #[test]
    fn the_bills_list_gives_each_bill_once_from_its_five_tables() {
        let file = docx(
            &[
                &[
                    &["Before the Assembly", "PMB * ++", "Introduced", "|2"],
                    &["Domestic Violence Agencies Amendment Bill 2026", "", "24-09-26"],
                    &["Electoral (Gambling Industry) Amendment Bill 2026", "PMB ++", "11-06-26"],
                    &["Workplace Legislation Amendment Bill 2025 [No 2]^1", "", "02-09-25"],
                    &["", "", ""],
                ],
                &[
                    // The flags heading spans two columns, as a row's flags
                    // and an empty cell do.
                    &["Bills Passed", "PMB * ++|2", "Introduced", "Passed", "Act No", "LR"],
                    &["Appropriation Bill 2024-2025 (No 2)", "++", "", "06-02-25", "20-03-25", "A2025-7", "27-03-25"],
                    &["Civil Law (Wrongs) Amendment Bill 2026 [Act Citation: Civil Law (Wrongs) Amendment Act 2026]", "*", "", "17-03-26", "06-05-26", "A2026-07", "15-05-26"],
                    &["Fuel Legislation Amendment Bill 2026", "", "", "05-05-26", "07-05-26", "", ""],
                ],
                &[
                    &["Inquired into by Committee", "PMB *", "Introduced", "Date of Referral", "Report Presented"],
                    &["Appropriation Bill 2024-2025 (No 2)", "++", "06-02-25", "06-02-25", "13-03-25"],
                    &["Electoral (Gambling Industry) Amendment Bill 2026", "PMB ++", "11-06-26", "11-06-26", ""],
                ],
                &[
                    &["Agreement in Principle Negatived", "PMB * ++", "Introduced", "Date Negatived"],
                    &["Nil", "", "", ""],
                    &["Planning (Ainslie Volcanics) Amendment Bill 2025", "PMB", "20-03-25", "08-04-25"],
                ],
                &[
                    &["Discharged/Withdrawn", "PMB * ++", "Introduced", "Date Discharged/ Withdrawn"],
                    &["Residential Tenancies (Posting Termination) Amendment Bill 2025", "PMB", "06-05-25", "26-03-26"],
                ],
            ],
            &[(1, DIVISION)],
        );
        let bills = read_list(&file).expect("read");
        let rows: Vec<(Table, &str, bool)> = bills
            .iter()
            .map(|b| (b.table, b.title.as_str(), b.private_member))
            .collect();
        assert_eq!(
            rows,
            [
                (
                    Table::Before,
                    "Domestic Violence Agencies Amendment Bill 2026",
                    false
                ),
                (
                    Table::Before,
                    "Electoral (Gambling Industry) Amendment Bill 2026",
                    true
                ),
                (
                    Table::Before,
                    "Workplace Legislation Amendment Bill 2025 [No 2]",
                    false
                ),
                (Table::Passed, "Appropriation Bill 2024-2025 (No 2)", false),
                (
                    Table::Passed,
                    "Civil Law (Wrongs) Amendment Bill 2026",
                    false
                ),
                (Table::Passed, "Fuel Legislation Amendment Bill 2026", false),
                (
                    Table::Negatived,
                    "Planning (Ainslie Volcanics) Amendment Bill 2025",
                    true
                ),
                (
                    Table::Ended,
                    "Residential Tenancies (Posting Termination) Amendment Bill 2025",
                    true
                ),
            ],
            "the committee's bills once each; no blank or \"Nil\" row, no Act citation"
        );
        assert_eq!(
            bills[1].steps,
            [step("2026-06-11", PRESENTED), step("2026-06-11", REFERRED)]
        );
        assert!(bills[2].divided, "footnoted as divided");
        assert_eq!(
            bills[2].steps,
            [step("2025-09-02", PRESENTED), step("2025-09-02", DIVIDED)]
        );
        assert_eq!(
            bills[3].steps,
            [
                step("2025-02-06", PRESENTED),
                step("2025-03-20", PASSED),
                step("2025-03-27", NOTIFIED),
                step("2025-02-06", REFERRED),
                step("2025-03-13", REPORTED),
            ],
            "dates read from the columns their headings name"
        );
        assert_eq!(bills[3].act.as_deref(), Some("A2025-7"));
        assert_eq!(bills[4].act.as_deref(), Some("A2026-7"), "no leading zero");
        assert_eq!(bills[5].act, None, "passed, not yet notified");
        assert_eq!(
            bills[5].steps,
            [step("2026-05-05", PRESENTED), step("2026-05-07", PASSED)]
        );
        assert_eq!(
            bills[6].steps,
            [step("2025-03-20", PRESENTED), step("2025-04-08", NEGATIVED)]
        );
        assert_eq!(
            bills[7].steps,
            [step("2025-05-06", PRESENTED), step("2026-03-26", ENDED)]
        );
    }

    #[test]
    fn a_list_that_changed_shape_is_refused() {
        let file = docx(
            &[&[
                &["Bills Passed", "PMB * ++", "Introduced", "Passed", "Act No"],
                &["Example Bill 2026", "", "05-05-26", "07-05-26", "A2026-1"],
            ]],
            &[],
        );
        let err = read_list(&file).expect_err("no LR column");
        assert!(err.to_string().contains("no LR column"), "{err}");

        let file = docx(
            &[&[
                &["Acts", "PMB * ++", "Introduced", "Passed", "Act No", "LR"],
                &[
                    "Example Bill 2026",
                    "",
                    "05-05-26",
                    "07-05-26",
                    "A2026-1",
                    "15-05-26",
                ],
            ]],
            &[],
        );
        let err = read_list(&file).expect_err("a table renamed");
        assert!(err.to_string().contains("headed \"Acts\""), "{err}");

        let not_word = read_list(b"<html>Service unavailable</html>").expect_err("not a zip");
        assert!(
            not_word.to_string().contains("not a Word document"),
            "{not_word}"
        );
    }

    const VOLUME: &str = r#"<h4 style="text-align: center;"><strong>A</strong></h4>
        <h4><a href="https://www.legislation.act.gov.au/b/db_72727">Appropriation Bill 2025-2026</a></h4><ul>
          <li><a href="https://www.legislation.act.gov.au/View/b/db_72727/20250624-87460/html/db_72727.html">As presented</a></li>
          <li><a href="https://www.legislation.act.gov.au/View/b/db_72727/20250624-87460/html/db_72727.html">Explanatory statement, including Compatibility Statement for Human Rights Act 2004</a></li>
          <li><a href="https://www.legislation.act.gov.au/View/es/db_72744/20250923-88214/html/db_72744.html">Supplementary Statement</a></li></ul>
        <h4><strong><a href="https://www.legislation.act.gov.au/b/db_72728">Appropriation (Office of the Legislation Assembly) Bill 2025-2026</a></strong></h4><ul>
          <li>As <a href="https://www.legislation.act.gov.au/View/b/db_72728/20250624-87461/html/db_72728.html">presented</a></li>
          <li><a href="https://www.legislation.act.gov.au/View/es/db_72745/20250624-87466/html/db_72745.html">Explanatory statement, including Compatibility Statement for Human Rights Act 2004</a></li></ul>
        <h4 style="text-align: center;"><strong>L</strong></h4>
        <h4><a href="https://www.legislation.act.gov.au/b/db_73536">Liquor Amendment Bill 2025</a></h4>
        <h4 style="text-align: center;"><strong>W</strong></h4>
        <h4><a href="https://www.legislation.act.gov.au/b/db_73536">Liquor Amendment Bill 2025</a></h4>
        <h4><a href="https://www.legislation.act.gov.au/b/db_73333">Workplace Legislation Amendment Bill 2025 (No 2)</a></h4>
        <h4><a href="https://www.legislation.act.gov.au/b/db_73335">Workplace Legislation Amendment Bill 2025 (No 3)</a></h4>
        <h4>Acknowledgement of Country</h4><p>We acknowledge <a href="https://example.org/country">Country</a>.</p>"#;

    #[test]
    fn titles_find_their_register_ids_in_the_bills_volume() {
        let volume = volume_entries(VOLUME);
        assert_eq!(volume.len(), 6, "letter headings link no bill");
        assert_eq!(
            volume[0].text.as_deref(),
            Some("https://www.legislation.act.gov.au/View/b/db_72727/20250624-87460/html/db_72727.html")
        );
        assert_eq!(
            volume[0].explanatory, None,
            "its explanatory statement link is the bill; the supplementary statement is not the statement"
        );
        assert_eq!(
            volume[1].text.as_deref(),
            Some("https://www.legislation.act.gov.au/View/b/db_72728/20250624-87461/html/db_72728.html")
        );
        assert!(volume[1]
            .explanatory
            .as_deref()
            .is_some_and(|link| link.contains("/View/es/db_72745/")));

        let titles = [
            "Workplace Legislation Amendment Bill 2025 [No 2]",
            "Appropriation (Office of the Legislative Assembly) Bill 2025-2026",
            "Appropriation Bill 2025-2026",
            "Liquor Amendment Bill 2025",
            "Workplace Legislation Amendment Bill 2025 (No 2)",
            "Competition Legislation Amendment Bill 2026",
            "Appropriation Bill 2026-2027",
        ];
        let ids: Vec<Option<&str>> = register_ids(&titles, &volume)
            .into_iter()
            .map(|at| at.map(|at| volume[at].id.as_str()))
            .collect();
        assert_eq!(
            ids,
            [
                None,
                Some("72728"),
                Some("72727"),
                Some("73536"),
                Some("73333"),
                None,
                None
            ],
            "a near title is the bill, but not another's \"(No 2)\", nor another year's"
        );
    }

    #[test]
    fn titles_are_one_bill_s_despite_a_slipped_letter_but_not_another_number() {
        assert!(same_bill(
            "Firearms (Firearms Prohibition Orders) Amendment Bill 2026",
            "Firearms (Firearm Prohibition Orders) Amendment Bill 2026"
        ));
        assert!(same_bill(
            "Building and Construction Legislation Amendment Bill 2025 (No 2)",
            "building and  construction legislation amendment bill 2025 (No 2)"
        ));
        assert!(!same_bill(
            "Health Legislation Amendment Bill 2025",
            "Health Legislation Amendment Bill 2025 (No 2)"
        ));
        assert!(!same_bill(
            "Crimes Legislation Amendment Bill 2025",
            "Civil Law (Wrongs) Amendment Bill 2025"
        ));
        assert_eq!(edit_distance("legislative", "legislation"), 2);
        assert_eq!(edit_distance("", "abc"), 3);
    }

    /// The register's page for a bill that passed, trimmed to the parts read.
    const PASSED_PAGE: &str = r#"<header><h2>Menu</h2></header>
<article class="wrap" aria-label="Bills: No year/number">
  <h2>Appropriation Bill 2024-2025 (No 2)</h2>
  <dl><dt>Type</dt><dd>Government Bill</dd><dt>Sponsor</dt><dd>Treasurer</dd><dt>Status</dt><dd>Passed</dd></dl>
  <form id="item-search" action="/results" method="post"><input type="hidden" name="sVersion" value="\b\db_71099\current"></form>
  <h3 tabindex="0">Presented bill</h3>
  <table class="datatable display"><thead><tr><th>Version</th><th>Published</th><th>Files/Formats</th><th>Notes</th></tr></thead><tbody>
    <tr><td>As presented</td><td data-order="20250206000000AM">6 February 2025</td>
      <td><a class="button viewable html" href="/View/b/db_71099/20250206-85638/html/db_71099.html">HTML</a><a class="button download pdf" href="/DownloadFile/b/db_71099/20250206-85638/PDF/db_71099.PDF">PDF</a><a class="button download doc" href="/DownloadFile/b/db_71099/20250206-85638/DOCX/db_71099.DOCX">Word</a></td>
      <td class="notes"><p><a href="https://www.parliament.act.gov.au/__data/assets/pdf_file/0008/2815541/Scrutiny-Report-2-Final.pdf">Scrutiny Committee report</a></p></td></tr>
  </tbody></table>
  <h3>Explanatory statement</h3>
  <table class="datatable display"><thead><tr><th>Version</th><th>Published</th><th>Files/Formats</th></tr></thead><tbody>
    <tr><td>Supplementary</td><td data-order="20250319000000AM">19 March 2025</td>
      <td><a href="/DownloadFile/es/db_71120/20250319-85700/PDF/db_71120.PDF">PDF</a></td></tr>
    <tr><td>As presented (includes compatibility statement)</td><td data-order="20250206000000AM">6 February 2025</td>
      <td><a href="/View/es/db_71120/20250206-85643/html/db_71120.html">HTML</a><a href="/DownloadFile/es/db_71120/20250206-85643/PDF/db_71120.PDF">PDF</a></td></tr>
  </tbody></table>
  <section id="history" class="panel hidden"><h3 tabindex="0">Bill progress</h3>
    <table id="bill-progress-table" class="datatable display"><thead><tr><th scope="col">Event</th><th scope="col" class="th-period">Date</th></tr></thead><tbody>
      <tr>
        <td>Presented by Treasurer </td>
        <td data-order="20250206000000AM">6 February 2025 (11th Assembly)</td>
      </tr>
      <tr><td>Passed</td><td data-order="20250320000000AM">20 March 2025</td></tr>
      <tr>

        <td>Act notified, see <a href="/a/2025-7/">Appropriation Act 2024-2025 (No 2)</a></td>
        <td data-order="20250327000000AM">27 March 2025</td>
      </tr>
    </tbody></table>
  </section>
</article>
<footer><h3>Contact us</h3></footer>"#;

    #[test]
    fn a_register_page_gives_type_sponsor_documents_and_progress() {
        let bill = register_bill(PASSED_PAGE, "https://www.legislation.act.gov.au");
        assert_eq!(
            bill,
            RegisterBill {
                title: "Appropriation Bill 2024-2025 (No 2)".to_string(),
                kind: Some("Government Bill".to_string()),
                sponsor: Some("Treasurer".to_string()),
                status: Some("Passed".to_string()),
                text: Some("https://www.legislation.act.gov.au/DownloadFile/b/db_71099/20250206-85638/PDF/db_71099.PDF".to_string()),
                explanatory: Some("https://www.legislation.act.gov.au/DownloadFile/es/db_71120/20250206-85643/PDF/db_71120.PDF".to_string()),
                act: Some("A2025-7".to_string()),
                progress: vec![
                    step("2025-02-06", "Presented by Treasurer"),
                    step("2025-03-20", "Passed"),
                    step(
                        "2025-03-27",
                        "Act notified, see Appropriation Act 2024-2025 (No 2)"
                    ),
                ],
            }
        );
        assert_eq!(
            register_bill("<html><p>Not found</p></html>", "https://x"),
            RegisterBill::default(),
            "a page with no bill reads as none"
        );
    }

    #[test]
    fn where_a_bill_stands_follows_its_table_and_news_on_the_register() {
        let endpoints = Endpoints::at("http://127.0.0.1:1");
        let listed = |table: Table, steps: &[(&str, &str)]| Listed {
            table,
            title: "Example Bill 2026".to_string(),
            private_member: false,
            steps: steps
                .iter()
                .map(|(date, event)| step(date, event))
                .collect(),
            act: None,
            divided: false,
        };
        let register = |status: &str, progress: &[(&str, &str)]| RegisterBill {
            title: "Example Bill 2026".to_string(),
            status: Some(status.to_string()),
            progress: progress
                .iter()
                .map(|(date, event)| step(date, event))
                .collect(),
            ..Default::default()
        };
        let read = |listed: &Listed, register: Option<&RegisterBill>| {
            roll_bill(listed, None, register, &endpoints, "https://list")
        };
        let stands = |bill: &RollBill| (bill.status.clone(), bill.stage, bill.stopped);
        let presented = [("2026-09-24", PRESENTED)];

        let before = read(&listed(Table::Before, &presented), None);
        assert_eq!(stands(&before), (BEFORE.to_string(), 1, false));
        assert_eq!(before.source, "https://list", "no register page known");

        // Passed since the list was published, and notified since.
        let news = register(
            "Passed",
            &[
                ("2026-09-24", "Presented by Treasurer"),
                ("2026-10-21", "Referred to Standing Committee on Justice"),
                ("2026-10-22", "Passed"),
            ],
        );
        let passed = read(&listed(Table::Before, &presented), Some(&news));
        assert_eq!(stands(&passed), (PASSED.to_string(), 2, false));
        assert_eq!(
            passed.timeline,
            [
                step("2026-09-24", PRESENTED),
                step("2026-10-21", "Referred to Standing Committee on Justice"),
                step("2026-10-22", PASSED),
            ],
            "the register's entries join the list's once each, oldest first"
        );
        let mut notified = news.clone();
        notified
            .progress
            .push(step("2026-10-29", "Act notified, see Example Act 2026"));
        notified.act = Some("A2026-20".to_string());
        let act = read(&listed(Table::Before, &presented), Some(&notified));
        assert_eq!(stands(&act), (NOTIFIED.to_string(), 4, false));
        assert_eq!(act.act.as_deref(), Some("A2026-20"));

        let on_list = read(
            &listed(
                Table::Passed,
                &[("2026-05-05", PRESENTED), ("2026-05-07", PASSED)],
            ),
            None,
        );
        assert_eq!(stands(&on_list), (PASSED.to_string(), 2, false));
        let notified_on_list = read(
            &listed(
                Table::Passed,
                &[
                    ("2026-05-05", PRESENTED),
                    ("2026-05-07", PASSED),
                    ("2026-05-15", NOTIFIED),
                ],
            ),
            None,
        );
        assert_eq!(stands(&notified_on_list), (NOTIFIED.to_string(), 4, false));

        let negatived = read(&listed(Table::Negatived, &presented), None);
        assert_eq!(stands(&negatived), (NEGATIVED.to_string(), 1, true));

        let ended = listed(
            Table::Ended,
            &[("2025-05-06", PRESENTED), ("2026-03-26", ENDED)],
        );
        let discharged = read(&ended, Some(&register("Discharged", &[])));
        assert_eq!(stands(&discharged), (DISCHARGED.to_string(), 1, true));
        assert_eq!(
            discharged.timeline,
            [
                step("2025-05-06", PRESENTED),
                step("2026-03-26", DISCHARGED)
            ]
        );
        assert_eq!(
            stands(&read(&ended, None)),
            (ENDED.to_string(), 1, true),
            "the list alone does not say which"
        );

        let mut divided = listed(
            Table::Before,
            &[("2025-09-02", PRESENTED), ("2025-09-02", DIVIDED)],
        );
        divided.divided = true;
        assert_eq!(
            stands(&read(&divided, None)),
            (DIVIDED.to_string(), 1, true)
        );
    }

    /// A register bill page in the register's layout.
    fn register_html(
        id: &str,
        title: &str,
        kind: &str,
        sponsor: &str,
        status: &str,
        progress: &[(&str, &str)],
    ) -> String {
        let steps: String = progress
            .iter()
            .map(|(event, date)| {
                format!("<tr><td>{event}</td><td data-order=\"{date}000000AM\">{date}</td></tr>")
            })
            .collect();
        format!(
            r#"<article class="wrap"><h2>{title}</h2>
            <dl><dt>Type</dt><dd>{kind}</dd><dt>Sponsor</dt><dd>{sponsor}</dd><dt>Status</dt><dd>{status}</dd></dl>
            <h3 tabindex="0">Presented bill</h3><table><tbody><tr><td>As presented</td><td>x</td>
              <td><a href="/View/b/db_{id}/current/html/db_{id}.html">HTML</a><a href="/DownloadFile/b/db_{id}/current/PDF/db_{id}.PDF">PDF</a></td></tr></tbody></table>
            <h3>Explanatory statement</h3><table><tbody><tr><td>As presented</td><td>x</td>
              <td><a href="/DownloadFile/es/db_{id}9/current/PDF/db_{id}9.PDF">PDF</a></td></tr></tbody></table>
            <h3 tabindex="0">Bill progress</h3><table><tbody>{steps}</tbody></table></article>"#
        )
    }

    #[tokio::test]
    async fn the_roll_reads_the_list_the_volume_and_each_register_page_it_needs() {
        let file = docx(
            &[
                &[
                    &["Before the Assembly", "PMB * ++", "Introduced"],
                    &[
                        "Domestic Violence Agencies Amendment Bill 2026",
                        "",
                        "24-09-26",
                    ],
                    &[
                        "Housing and Consumer Affairs Legislation Amendment Bill 2026",
                        "",
                        "17-09-26",
                    ],
                ],
                &[
                    &[
                        "Bills Passed",
                        "PMB * ++",
                        "Introduced",
                        "Passed",
                        "Act No",
                        "LR",
                    ],
                    &[
                        "Appropriation Bill 2024-2025 (No 2)",
                        "++",
                        "06-02-25",
                        "20-03-25",
                        "A2025-7",
                        "27-03-25",
                    ],
                    &[
                        "Last Assembly's Bill 2024",
                        "",
                        "01-08-24",
                        "08-08-24",
                        "A2024-30",
                        "15-08-24",
                    ],
                ],
                &[
                    &[
                        "Agreement in Principle Negatived",
                        "PMB * ++",
                        "Introduced",
                        "Date Negatived",
                    ],
                    &[
                        "Planning (Ainslie Volcanics) Amendment Bill 2025",
                        "PMB",
                        "20-03-25",
                        "08-04-25",
                    ],
                ],
                &[
                    &[
                        "Discharged/Withdrawn",
                        "PMB * ++",
                        "Introduced",
                        "Date Discharged/ Withdrawn",
                    ],
                    &[
                        "Residential Tenancies (Posting Termination) Amendment Bill 2025",
                        "PMB",
                        "06-05-25",
                        "26-03-26",
                    ],
                ],
            ],
            &[],
        );
        let volume: String = [
            ("71099", "Appropriation Bill 2024-2025 (No 2)"),
            ("75044", "Domestic Violence Agencies Amendment Bill 2026"),
            ("72290", "Planning (Ainslie Volcanics) Amendment Bill 2025"),
            ("72475", "Residential Tenancies (Posting Termination) Amendment Bill 2025"),
        ]
        .iter()
        .map(|(id, title)| {
            format!(r#"<h4><a href="https://www.legislation.act.gov.au/b/db_{id}">{title}</a></h4><ul><li><a href="https://www.legislation.act.gov.au/View/b/db_{id}/current/html/db_{id}.html">As presented</a></li></ul>"#)
        })
        .collect();
        let server = TestServer::start(move |req| {
            match req.path.as_str() {
            "/act/parliamentary-business/in-the-chamber/bills/bills_list" => Response::html(
                r#"<p>Bills List 18&mdash;1 October 2026</p><a href="/__data/assets/word_doc/0010/2/Report-Bills-List-18.docx">Word (264KB)</a>"#,
            ),
            "/act/__data/assets/word_doc/0010/2/Report-Bills-List-18.docx" => Response::bytes(
                file.clone(),
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            ),
            "/act/parliamentary-business/in-the-chamber/bills/bills_volume" => {
                Response::html(volume.clone())
            }
            "/act-legislation/b/db_71099/" => Response::html(PASSED_PAGE),
            "/act-legislation/b/db_75044/" => Response::html(register_html(
                "75044",
                "Domestic Violence Agencies Amendment Bill 2026",
                "Government Bill",
                "Minister for the Prevention of Domestic, Family and Sexual Violence",
                "Before the Assembly",
                &[(
                    "Presented by Minister for the Prevention of Domestic, Family and Sexual Violence",
                    "20260924",
                )],
            )),
            "/act-legislation/b/db_72290/" => Response::html(register_html(
                "72290",
                "Planning (Ainslie Volcanics) Amendment Bill 2025",
                "Private Member&#39;s Bill",
                "Ms Jo Clay",
                "Agreement in principle negatived",
                &[("Presented by Ms Jo Clay", "20250320")],
            )),
            "/act-legislation/b/db_72475/" => Response::html(register_html(
                "72475",
                "Residential Tenancies (Posting Termination) Amendment Bill 2025",
                "Private Member&#39;s Bill",
                "Mr Thomas Emerson",
                "Withdrawn",
                &[
                    ("Presented by Mr Thomas Emerson", "20250506"),
                    ("Withdrawn", "20260326"),
                ],
            )),
            _ => Response::status(404, "unexpected path"),
        }
        });
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/states-tests/act-bills");
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::Local(LocalStore::new(dir));
        let endpoints = Endpoints::at(&server.base);
        let list_page = format!("{}/act{LIST_PAGE}", server.base);
        let register = |path: &str| format!("{}/act-legislation{path}", server.base);

        let roll = read(&store, &endpoints, "2024-10-19").await.expect("read");
        assert_eq!(roll.state, StateCode::ACT);
        assert_eq!(roll.publisher, "ACT Legislative Assembly");
        assert_eq!(roll.source, list_page);
        let titles: Vec<&str> = roll.bills.iter().map(|b| b.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "Domestic Violence Agencies Amendment Bill 2026",
                "Housing and Consumer Affairs Legislation Amendment Bill 2026",
                "Appropriation Bill 2024-2025 (No 2)",
                "Planning (Ainslie Volcanics) Amendment Bill 2025",
                "Residential Tenancies (Posting Termination) Amendment Bill 2025",
            ],
            "the last Assembly's bill is left out"
        );

        let before = &roll.bills[0];
        assert_eq!(before.status, "Before the Assembly");
        assert_eq!((before.stage, before.stopped), (1, false));
        assert_eq!(before.origin.as_deref(), Some("Legislative Assembly"));
        assert_eq!(before.kind.as_deref(), Some("Government"));
        assert_eq!(
            before.sponsor.as_deref(),
            Some("Minister for the Prevention of Domestic, Family and Sexual Violence")
        );
        assert_eq!(before.source, register("/b/db_75044/"));
        assert_eq!(
            before.text.as_deref(),
            Some(register("/DownloadFile/b/db_75044/current/PDF/db_75044.PDF").as_str())
        );
        assert_eq!(
            before.explanatory.as_deref(),
            Some(register("/DownloadFile/es/db_750449/current/PDF/db_750449.PDF").as_str())
        );
        assert_eq!(before.timeline, [step("2026-09-24", PRESENTED)]);

        let unlinked = &roll.bills[1];
        assert_eq!(unlinked.source, list_page, "not in the Volume yet");
        assert_eq!(
            (
                unlinked.sponsor.as_deref(),
                unlinked.kind.as_deref(),
                unlinked.text.as_deref()
            ),
            (None, None, None)
        );
        assert_eq!(unlinked.timeline, [step("2026-09-17", PRESENTED)]);

        let act = &roll.bills[2];
        assert_eq!(act.status, "Act notified");
        assert_eq!(
            (act.stage, act.stopped),
            (4, false),
            "notified on the register"
        );
        assert_eq!(act.act.as_deref(), Some("A2025-7"));
        assert_eq!(act.sponsor.as_deref(), Some("Treasurer"));
        assert_eq!(
            act.timeline,
            [
                step("2025-02-06", PRESENTED),
                step("2025-03-20", PASSED),
                step("2025-03-27", NOTIFIED),
            ]
        );

        let negatived = &roll.bills[3];
        assert_eq!(negatived.status, "Agreement in principle negatived");
        assert_eq!((negatived.stage, negatived.stopped), (1, true));
        assert_eq!(negatived.kind.as_deref(), Some("Private member's"));
        assert_eq!(negatived.sponsor.as_deref(), Some("Ms Jo Clay"));

        let withdrawn = &roll.bills[4];
        assert_eq!(withdrawn.status, "Withdrawn", "the register says which");
        assert_eq!((withdrawn.stage, withdrawn.stopped), (1, true));
        assert_eq!(
            withdrawn.timeline,
            [step("2025-05-06", PRESENTED), step("2026-03-26", WITHDRAWN)]
        );

        // The next night: only the bill before the Assembly is read again.
        let again = read(&store, &endpoints, "2024-10-19")
            .await
            .expect("read again");
        assert_eq!(again.bills, roll.bills);
        let reads = |id: &str| {
            server
                .requests()
                .iter()
                .filter(|r| r.path == format!("/act-legislation/b/db_{id}/"))
                .count()
        };
        assert_eq!(reads("75044"), 2, "before the Assembly, so read nightly");
        assert_eq!(reads("71099"), 1, "passed and notified, so read once");
        assert_eq!(reads("72290"), 1);
        assert_eq!(reads("72475"), 1);
        assert_eq!(server.hits(), 3 + 4 + 3 + 1, "three Assembly pages a night");
    }
}
