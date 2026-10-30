//! The Northern Territory. The Assembly's bills are kept in the Territory's
//! legislation database rather than on the Assembly's own website. One page
//! lists every Assembly's bills, each Assembly in a table of its own: the
//! serial number, the days a bill was introduced and passed, and remarks
//! that mark a private member's bill and say how one that went no further
//! ended. Each bill's own page names its sponsor and links its documents
//! and, once it is law, its Act, whose page numbers it and dates the assent.
//!
//! The list is read each time; a bill's pages only when its row reads
//! differently from the last time they were read, or while its assent is
//! awaited, which its row never shows.

use super::{BillRoll, RollBill};
use crate::endpoints::Endpoints;
use crate::http::fetch_text;
use crate::sources::aph_bills::strip_html;
use crate::store::Store;
use anyhow::{anyhow, bail, Result};
use chrono::NaiveDate;
use pollywiki_schema::{StateCode, TimelineStep};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

/// Where each bill's last read is kept, by its id in the database.
const LAST_READS: &str = "raw/states/nt/bills/";

/// How long after a bill passes its page is read again each night for the
/// Act it became. Assent usually follows within a few weeks.
const ASSENT_WAIT_DAYS: i64 = 60;

/// A date as the database writes it, "23/07/2026"; old remarks write
/// "6/10/98".
static DATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(\d{1,2})/(\d{1,2})/(\d{4}|\d{2})\b").unwrap());

/// A bill's row on the list, as the list words it. A row that reads as it
/// did when the bill's pages were last read means they have nothing new to
/// say, assent apart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Row {
    /// The bill's id in the database, which its page's address is made of.
    id: String,
    serial: String,
    title: String,
    /// Days as the list writes them, dd/mm/yyyy.
    introduced: String,
    passed: String,
    amended: String,
    remarks: String,
}

/// What a bill's own page, and its Act's page where it links one, add to
/// its row: facts and links only.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pages {
    sponsor: Option<String>,
    text: Option<String>,
    explanatory: Option<String>,
    /// "Act No. 23 of 2026".
    act: Option<String>,
    /// The day of assent, as an ISO date.
    assent: Option<String>,
}

impl Pages {
    fn assented(&self) -> bool {
        self.act.is_some() || self.assent.is_some()
    }
}

/// A bill's pages as last read, with the row they were read for.
#[derive(Serialize, Deserialize)]
struct LastRead {
    row: Row,
    pages: Pages,
}

/// Every bill of the current Assembly: those introduced on or after
/// `since`, the day it was elected.
pub async fn read(store: &Store, endpoints: &Endpoints, since: &str) -> Result<BillRoll> {
    read_on(store, endpoints, since, chrono::Utc::now().date_naive()).await
}

/// The roll as read on `today`, which decides whose assent is still
/// awaited.
async fn read_on(
    store: &Store,
    endpoints: &Endpoints,
    since: &str,
    today: NaiveDate,
) -> Result<BillRoll> {
    let since = NaiveDate::parse_from_str(since, "%Y-%m-%d")
        .map_err(|_| anyhow!("billsSince \"{since}\" is not a YYYY-MM-DD date"))?;
    let opts = endpoints.opts(2000);
    let source = format!(
        "{}/en/LegislationPortal/Bills/By-Session",
        endpoints.nt_legislation
    );
    let list = fetch_text(&source, &opts).await?;

    // Each Assembly's table is named for it, but the name is not relied on:
    // the current Assembly's bills are those introduced since its election.
    let mut seen = HashSet::new();
    let rows: Vec<Row> = list_rows(&list)
        .into_iter()
        .filter(|row| listed_date(&row.introduced).is_some_and(|day| day >= since))
        .filter(|row| seen.insert(row.id.clone()))
        .collect();

    let mut bills = Vec::with_capacity(rows.len());
    let mut fresh = 0;
    for row in &rows {
        let page = format!(
            "{}/~/link.aspx?_id={}&_z=z",
            endpoints.nt_legislation, row.id
        );
        let key = format!("{LAST_READS}{}.json", row.id);
        // A last read in a shape this reader no longer writes counts as none.
        let last: Option<LastRead> = store
            .get_raw(&key)
            .await?
            .and_then(|raw| serde_json::from_str(&raw).ok());
        let pages = match last {
            Some(last) if last.row == *row && !awaiting_assent(row, &last.pages, today) => {
                last.pages
            }
            last => match read_pages(row, &page, endpoints).await {
                Ok(pages) => {
                    fresh += 1;
                    let last_read = LastRead {
                        row: row.clone(),
                        pages: pages.clone(),
                    };
                    store.put_json(&key, &last_read).await?;
                    pages
                }
                // The bill keeps its place from its row, with what its pages
                // said last time, and they are read again next time.
                Err(err) => {
                    eprintln!("nt-bills: Serial No. {} unread - {err}", row.serial);
                    last.map(|last| last.pages).unwrap_or_default()
                }
            },
        };
        bills.push(roll_bill(row, &pages, page));
    }
    println!(
        "nt-bills: read the pages of {fresh} of {} bills",
        rows.len()
    );
    Ok(BillRoll {
        state: StateCode::NT,
        listed_at: crate::now_iso(),
        publisher: "NT Legislation".to_string(),
        source,
        bills,
    })
}

/// Every bill on the list, from each Assembly's table, with its cells read
/// by their column headings rather than their places.
fn list_rows(list: &str) -> Vec<Row> {
    static TABLE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)<table(?:\s[^>]*)?>(.*?)</table>").unwrap());
    static HEADING: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)<th(?:\s[^>]*)?>(.*?)</th>").unwrap());
    static ROW: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)<tr(?:\s[^>]*)?>(.*?)</tr>").unwrap());
    static CELL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)<td(?:\s[^>]*)?>(.*?)</td>").unwrap());
    static BILL_ID: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"_id=([0-9A-Fa-f]{32})").unwrap());

    let mut rows = Vec::new();
    for table in TABLE.captures_iter(list) {
        let headings: Vec<String> = HEADING
            .captures_iter(&table[1])
            .map(|heading| strip_html(&heading[1]).to_lowercase())
            .collect();
        let column = |name: &str| headings.iter().position(|heading| heading == name);
        let (Some(serial), Some(bill), Some(introduced)) =
            (column("serial no"), column("bill"), column("introduced"))
        else {
            continue;
        };
        let (passed, amended, remarks) = (column("passed"), column("amended"), column("remarks"));
        for row in ROW.captures_iter(&table[1]) {
            let cells: Vec<&str> = CELL
                .captures_iter(&row[1])
                .filter_map(|cell| cell.get(1))
                .map(|cell| cell.as_str())
                .collect();
            let words = |column: Option<usize>| {
                column
                    .and_then(|i| cells.get(i))
                    .map(|cell| strip_html(cell))
                    .unwrap_or_default()
            };
            let Some(id) = cells.get(bill).and_then(|cell| BILL_ID.captures(cell)) else {
                continue;
            };
            rows.push(Row {
                id: id[1].to_uppercase(),
                serial: words(Some(serial)),
                title: words(Some(bill)),
                introduced: words(Some(introduced)),
                passed: words(passed),
                amended: words(amended),
                remarks: words(remarks),
            });
        }
    }
    rows
}

/// A passed bill whose Act has not appeared yet, while assent could still
/// be coming. Assent changes nothing in the row, so only the bill's page,
/// which links the Act once there is one, can show it.
fn awaiting_assent(row: &Row, pages: &Pages, today: NaiveDate) -> bool {
    if pages.assented() || ended(&row.remarks).is_some() {
        return false;
    }
    listed_date(&row.passed).is_some_and(|passed| (today - passed).num_days() <= ASSENT_WAIT_DAYS)
}

/// What a bill's own page says, and its Act's page where it links one.
async fn read_pages(row: &Row, page_url: &str, endpoints: &Endpoints) -> Result<Pages> {
    static DOCUMENT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"href="([^"]*/Bill/(BPDF|BWord)\?[^"]*)""#).unwrap());
    let opts = endpoints.opts(2000);
    let base = &endpoints.nt_legislation;
    let page = fetch_text(page_url, &opts).await?;
    let details = fields(&page);
    // A link the database cannot follow lands on some other page, which is
    // not to be read as this bill's.
    match details.words.get("serial no") {
        Some(serial) if *serial == row.serial => {}
        Some(serial) => bail!("{page_url} is the page for Serial No. {serial}"),
        None => bail!("{page_url} gives no Serial No"),
    }
    let document = |kind: &str| {
        DOCUMENT
            .captures_iter(&page)
            .find(|found| &found[2] == kind)
            .map(|found| address(&found[1], base))
    };
    let mut pages = Pages {
        sponsor: details.words.get("sponsor").cloned(),
        text: document("BPDF").or_else(|| document("BWord")),
        explanatory: details
            .links
            .get("explanatory statement")
            .map(|href| address(href, base)),
        ..Pages::default()
    };
    if let Some(href) = details.links.get("act") {
        let act_url = address(href, base);
        let act = fields(&fetch_text(&act_url, &opts).await?).words;
        pages.act = act
            .get("act no")
            .zip(act.get("year"))
            .map(|(number, year)| format!("Act No. {number} of {year}"));
        pages.assent = act.get("assent").and_then(|day| listed_date(day)).map(iso);
        if !pages.assented() {
            bail!("{act_url} gives neither an Act number nor a day of assent");
        }
    }
    Ok(pages)
}

/// A page's labelled fields, each label lowercased and without its colon.
#[derive(Default)]
struct Fields {
    /// The words after a label: "sponsor" to "Ms Boothby" from
    /// `<label>Sponsor:</label><span>Ms Boothby</span>`.
    words: HashMap<String, String>,
    /// Where the link after a label goes: "act" to the Act's page.
    links: HashMap<String, String>,
}

/// Fields are found by their labels rather than their places, so one that
/// moves about the page still reads.
fn fields(page: &str) -> Fields {
    static FIELD: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#"(?is)<label(?:\s[^>]*)?>([^<]*)</label>\s*(?:<span(?:\s[^>]*)?>(.*?)</span>|<a\s[^>]*?href="([^"]*)")"#,
        )
        .unwrap()
    });
    let mut out = Fields::default();
    for field in FIELD.captures_iter(page) {
        let label = strip_html(&field[1]);
        let label = label.trim_end_matches(':').trim_end().to_lowercase();
        let (into, value) = match (field.get(2), field.get(3)) {
            (Some(words), _) => (&mut out.words, strip_html(words.as_str())),
            (None, Some(href)) => (&mut out.links, href.as_str().trim().to_string()),
            (None, None) => continue,
        };
        if !label.is_empty() && !value.is_empty() {
            into.entry(label).or_insert(value);
        }
    }
    out
}

/// A link on a page as a full address: `&amp;` decoded, and a path on the
/// site put after the database's own address.
fn address(href: &str, base: &str) -> String {
    let href = href.replace("&amp;", "&");
    if href.contains("://") {
        href
    } else {
        format!("{base}/{}", href.trim_start_matches('/'))
    }
}

/// A bill as the roll has it, from its row and what its pages said.
fn roll_bill(row: &Row, pages: &Pages, source: String) -> RollBill {
    let passed = listed_date(&row.passed);
    let ending = ended(&row.remarks).filter(|_| !pages.assented());
    let mut timeline: Vec<TimelineStep> = [
        ("Introduced", listed_date(&row.introduced).map(iso)),
        ("Passed", passed.map(iso)),
        ("Assent", pages.assent.clone()),
    ]
    .into_iter()
    .filter_map(|(event, date)| {
        Some(TimelineStep {
            date: date?,
            event: event.to_string(),
        })
    })
    .chain(remark_steps(&row.remarks))
    .collect();
    timeline.sort_by(|a, b| a.date.cmp(&b.date));
    // One chamber: a bill that passes it goes straight on to assent.
    let (status, stage) = match (pages.assented(), ending, passed) {
        (true, _, _) => ("Assented", 4),
        (false, Some(ending), passed) => (ending, if passed.is_some() { 2 } else { 1 }),
        (false, None, Some(_)) => ("Passed", 2),
        (false, None, None) => ("Before the Assembly", 1),
    };
    RollBill {
        title: row.title.clone(),
        number: (!row.serial.is_empty()).then(|| format!("Serial No. {}", row.serial)),
        origin: Some("Legislative Assembly".to_string()),
        sponsor: pages.sponsor.clone(),
        kind: private_member(&row.remarks).then(|| "Private member's".to_string()),
        status: status.to_string(),
        stage,
        stopped: ending.is_some(),
        timeline,
        source,
        text: pages.text.clone(),
        explanatory: pages.explanatory.clone(),
        act: pages.act.clone(),
    }
}

/// How a bill that went no further ended, in the roll's plain word, where
/// its remarks say: "Private Member's Bill - Negatived 12/05/2026", "Bill
/// lapsed because Parliament prorogued on 01/08/2024", "Withdrawn
/// 27/11/2019".
fn ended(remarks: &str) -> Option<&'static str> {
    let remarks = remarks.to_lowercase();
    [
        ("negatived", "Negatived"),
        ("defeated", "Negatived"),
        ("lapsed", "Lapsed"),
        ("withdrawn", "Withdrawn"),
        ("discharged", "Discharged"),
    ]
    .into_iter()
    .find(|(word, _)| remarks.contains(word))
    .map(|(_, plain)| plain)
}

/// Remarks begin "Private Member's Bill" for one; a government bill's say
/// nothing of the kind.
fn private_member(remarks: &str) -> bool {
    remarks.to_lowercase().contains("private member")
}

/// A dated step for each date in a bill's remarks, named for what happened
/// that day: "Negatived 12/05/2026" is the day it was negatived.
fn remark_steps(remarks: &str) -> Vec<TimelineStep> {
    let mut steps = Vec::new();
    let mut from = 0;
    for found in DATE.find_iter(remarks) {
        let words = &remarks[from..found.start()];
        from = found.end();
        let event = ended(words)
            .map(str::to_string)
            .or_else(|| last_clause(words));
        if let (Some(day), Some(event)) = (listed_date(found.as_str()), event) {
            steps.push(TimelineStep {
                date: iso(day),
                event,
            });
        }
    }
    steps
}

/// The last clause before a date that names no ending, as a step's name:
/// "Postponed" from "Private Member's Bill - Postponed".
fn last_clause(words: &str) -> Option<String> {
    let clause = words
        .rsplit(';')
        .flat_map(|part| part.rsplit(" - "))
        .map(|part| part.trim_matches(|c: char| c.is_whitespace() || c == ',' || c == ':'))
        .find(|part| !part.is_empty())?;
    let mut chars = clause.chars();
    let first = chars.next()?;
    Some(first.to_uppercase().chain(chars).collect())
}

/// The first date in some text, as the database writes them. A two-digit
/// year from 74 on is the last century's: the Assembly first sat in 1974.
fn listed_date(text: &str) -> Option<NaiveDate> {
    let found = DATE.captures(text)?;
    let year: i32 = found[3].parse().ok()?;
    let year = match found[3].len() {
        2 if year >= 74 => 1900 + year,
        2 => 2000 + year,
        _ => year,
    };
    NaiveDate::from_ymd_opt(year, found[2].parse().ok()?, found[1].parse().ok()?)
}

fn iso(day: NaiveDate) -> String {
    day.format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::LocalStore;
    use crate::test_http::{Response, TestServer};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    const SINCE: &str = "2024-08-24";
    const LIST: &str = "/nt-legislation/en/LegislationPortal/Bills/By-Session";
    const RIGHTS: &str = "EBD33A287B5F496FBF9D3DE24A9FD373";
    const MARINE: &str = "1BC412B9C6CE484B8F09EDEDC622B572";
    const HOSPITALS: &str = "2028CE3EECAA4D9C8D9037A6E88E0439";
    const ELECTORAL: &str = "0000000000000000000000000000000C";
    const TRAFFIC: &str = "0000000000000000000000000000000B";
    const PAYROLL: &str = "00000000000000000000000000000009";
    const LAST_YOUTH: &str = "A0000000000000000000000000000123";
    const LAST_FUEL: &str = "A0000000000000000000000000000058";

    fn new_store(name: &str) -> Store {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/state-bills-tests")
            .join(format!("nt-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Store::Local(LocalStore::new(dir))
    }

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 30).unwrap()
    }

    /// A row as the list lays it out, subscribe button first.
    fn row(
        serial: &str,
        id: &str,
        title: &str,
        introduced: &str,
        passed: &str,
        amended: &str,
        remarks: &str,
    ) -> String {
        format!(
            r#"<tr>
                <td><button class="btn btn-default btn-sm btn-subscribe disabled" title="Please login to My Account to subscribe"><span class="glyphicon glyphicon-envelope"></span></button></td>
                <td>{serial}</td>
                <td><a href="~/link.aspx?_id={id}&amp;amp;_z=z" onclick="replaceHistory('header_bill_acc_X')">{title}</a></td>
                <td>{introduced}</td>
                <td>{passed}</td>
                <td>{amended}</td>
                <td>{remarks}</td>
            </tr>"#
        )
    }

    /// One Assembly's table on the list.
    fn panel(name: &str, years: &str, rows: &[String]) -> String {
        format!(
            r##"<div class="panel-heading" data-toggle="collapse" data-target="#bill_acc_{name}">
                <span class="navanchor" id="header_bill_acc_{name}" ></span>
                <h4 class="panel-title">{name} ({years})</h4>
            </div>
            <div id="bill_acc_{name}" class="row panel-collapse collapse">
                <div class="panel-body col-md-12"><div class="table-responsive bill-by-session">
                    <table class="table">
                        <thead><tr><th></th><th>Serial No</th><th>Bill</th><th>Introduced</th>
                            <th>Passed</th><th>Amended</th><th>Remarks</th></tr></thead>
                        {}
                    </table>
                </div></div>
            </div>"##,
            rows.concat()
        )
    }

    fn list_page(panels: &[String]) -> String {
        format!(
            r#"<h3 class="act-list-title roundedDarkBorders">Bills - Browse by Session</h3>
            <div class="panel panel-default">{}</div>"#,
            panels.concat()
        )
    }

    /// A bill's page: its details, its downloads and what is related.
    fn bill_page(serial: &str, sponsor: &str, downloads: &str, related: &str) -> String {
        format!(
            r#"<div class="actPage"><fieldset class="roundedWhiteBorders">
                <legend class="roundedWhiteBordersSmall">Details</legend>
                <div class="row"><label>Serial No:</label><span>{serial}</span></div>
                <div class="row"><label>Assembly:</label><span>Fifteenth (2024 - )</span></div>
                <div class="row"><label>Sponsor:</label><span>{sponsor}</span></div>
                <div class="row"><label>Remarks:</label><span></span></div>
            </fieldset>
            <fieldset><legend>Download</legend>{downloads}</fieldset>
            <fieldset><legend>Related</legend>{related}</fieldset></div>
            <label for="findInput" class="toolbarLabel">Find:</label><input id="findInput">"#
        )
    }

    fn download(href: &str, title: &str) -> String {
        format!(r#"<a href="{href}"><i class="fa fa-3x" title="{title}"></i></a>"#)
    }

    fn act_link(slug: &str, act: &str) -> String {
        format!(
            r#"<div class="row"><label>Act:</label> <a href="/en/Bills/{slug}?format=assented">{act}</a></div>"#
        )
    }

    /// An Act's page; its labels carry a space before the closing tag.
    fn act_page(number: &str, year: &str, assent: &str) -> String {
        format!(
            r#"<fieldset><legend>Details</legend>
                <div class="row"><label>Act No: </label><span>{number}</span></div>
                <div class="row"><label>Year: </label><span>{year}</span></div>
                <div class="row"><label>Assent: </label><span>{assent}</span></div>
                <div class="row"><label>Commencement: </label><span></span></div>
                <div class="row"><label>Status:</label><span>As Assented</span></div>
            </fieldset>"#
        )
    }

    fn bill_path(id: &str) -> String {
        format!("/nt-legislation/~/link.aspx?_id={id}&_z=z")
    }

    fn step(date: &str, event: &str) -> TimelineStep {
        TimelineStep {
            date: date.to_string(),
            event: event.to_string(),
        }
    }

    /// The paths asked for since the first `skip` requests.
    fn asked(server: &TestServer, skip: usize) -> Vec<String> {
        server.requests()[skip..]
            .iter()
            .map(|r| r.path.clone())
            .collect()
    }

    #[test]
    fn dates_are_read_as_the_database_writes_them() {
        let day = |text: &str| listed_date(text).map(iso);
        assert_eq!(day("23/07/2026").as_deref(), Some("2026-07-23"));
        assert_eq!(day(" Withdrawn 6/10/26 ").as_deref(), Some("2026-10-06"));
        assert_eq!(
            day("Discharged 19/8/98").as_deref(),
            Some("1998-08-19"),
            "so an old row can never pass for a current one"
        );
        assert_eq!(day("31/02/2026"), None, "no such day");
        assert_eq!(day("Cognate with Serial 35"), None);
        assert_eq!(day(""), None);
    }

    #[test]
    fn remarks_say_how_a_bill_ended_and_when() {
        let cases = [
            (
                "Private Member's Bill - Negatived 12/05/2026",
                Some("Negatived"),
                true,
                vec![step("2026-05-12", "Negatived")],
            ),
            (
                "Bill lapsed because Parliament prorogued on 01/08/2024",
                Some("Lapsed"),
                false,
                vec![step("2024-08-01", "Lapsed")],
            ),
            (
                "Private Member's Bill - Lapsed due to resignation of Member - 09/07/2025",
                Some("Lapsed"),
                true,
                vec![step("2025-07-09", "Lapsed")],
            ),
            (
                "Withdrawn 27/11/2024",
                Some("Withdrawn"),
                false,
                vec![step("2024-11-27", "Withdrawn")],
            ),
            (
                "Defeated - 17/09/2025 - Standing Order 180",
                Some("Negatived"),
                false,
                vec![step("2025-09-17", "Negatived")],
            ),
            (
                "Private Members Bill - postponed 23/4/25; cognate with s12",
                None,
                true,
                vec![step("2025-04-23", "Postponed")],
            ),
            ("Cognate with Serial 35", None, false, vec![]),
            ("Passed on urgency", None, false, vec![]),
            ("", None, false, vec![]),
        ];
        for (remarks, outcome, private, steps) in cases {
            assert_eq!(ended(remarks), outcome, "{remarks}");
            assert_eq!(private_member(remarks), private, "{remarks}");
            assert_eq!(remark_steps(remarks), steps, "{remarks}");
        }
    }

    #[test]
    fn columns_are_read_by_their_headings_not_their_places() {
        let list = r#"<table class="table"><thead><tr>
                <th>Bill</th><th>Remarks</th><th>Serial No</th><th>Introduced</th><th>Passed</th>
            </tr></thead>
            <tr><td><a href="~/link.aspx?_id=ebd33a287b5f496fbf9d3de24a9fd373&amp;amp;_z=z">Rights of the
                Terminally Ill Bill 2026 </a></td><td>Private Member&#39;s Bill</td>
                <td>70</td><td>23/07/2026</td><td></td></tr>
            <tr><td>no link, so no bill</td><td></td><td>71</td><td>23/07/2026</td><td></td></tr>
            </table>
            <table><tr><th>Act</th><th>Year</th></tr><tr><td>Not a bill table</td><td>2026</td></tr></table>"#;
        assert_eq!(
            list_rows(list),
            [Row {
                id: RIGHTS.to_string(),
                serial: "70".to_string(),
                title: "Rights of the Terminally Ill Bill 2026".to_string(),
                introduced: "23/07/2026".to_string(),
                passed: String::new(),
                amended: String::new(),
                remarks: "Private Member's Bill".to_string(),
            }]
        );
    }

    #[test]
    fn fields_are_read_by_their_labels() {
        let page = act_page("23", "2026", "03/09/2026")
            + &bill_page(
                "70",
                "Ms Boothby",
                "",
                &act_link("Some-Bill-S70", "Some Act"),
            );
        let fields = fields(&page);
        assert_eq!(fields.words["act no"], "23");
        assert_eq!(fields.words["assent"], "03/09/2026");
        assert_eq!(fields.words["sponsor"], "Ms Boothby");
        assert_eq!(
            fields.links["act"],
            "/en/Bills/Some-Bill-S70?format=assented"
        );
        assert!(
            !fields.words.contains_key("remarks"),
            "an empty value is none"
        );
        assert!(
            !fields.words.contains_key("find"),
            "a label for an input is none"
        );
    }

    #[tokio::test]
    async fn the_current_assembly_s_bills_are_read_with_their_pages() {
        let server = TestServer::start(|req| {
            let es = "https://legislation.nt.gov.au/api/sitecore/Bill/SC_OtherDoc?itemId=ebd33a28-7b5f-496f-bf9d-3de24a9fd373&amp;type=ExplanatoryStatementBody";
            let body = match req.path.as_str() {
                LIST => list_page(&[
                    panel(
                        "Fifteenth",
                        "2024 - ",
                        &[
                            row("82", HOSPITALS, "Private Hospitals Amendment Bill 2026", "22/10/2026", "", "", ""),
                            row("78", MARINE, "Marine Facilities (Access and Pricing) Bill 2026", "26/08/2026", "22/10/2026", "No", ""),
                            row("70", RIGHTS, "Rights of the Terminally Ill Bill 2026", "23/07/2026", "27/08/2026", "Yes", ""),
                            row("12", ELECTORAL, "Electoral Amendment Bill 2025", "13/02/2025", "", "", "Private Member&#39;s Bill - Negatived 19/03/2025"),
                            row("11", TRAFFIC, "Traffic Amendment Bill 2025", "12/02/2025", "", "", "Private Member&#39;s Bill - Lapsed due to resignation of Member 09/07/2025"),
                            row("9", PAYROLL, "Payroll Tax Amendment Bill 2024", "16/10/2024", "", "", "Withdrawn 27/11/2024"),
                        ],
                    ),
                    panel(
                        "Fourteenth",
                        "2020 - 2024",
                        &[
                            row("123", LAST_YOUTH, "Youth Justice Amendment Bill 2024", "20/03/2024", "", "", "Private Member&#39;s Bill - Bill lapsed because Parliament prorogued on 01/08/2024"),
                            row("58", LAST_FUEL, "Fuel Price Disclosure Bill 2022", "31/08/2022", "12/10/2022", "No", ""),
                        ],
                    ),
                ]),
                path if path == bill_path(HOSPITALS) => bill_page(
                    "82",
                    "Mr Example",
                    &download("/api/sitecore/Bill/BWord?id=19420", "Word Download"),
                    "",
                ),
                path if path == bill_path(MARINE) => bill_page(
                    "78",
                    "Mrs Sample",
                    &download("/api/sitecore/Bill/BPDF?id=19410", "PDF Download"),
                    "",
                ),
                path if path == bill_path(RIGHTS) => bill_page(
                    "70",
                    "Ms Boothby",
                    &(download("https://legislation.nt.gov.au/api/sitecore/Bill/BWord?id=19398", "Word Download")
                        + &download("https://legislation.nt.gov.au/api/sitecore/Bill/BPDF?id=19398", "PDF Download")),
                    &(act_link("Rights-of-the-Terminally-Ill-Bill-2026-S70", "Rights of the Terminally Ill Act 2026")
                        + &format!(r#"<div class="row"><label>Explanatory Statement:</label>
                            <a href="{es}"><i class="fa fa-2x" title="Explanatory Statement Download"></i></a></div>"#)),
                ),
                path if path == bill_path(ELECTORAL) => bill_page("12", "Mr Example", "", ""),
                path if path == bill_path(TRAFFIC) => bill_page("11", "Ms Instance", "", ""),
                path if path == bill_path(PAYROLL) => bill_page("9", "Mrs Sample", "", ""),
                "/nt-legislation/en/Bills/Rights-of-the-Terminally-Ill-Bill-2026-S70?format=assented" => {
                    act_page("23", "2026", "03/09/2026")
                }
                _ => return Response::status(404, "unexpected path"),
            };
            Response::html(body)
        });
        let store = new_store("read");
        let roll = read(&store, &Endpoints::at(&server.base), SINCE)
            .await
            .expect("read");

        let base = format!("{}/nt-legislation", server.base);
        assert_eq!(roll.state, StateCode::NT);
        assert_eq!(roll.publisher, "NT Legislation");
        assert_eq!(
            roll.source,
            format!("{base}/en/LegislationPortal/Bills/By-Session")
        );
        let numbers: Vec<&str> = roll
            .bills
            .iter()
            .filter_map(|b| b.number.as_deref())
            .collect();
        assert_eq!(
            numbers,
            [
                "Serial No. 82",
                "Serial No. 78",
                "Serial No. 70",
                "Serial No. 12",
                "Serial No. 11",
                "Serial No. 9"
            ],
            "the last Assembly's bills, in a table of their own, are left out"
        );
        assert!(
            asked(&server, 0)
                .iter()
                .all(|path| !path.contains(LAST_YOUTH) && !path.contains(LAST_FUEL)),
            "nor are their pages read"
        );
        assert_eq!(server.hits(), 8, "the list, six bills and one Act");

        assert_eq!(
            roll.bills[2],
            RollBill {
                title: "Rights of the Terminally Ill Bill 2026".to_string(),
                number: Some("Serial No. 70".to_string()),
                origin: Some("Legislative Assembly".to_string()),
                sponsor: Some("Ms Boothby".to_string()),
                kind: None,
                status: "Assented".to_string(),
                stage: 4,
                stopped: false,
                timeline: vec![
                    step("2026-07-23", "Introduced"),
                    step("2026-08-27", "Passed"),
                    step("2026-09-03", "Assent"),
                ],
                source: format!("{base}/~/link.aspx?_id={RIGHTS}&_z=z"),
                text: Some("https://legislation.nt.gov.au/api/sitecore/Bill/BPDF?id=19398".to_string()),
                explanatory: Some("https://legislation.nt.gov.au/api/sitecore/Bill/SC_OtherDoc?itemId=ebd33a28-7b5f-496f-bf9d-3de24a9fd373&type=ExplanatoryStatementBody".to_string()),
                act: Some("Act No. 23 of 2026".to_string()),
            }
        );

        let passed = &roll.bills[1];
        assert_eq!(
            (passed.status.as_str(), passed.stage, passed.stopped),
            ("Passed", 2, false)
        );
        assert_eq!(passed.act, None);
        assert_eq!(
            passed.timeline,
            [
                step("2026-08-26", "Introduced"),
                step("2026-10-22", "Passed")
            ]
        );
        assert_eq!(
            passed.text.as_deref(),
            Some(format!("{base}/api/sitecore/Bill/BPDF?id=19410").as_str()),
            "a link on the site is a full address"
        );

        let before = &roll.bills[0];
        assert_eq!(
            (before.status.as_str(), before.stage, before.stopped),
            ("Before the Assembly", 1, false)
        );
        assert_eq!(before.sponsor.as_deref(), Some("Mr Example"));
        assert_eq!(before.kind, None, "a government bill");
        assert_eq!(before.timeline, [step("2026-10-22", "Introduced")]);
        assert_eq!(
            before.text.as_deref(),
            Some(format!("{base}/api/sitecore/Bill/BWord?id=19420").as_str()),
            "the Word text where there is no PDF"
        );
        assert_eq!(before.explanatory, None);

        for (bill, status, private, ended_on) in [
            (&roll.bills[3], "Negatived", true, "2025-03-19"),
            (&roll.bills[4], "Lapsed", true, "2025-07-09"),
            (&roll.bills[5], "Withdrawn", false, "2024-11-27"),
        ] {
            assert_eq!(bill.status, status);
            assert_eq!(bill.stage, 1, "{status}");
            assert!(bill.stopped, "{status}");
            assert_eq!(
                bill.kind.as_deref(),
                private.then_some("Private member's"),
                "{status}"
            );
            assert_eq!(bill.timeline.last(), Some(&step(ended_on, status)));
            assert_eq!(bill.timeline.len(), 2, "{status}: introduced, then ended");
        }
    }

    #[tokio::test]
    async fn a_bill_s_pages_are_read_again_only_when_its_row_changes() {
        let passed = Arc::new(AtomicBool::new(false));
        let has_passed = Arc::clone(&passed);
        let server = TestServer::start(move |req| {
            let body = match req.path.as_str() {
                LIST => {
                    let day = if has_passed.load(Ordering::SeqCst) { "29/10/2026" } else { "" };
                    list_page(&[panel(
                        "Fifteenth",
                        "2024 - ",
                        &[
                            row("82", HOSPITALS, "Private Hospitals Amendment Bill 2026", "22/10/2026", day, "", ""),
                            row("70", RIGHTS, "Rights of the Terminally Ill Bill 2026", "23/07/2026", "27/08/2026", "Yes", ""),
                            row("12", ELECTORAL, "Electoral Amendment Bill 2025", "13/02/2025", "", "", "Private Member&#39;s Bill - Negatived 19/03/2025"),
                        ],
                    )])
                }
                path if path == bill_path(HOSPITALS) => bill_page("82", "Mr Example", "", ""),
                path if path == bill_path(ELECTORAL) => bill_page("12", "Mr Example", "", ""),
                path if path == bill_path(RIGHTS) => bill_page(
                    "70",
                    "Ms Boothby",
                    "",
                    &act_link("Rights-of-the-Terminally-Ill-Bill-2026-S70", "Rights of the Terminally Ill Act 2026"),
                ),
                "/nt-legislation/en/Bills/Rights-of-the-Terminally-Ill-Bill-2026-S70?format=assented" => {
                    act_page("23", "2026", "03/09/2026")
                }
                _ => return Response::status(404, "unexpected path"),
            };
            Response::html(body)
        });
        let store = new_store("unchanged");
        let endpoints = Endpoints::at(&server.base);

        let first = read_on(&store, &endpoints, SINCE, today())
            .await
            .expect("first");
        assert_eq!(server.hits(), 5, "the list, three bills and one Act");

        let read_before = server.hits();
        let second = read_on(&store, &endpoints, SINCE, today())
            .await
            .expect("second");
        assert_eq!(
            asked(&server, read_before),
            [LIST],
            "bills whose rows read as before are not read again"
        );
        assert_eq!(second.bills, first.bills, "and read as they did");

        passed.store(true, Ordering::SeqCst);
        let read_before = server.hits();
        let third = read_on(&store, &endpoints, SINCE, today())
            .await
            .expect("third");
        assert_eq!(
            asked(&server, read_before),
            [LIST.to_string(), bill_path(HOSPITALS)],
            "only the bill whose row changed is read again"
        );
        assert_eq!(third.bills[0].status, "Passed");
        assert_eq!(third.bills[0].sponsor.as_deref(), Some("Mr Example"));
    }

    #[tokio::test]
    async fn a_passed_bill_is_read_each_night_until_its_act_appears() {
        let assented = Arc::new(AtomicBool::new(false));
        let has_act = Arc::clone(&assented);
        let server = TestServer::start(move |req| {
            let body = match req.path.as_str() {
                LIST => list_page(&[panel(
                    "Fifteenth",
                    "2024 - ",
                    &[
                        row("78", MARINE, "Marine Facilities (Access and Pricing) Bill 2026", "26/08/2026", "22/10/2026", "No", ""),
                        row("70", RIGHTS, "Rights of the Terminally Ill Bill 2026", "23/07/2026", "27/08/2026", "Yes", ""),
                    ],
                )]),
                path if path == bill_path(MARINE) => {
                    let related = if has_act.load(Ordering::SeqCst) {
                        act_link("Marine-Facilities-Access-and-Pricing-Bill-2026-S78", "Marine Facilities (Access and Pricing) Act 2026")
                    } else {
                        String::new()
                    };
                    bill_page("78", "Mrs Sample", "", &related)
                }
                // Long passed, and no Act linked: past waiting for.
                path if path == bill_path(RIGHTS) => bill_page("70", "Ms Boothby", "", ""),
                "/nt-legislation/en/Bills/Marine-Facilities-Access-and-Pricing-Bill-2026-S78?format=assented" => {
                    act_page("30", "2026", "05/11/2026")
                }
                _ => return Response::status(404, "unexpected path"),
            };
            Response::html(body)
        });
        let store = new_store("assent");
        let endpoints = Endpoints::at(&server.base);

        read_on(&store, &endpoints, SINCE, today())
            .await
            .expect("first");
        assert_eq!(server.hits(), 3, "the list and both bills");

        let read_before = server.hits();
        let waiting = read_on(&store, &endpoints, SINCE, today())
            .await
            .expect("waiting");
        assert_eq!(
            asked(&server, read_before),
            [LIST.to_string(), bill_path(MARINE)],
            "the bill passed eight days ago is looked at again; the one passed in August is not"
        );
        assert_eq!(waiting.bills[0].status, "Passed");

        assented.store(true, Ordering::SeqCst);
        let read_before = server.hits();
        let night = NaiveDate::from_ymd_opt(2026, 11, 6).unwrap();
        let law = read_on(&store, &endpoints, SINCE, night)
            .await
            .expect("assented");
        assert_eq!(
            asked(&server, read_before).len(),
            3,
            "the list, the bill and now its Act"
        );
        let marine = &law.bills[0];
        assert_eq!((marine.status.as_str(), marine.stage), ("Assented", 4));
        assert_eq!(marine.act.as_deref(), Some("Act No. 30 of 2026"));
        assert_eq!(marine.timeline.last(), Some(&step("2026-11-05", "Assent")));
        assert_eq!(
            law.bills[1].status, "Passed",
            "no Act is linked for this one"
        );

        let read_before = server.hits();
        read_on(&store, &endpoints, SINCE, night)
            .await
            .expect("after");
        assert_eq!(
            asked(&server, read_before),
            [LIST],
            "an Act once found is kept"
        );
    }

    #[tokio::test]
    async fn a_bill_whose_page_cannot_be_read_keeps_its_row_and_is_tried_again() {
        let server = TestServer::start(|req| {
            let body = match req.path.as_str() {
                LIST => list_page(&[panel(
                    "Fifteenth",
                    "2024 - ",
                    &[
                        row(
                            "82",
                            HOSPITALS,
                            "Private Hospitals Amendment Bill 2026",
                            "22/10/2026",
                            "",
                            "",
                            "",
                        ),
                        row(
                            "12",
                            ELECTORAL,
                            "Electoral Amendment Bill 2025",
                            "13/02/2025",
                            "",
                            "",
                            "Private Member&#39;s Bill - Negatived 19/03/2025",
                        ),
                    ],
                )]),
                // The link lands on another bill's page.
                path if path == bill_path(ELECTORAL) => bill_page("13", "Mr Example", "", ""),
                _ => return Response::status(404, "no such page"),
            };
            Response::html(body)
        });
        let store = new_store("unread");
        let endpoints = Endpoints::at(&server.base);

        let roll = read_on(&store, &endpoints, SINCE, today())
            .await
            .expect("read");
        assert_eq!(roll.bills.len(), 2, "an unread page sinks no bill");
        let hospitals = &roll.bills[0];
        assert_eq!(hospitals.title, "Private Hospitals Amendment Bill 2026");
        assert_eq!(hospitals.status, "Before the Assembly");
        assert_eq!(hospitals.sponsor, None);
        let electoral = &roll.bills[1];
        assert_eq!(
            (electoral.status.as_str(), electoral.stopped),
            ("Negatived", true)
        );
        assert_eq!(
            electoral.sponsor, None,
            "another bill's page is not this one's"
        );

        let read_before = server.hits();
        read_on(&store, &endpoints, SINCE, today())
            .await
            .expect("again");
        assert_eq!(server.hits() - read_before, 3, "both are tried again");
    }

    #[tokio::test]
    async fn a_since_that_is_not_a_date_is_refused_before_any_request() {
        let server = TestServer::start(|_| Response::status(404, "unexpected"));
        let err = read(
            &new_store("since"),
            &Endpoints::at(&server.base),
            "24/08/2024",
        )
        .await
        .expect_err("not a date");
        assert!(err.to_string().contains("24/08/2024"), "got {err}");
        assert_eq!(server.hits(), 0);
    }
}
