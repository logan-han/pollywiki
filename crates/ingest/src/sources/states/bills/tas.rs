//! Tasmania. Each bill has a page on the Parliament's website, linked from
//! the list of bills introduced in its year and, while it is before
//! Parliament, from the Current Bills list. A page gives the bill's number,
//! who introduced it, the parliament it was first introduced in and a table
//! of the days each house passed each stage, the House of Assembly's column
//! on the left. It does not say which house the bill began in; that is the
//! house that read it a first time first.
//!
//! Pages are kept in the store as read. A bill on the Current Bills list is
//! read again each night, and once more on the first night it is off the
//! list, when its page shows the assent or the end it came to. After that
//! it does not change, so the kept copy stands in for it.

use super::{BillRoll, RollBill};
use crate::endpoints::Endpoints;
use crate::http::{fetch_text, FetchOpts};
use crate::sources::aph_bills::strip_html;
use crate::store::Store;
use anyhow::{anyhow, bail, Context, Result};
use chrono::{Datelike, NaiveDate};
use pollywiki_schema::{StateCode, TimelineStep};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

const PUBLISHER: &str = "Parliament of Tasmania";
const ASSEMBLY: &str = "House of Assembly";
const COUNCIL: &str = "Legislative Council";

/// Where each bill's page is kept once read, under its address.
const KEPT: &str = "raw/states/tas/bills/";

static LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)<a\s[^>]*?href="([^"]*)"[^>]*>(.*?)</a>"#).unwrap());

/// A year's list of bills as the bills page links it under "Annual List of
/// Bills": the link's words are the year alone.
static YEAR_LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)<a\s[^>]*?href="([^"]*)"[^>]*>\s*(\d{4})\s*</a>"#).unwrap()
});

/// A bill on a year's list: <li><a href="...">Residential Parks Bill 2026
/// (2 of 2026)</a></li>.
static LIST_ITEM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)<li(?:\s[^>]*)?>\s*<a\s[^>]*?href="([^"]*)""#).unwrap());

static HEADING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<h1(?:\s[^>]*)?>(.*?)</h1>").unwrap());

static LAST_UPDATED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<strong>\s*Last updated:?\s*</strong>(.*?)</p>").unwrap());

static INTRODUCED_BY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<strong>\s*Introduced by:?\s*</strong>(.*?)</p>").unwrap());

/// "Bill first introduced in the 52nd Parliament, 1st Session."
static PARLIAMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)introduced in the (\d+)(?:st|nd|rd|th)?\s+Parliament").unwrap()
});

static TABLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<table(?:\s[^>]*)?>.*?</table>").unwrap());

static ROW: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<tr(?:\s[^>]*)?>(.*?)</tr>").unwrap());

static CELL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<td(?:\s[^>]*)?>(.*?)</td>").unwrap());

/// The table's footnote marks: "2nd Reading moved <sup>*</sup>".
static FOOTNOTE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<sup(?:\s[^>]*)?>.*?</sup>").unwrap());

/// The number a heading ends with, typed as the site types it: "(21 of
/// 2026)", "(21 0f 2026)", "(Bill 26 of 2025)".
static NUMBER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\(\s*(?:bill\s+)?(?:no\.?\s*)?(\d+)\s*[o0]f\s*(\d{4})\s*\)?\s*$").unwrap()
});

/// "Act 19 of 2026", which the table gives once the bill is law, and leaves
/// as "Act" until then.
static ACT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^Act\s+(?:No\.?\s*)?(\d+)\s*[o0]f\s*(\d{4})$").unwrap());

static DATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d{1,2})/(\d{1,2})/(\d{4})").unwrap());

/// A member named rather than a portfolio: "Ms Tabatha Badger MP". An
/// opposition office counts as a member too, as only ministers bring in
/// the Government's bills.
static MEMBER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:the\s+)?(?:hon(?:ourable)?|mr|mrs|ms|miss|mx|dr|prof(?:essor)?)\.?\s|\s(?:mp|mlc)$|\b(?:opposition|shadow)\b",
    )
    .unwrap()
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum House {
    Assembly,
    Council,
}

impl House {
    fn name(self) -> &'static str {
        match self {
            House::Assembly => ASSEMBLY,
            House::Council => COUNCIL,
        }
    }

    fn other(self) -> House {
        match self {
            House::Assembly => House::Council,
            House::Council => House::Assembly,
        }
    }
}

/// A bill's page as it reads, which is what the store keeps. It is worded
/// into the roll afresh on every run, so a change to that wording reaches
/// every bill without a page being read again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BillPage {
    url: String,
    /// Whether the bill was on the Current Bills list when this copy was
    /// read. A copy read while it was can be out of date the night it
    /// leaves, so only one read after that is kept for good.
    listed: bool,
    /// The title and number: "Residential Parks Bill 2026 (2 of 2026)".
    heading: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_updated: Option<String>,
    /// A portfolio for the Government's bills, a member for their own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    introduced_by: Option<String>,
    /// The parliament it was first introduced in: 52 for the 52nd.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parliament: Option<u32>,
    /// The table of stages, row by row.
    #[serde(default)]
    stages: Vec<Stage>,
    /// The "Related Documents" links.
    #[serde(default)]
    documents: Vec<Document>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Stage {
    house: House,
    /// The stage as the table names it: "Third reading", "Act 19 of 2026".
    label: String,
    /// The entry against it: a date, "Yes", "No" or nothing.
    value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Document {
    name: String,
    url: String,
}

pub async fn read(store: &Store, endpoints: &Endpoints, since: &str) -> Result<BillRoll> {
    let first_year = NaiveDate::parse_from_str(since, "%Y-%m-%d")
        .with_context(|| format!("billsSince {since:?} is not a date"))?
        .year();
    let opts = endpoints.opts(1500);
    let source = format!("{}/bills", endpoints.tas_parliament);
    let front = fetch_text(&source, &opts).await?;
    let current = current_bills(&front, &source)?;

    // Every bill introduced since the parliament was elected is on its
    // year's list, the current ones included.
    let mut urls: Vec<String> = Vec::new();
    for list in year_lists(&front, &source, first_year)? {
        let page = fetch_text(&list, &opts).await?;
        for url in listed_bills(&page, &list)? {
            if !urls.contains(&url) {
                urls.push(url);
            }
        }
    }
    if urls.is_empty() {
        bail!("the lists of bills by year linked from {source} list no bills");
    }
    for url in &current {
        if !urls.contains(url) {
            urls.push(url.clone());
        }
    }

    let mut pages = Vec::with_capacity(urls.len());
    for url in &urls {
        pages.push(bill_page(store, url, current.contains(url), &opts).await?);
    }

    // The parliament is the one the bills before it now were introduced in,
    // which lets the bills of the last one, still on the year lists, go.
    let parliament = pages
        .iter()
        .filter(|p| p.listed)
        .filter_map(|p| p.parliament)
        .max()
        .ok_or_else(|| {
            anyhow!("no bill on the Current Bills list at {source} names the parliament it is in")
        })?;
    let bills = pages
        .iter()
        .filter(|p| belongs(p, parliament, since))
        .map(roll_bill)
        .collect();
    Ok(BillRoll {
        state: StateCode::TAS,
        listed_at: crate::now_iso(),
        publisher: PUBLISHER.to_string(),
        source,
        bills,
    })
}

/// The bills the bills page lists under "Current Bills": those before
/// Parliament now.
fn current_bills(front: &str, source: &str) -> Result<Vec<String>> {
    let list = section(main_content(front), "Current Bills")
        .ok_or_else(|| anyhow!("no Current Bills list on {source}"))?;
    LINK.captures_iter(list)
        .map(|link| resolve(source, &link[1]))
        .collect()
}

/// The lists of bills introduced in each year from the parliament's first,
/// as the bills page links them. A new year's list may not be up yet, and
/// its bills are on the Current Bills list meanwhile; the first year's list
/// must be there.
fn year_lists(front: &str, source: &str, first_year: i32) -> Result<Vec<String>> {
    let mut lists: Vec<(i32, String)> = Vec::new();
    for link in YEAR_LINK.captures_iter(main_content(front)) {
        let year: i32 = link[2].parse()?;
        if year >= first_year && !lists.iter().any(|(y, _)| *y == year) {
            lists.push((year, resolve(source, &link[1])?));
        }
    }
    if !lists.iter().any(|(y, _)| *y == first_year) {
        bail!("{source} links no list of the bills introduced in {first_year}");
    }
    lists.sort();
    Ok(lists.into_iter().map(|(_, list)| list).collect())
}

/// The bills a year's list links to, in its order. Their addresses follow
/// no one pattern, so they are taken as linked.
fn listed_bills(list: &str, url: &str) -> Result<Vec<String>> {
    LIST_ITEM
        .captures_iter(main_content(list))
        .map(|link| resolve(url, &link[1]))
        .collect()
}

/// A bill's page: the copy in the store where it was read after the bill
/// had left the Current Bills list, as nothing on it changes after that;
/// otherwise read and kept.
async fn bill_page(store: &Store, url: &str, listed: bool, opts: &FetchOpts) -> Result<BillPage> {
    let key = kept_key(url);
    // A copy in a shape this reader no longer writes counts as none.
    let kept: Option<BillPage> = store
        .get_raw(&key)
        .await?
        .and_then(|raw| serde_json::from_str::<BillPage>(&raw).ok())
        .filter(|k| k.url == url);
    if let Some(page) = kept.as_ref().filter(|k| !listed && !k.listed) {
        return Ok(page.clone());
    }
    let html = match fetch_text(url, opts).await {
        Ok(html) => html,
        // A page that will not load leaves the bill as it was last read, and
        // it is read again next time. A bill never read cannot be left out
        // quietly, so its page failing fails the read.
        Err(err) => match kept {
            Some(mut page) => {
                eprintln!("tas-bills: {url} unread, so as last read - {err}");
                page.listed = listed;
                return Ok(page);
            }
            None => return Err(err),
        },
    };
    let page = parse_page(&html, url, listed)?;
    store.put_json(&key, &page).await?;
    Ok(page)
}

/// Where a bill's page is kept: its address after "/bills/", e.g.
/// "raw/states/tas/bills/bills2026/residential-parks-bill-2026-2-of-2026.json".
/// The copy records its full address too, so two addresses that came out
/// the same here would only cost a read.
fn kept_key(url: &str) -> String {
    let path = url.split_once("/bills/").map_or(url, |(_, rest)| rest);
    let path: String = path
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_./".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{KEPT}{}.json", path.replace("..", "-").trim_matches('/'))
}

/// What a bill's page says, as it says it.
fn parse_page(html: &str, url: &str, listed: bool) -> Result<BillPage> {
    let body = main_content(html);
    let heading = text_of(&HEADING, body).ok_or_else(|| anyhow!("no heading on {url}"))?;
    let table = TABLE
        .find_iter(body)
        .map(|t| t.as_str())
        .find(|t| t.contains(ASSEMBLY))
        .ok_or_else(|| anyhow!("no table of stages on {url}"))?;
    // The houses' columns are told apart only by place, so a table laid out
    // the other way round would put every stage against the wrong house.
    match (table.find(ASSEMBLY), table.find(COUNCIL)) {
        (Some(assembly), Some(council)) if assembly < council => {}
        _ => bail!("the table of stages on {url} does not have the {ASSEMBLY} on the left"),
    }
    let mut stages = Vec::new();
    for row in ROW.captures_iter(table) {
        let cells: Vec<String> = CELL
            .captures_iter(&row[1])
            .map(|cell| strip_html(&FOOTNOTE.replace_all(&cell[1], "")))
            .collect();
        for (house, pair) in [House::Assembly, House::Council]
            .into_iter()
            .zip(cells.chunks(2))
        {
            if let [label, value] = pair {
                if !label.is_empty() {
                    stages.push(Stage {
                        house,
                        label: label.clone(),
                        value: value.clone(),
                    });
                }
            }
        }
    }
    let documents = match section(body, "Related Documents") {
        Some(list) => LINK
            .captures_iter(list)
            .map(|link| {
                Ok(Document {
                    name: strip_html(&link[2]),
                    url: resolve(url, &link[1])?,
                })
            })
            .collect::<Result<Vec<_>>>()?,
        None => Vec::new(),
    };
    Ok(BillPage {
        url: url.to_string(),
        listed,
        heading,
        last_updated: text_of(&LAST_UPDATED, body),
        introduced_by: text_of(&INTRODUCED_BY, body),
        parliament: PARLIAMENT
            .captures(&strip_html(body))
            .and_then(|p| p[1].parse().ok()),
        stages,
        documents,
    })
}

/// Whether a bill is the parliament's own: before it now, or introduced in
/// it. A page that does not name its parliament is told by its first dated
/// stage, against the day the parliament was elected.
fn belongs(page: &BillPage, parliament: u32, since: &str) -> bool {
    page.listed
        || match page.parliament {
            Some(p) => p == parliament,
            None => page
                .stages
                .iter()
                .filter_map(|s| iso_date(&s.value))
                .min()
                .is_some_and(|first| first.as_str() >= since),
        }
}

/// A bill as the roll records it, from its page and from whether it was on
/// the Current Bills list when the page was read.
fn roll_bill(page: &BillPage) -> RollBill {
    let (title, number) = split_heading(&page.heading);
    let origin = origin(page);
    let first = origin.unwrap_or(House::Assembly);
    let passed = |house| stage_date(page, house, "third reading").is_some();
    let act = page
        .stages
        .iter()
        .find_map(|s| act_number(&s.label).or_else(|| act_number(&s.value)));
    let assented = act.is_some()
        || page
            .stages
            .iter()
            .any(|s| stage_key(&s.label) == "royal assent" && iso_date(&s.value).is_some());
    let negatived = page
        .stages
        .iter()
        .any(|s| stage_key(&s.label) == "negatived" && iso_date(&s.value).is_some());
    let stage = if assented {
        4
    } else if passed(first) && passed(first.other()) {
        3
    } else if passed(first) {
        2
    } else {
        1
    };
    // An Assembly bill the Council amended goes back to the Assembly. The
    // table has no row for the Council agreeing to the Assembly's
    // amendments to a Council bill, so that wait cannot be told.
    let amendments_pending = first == House::Assembly
        && page.stages.iter().any(|s| {
            s.house == House::Council
                && stage_key(&s.label) == "amended"
                && s.value.eq_ignore_ascii_case("yes")
        })
        && stage_date(page, House::Assembly, "lc amendments agreed to").is_none();
    let status = if assented {
        "Assented".to_string()
    } else if negatived {
        "Negatived".to_string()
    } else if !page.listed {
        // Off the list with no assent: lapsed or withdrawn, and the page
        // does not say which.
        "No longer before Parliament".to_string()
    } else {
        match (stage, origin) {
            (3, _) if amendments_pending => {
                format!("Before the {ASSEMBLY} for the Council's amendments")
            }
            (3, _) => "Awaiting Royal Assent".to_string(),
            (2, _) => format!("Before the {}", first.other().name()),
            (_, Some(house)) => format!("Before the {}", house.name()),
            (_, None) => "Before Parliament".to_string(),
        }
    };
    let document = |wanted: &[&str]| {
        page.documents
            .iter()
            .find(|d| {
                let name = d.name.to_lowercase();
                wanted.iter().any(|w| name.contains(w))
            })
            .map(|d| d.url.clone())
    };
    RollBill {
        title,
        number,
        origin: origin.map(|house| house.name().to_string()),
        sponsor: page.introduced_by.clone(),
        kind: page.introduced_by.as_deref().map(|by| kind(by).to_string()),
        status,
        stage,
        stopped: !assented && (negatived || !page.listed),
        timeline: timeline(page, first),
        source: page.url.clone(),
        text: document(&["text of bill as introduced"]).or_else(|| document(&["text of bill"])),
        // Tasmania publishes clause notes rather than an explanatory
        // memorandum; they do the same work.
        explanatory: document(&["clause notes", "notes on clauses"]),
        act,
    }
}

/// The house a bill began in. The page does not say, so it is the house
/// that read it a first time first; where both did on the same day, the
/// one that passed it first, and the Assembly, where most bills begin,
/// when neither has.
fn origin(page: &BillPage) -> Option<House> {
    let began = |house| stage_date(page, house, "first reading");
    let passed = |house| stage_date(page, house, "third reading");
    match (began(House::Assembly), began(House::Council)) {
        (Some(assembly), Some(council)) if assembly != council => Some(if assembly < council {
            House::Assembly
        } else {
            House::Council
        }),
        (Some(_), Some(_)) => match (passed(House::Assembly), passed(House::Council)) {
            (assembly, Some(council)) if assembly.as_ref().is_none_or(|a| council < *a) => {
                Some(House::Council)
            }
            _ => Some(House::Assembly),
        },
        (Some(_), None) => Some(House::Assembly),
        (None, Some(_)) => Some(House::Council),
        (None, None) => None,
    }
}

/// Every dated stage, oldest first. Stages on the same day stay in the
/// order the bill took them: through the house it began in, then the
/// other, then the Assembly's agreeing to the Council's amendments and the
/// assent, which the table keeps in the Assembly's column.
fn timeline(page: &BillPage, first: House) -> Vec<TimelineStep> {
    let concluding = |s: &Stage| {
        matches!(
            stage_key(&s.label).as_str(),
            "lc amendments agreed to" | "royal assent"
        )
    };
    let mut steps = Vec::new();
    for house in [Some(first), Some(first.other()), None] {
        let stages = page.stages.iter().filter(|s| match house {
            Some(house) => s.house == house && !concluding(s),
            None => concluding(s),
        });
        for stage in stages {
            let Some(date) = iso_date(&stage.value) else {
                continue;
            };
            let event = if stage_key(&stage.label) == "royal assent" {
                "Royal Assent".to_string()
            } else {
                format!("{} ({})", plain_stage(&stage.label), stage.house.name())
            };
            steps.push(TimelineStep { date, event });
        }
    }
    steps.sort_by(|a, b| a.date.cmp(&b.date));
    steps
}

/// A stage as the timeline words it. The table numbers and abbreviates
/// some ("2nd Reading committed", "LC amendments agreed to"); these say
/// them in full. Any other keeps the table's own words.
fn plain_stage(label: &str) -> String {
    match stage_key(label).as_str() {
        "2nd reading moved" => "Second reading moved",
        "2nd reading agreed" | "2nd reading committed" => "Second reading agreed",
        "committee" => "Committee stage",
        "reported" => "Reported from committee",
        "lc amendments agreed to" => "Legislative Council's amendments agreed to",
        _ => label,
    }
    .to_string()
}

fn stage_key(label: &str) -> String {
    label.to_lowercase()
}

/// The day a house passed a stage, where the table gives one.
fn stage_date(page: &BillPage, house: House, stage: &str) -> Option<String> {
    page.stages
        .iter()
        .filter(|s| s.house == house && stage_key(&s.label) == stage)
        .find_map(|s| iso_date(&s.value))
}

/// A table date, "16/06/2026", as 2026-06-16.
fn iso_date(value: &str) -> Option<String> {
    let date = DATE.captures(value)?;
    NaiveDate::from_ymd_opt(
        date[3].parse().ok()?,
        date[2].parse().ok()?,
        date[1].parse().ok()?,
    )
    .map(|d| d.format("%Y-%m-%d").to_string())
}

fn act_number(text: &str) -> Option<String> {
    let act = ACT.captures(text.trim())?;
    Some(format!(
        "Act {} of {}",
        act[1].parse::<u32>().ok()?,
        &act[2]
    ))
}

/// A heading's title and its number, "21 of 2026", however the number was
/// typed.
fn split_heading(heading: &str) -> (String, Option<String>) {
    let heading = heading.split_whitespace().collect::<Vec<_>>().join(" ");
    match NUMBER.captures(&heading) {
        Some(found) => {
            let number = found[1]
                .parse::<u32>()
                .ok()
                .map(|n| format!("{n} of {}", &found[2]));
            let title = heading[..found.get(0).map_or(heading.len(), |m| m.start())].trim();
            (title.to_string(), number)
        }
        None => (heading, None),
    }
}

/// "Government" for a bill introduced by a portfolio, "Private member's"
/// for one a member introduced in their own name.
fn kind(introduced_by: &str) -> &'static str {
    if MEMBER.is_match(introduced_by.trim()) {
        "Private member's"
    } else {
        "Government"
    }
}

/// A page without the menus and footer around it, which link all over the
/// site.
fn main_content(html: &str) -> &str {
    let start = html.find("<main").unwrap_or(0);
    let end = html[start..]
        .find("</main>")
        .map_or(html.len(), |end| start + end);
    &html[start..end]
}

/// From a heading's words to the end of the list under it.
fn section<'a>(html: &'a str, heading: &str) -> Option<&'a str> {
    let rest = &html[html.find(heading)? + heading.len()..];
    let end = ["</ul>", "<h2"]
        .iter()
        .filter_map(|end| rest.find(end))
        .min()
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

fn text_of(pattern: &Regex, html: &str) -> Option<String> {
    pattern
        .captures(html)
        .map(|found| strip_html(&found[1]))
        .filter(|text| !text.is_empty())
}

/// A link's full address, written the same way from every list that links
/// the bill, so the lists agree on which bills they share.
fn resolve(base: &str, href: &str) -> Result<String> {
    let href = href.trim().replace("&amp;", "&");
    let mut url = reqwest::Url::parse(base)
        .and_then(|base| base.join(&href))
        .with_context(|| format!("a link to {href:?} on {base}"))?;
    url.set_fragment(None);
    let path = url.path().trim_end_matches('/').to_string();
    if !path.is_empty() {
        url.set_path(&path);
    }
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::LocalStore;
    use crate::test_http::{Response, TestServer};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// The table's rows, by their House of Assembly names.
    const STAGES: [&str; 11] = [
        "First reading",
        "Resumed",
        "2nd Reading moved",
        "2nd Reading Agreed",
        "Committee",
        "Reported",
        "Amended",
        "Third reading",
        "Negatived",
        "LC amendments agreed to",
        "Royal Assent",
    ];

    /// A bill's page laid out as the Parliament lays it out, cut to what is
    /// read: every row of the table, with an entry wherever a test gives one.
    #[derive(Default)]
    struct Page<'a> {
        heading: &'a str,
        introduced_by: &'a str,
        /// "52nd".
        parliament: &'a str,
        assembly: &'a [(&'a str, &'a str)],
        council: &'a [(&'a str, &'a str)],
        /// "19 of 2026", once the bill is law.
        act: &'a str,
        documents: &'a [(&'a str, &'a str)],
    }

    impl Page<'_> {
        fn html(&self) -> String {
            let entry = |column: &[(&str, &str)], stage: &str| {
                column
                    .iter()
                    .find(|(s, _)| *s == stage)
                    .map_or("&nbsp;", |(_, entry)| *entry)
                    .to_string()
            };
            let mut rows = String::new();
            for stage in STAGES {
                let marked = |label: &str| match label {
                    "2nd Reading moved" => "2nd Reading moved <sup>*</sup>".to_string(),
                    "Amended" => "Amended <sup>#</sup>".to_string(),
                    _ => label.to_string(),
                };
                let (council, council_entry) = match stage {
                    "2nd Reading Agreed" => (
                        "2nd Reading committed".to_string(),
                        entry(self.council, stage),
                    ),
                    "LC amendments agreed to" => (format!("Act {}", self.act), "&nbsp;".into()),
                    "Royal Assent" => (String::new(), String::new()),
                    _ => (marked(stage), entry(self.council, stage)),
                };
                rows.push_str(&format!(
                    "<tr>\n<td>{}</td>\n<td>\n {} \n</td>\n<td>{council}</td>\n<td>\n {council_entry} \n</td>\n</tr>\n",
                    marked(stage),
                    entry(self.assembly, stage),
                ));
            }
            let parliament = match self.parliament {
                "" => String::new(),
                ordinal => {
                    format!(
                        "<p>Bill first introduced in the {ordinal} Parliament, 1st Session.</p>"
                    )
                }
            };
            let documents: String = self
                .documents
                .iter()
                .map(|(name, href)| format!(r#"<li><a href="{href}">{name}</a></li>"#))
                .collect();
            format!(
                r#"<nav><ul><li><a href="/tas/members">Members</a></li></ul></nav>
<main id="main" class="content">
<h1 class="content__title">{}</h1>
<div class="col">
    <p><strong>Last updated:</strong> 15 Oct 2026</p>
    <p><strong>Introduced by:</strong> {}</p>
        {parliament}
</div>
<table class="">
  <thead><tr><th>House of Assembly - stage</th><th>Date passed </th><th>Legislative Council - stage</th><th>Date passed</th></tr></thead>
  <tbody>
{rows}  </tbody>
</table>
<div class="col"><h2>Related Documents</h2><ul>{documents}</ul></div>
<div><p>Note:</p><p>Date shown is when the Bill passes the particular stage.</p></div>
</main>"#,
                self.heading, self.introduced_by
            )
        }
    }

    const ASSENTED: &str = "/tas/bills/bills2026/taxation-and-related-legislation-miscellaneous-amendments-bill-2026-21-0f-2026";
    const PRIVATE: &str =
        "/tas/bills/bills2026/mineral-resources-legislation-ban-on-fracking-bill-2026";
    const FROM_COUNCIL: &str = "/tas/bills/bills2026/example-council-bill-2026-30-of-2026";
    const NEGATIVED: &str = "/tas/bills/bills2025/example-negatived-bill-2025-bill-55";
    const LAPSED: &str = "/tas/bills/bills2025/example-lapsed-bill-2026";
    const LAST_PARLIAMENT: &str =
        "/tas/bills/bills2025/appropriation-bill-no.-1-2025-bill-26-of-2025";
    const PDF: &str = "https://www.parliament.tas.gov.au/__data/assets/pdf_file";

    /// The Parliament's bills pages on a given night. On the first, the
    /// Council's bill is before the Assembly; from the second it is law and
    /// off the Current Bills list. On the fourth the private member's bill's
    /// page will not load.
    fn site(base: &str, path: &str, night: usize) -> Option<String> {
        let current: &[&str] = if night == 1 {
            &[PRIVATE, FROM_COUNCIL]
        } else {
            &[PRIVATE]
        };
        if night == 4 && path == PRIVATE {
            return None;
        }
        let page = match path {
            "/tas/bills" => {
                let current: String = current
                    .iter()
                    .map(|path| {
                        format!(r#"<div style="border-top:1px solid #ccc"><a href="{base}{path}">A bill</a></div>"#)
                    })
                    .collect();
                // The menu's "2026" leads nowhere: only the page's own list
                // of years is read.
                return Some(format!(
                    r#"<nav><ul><li><a href="{base}/tas/bills/bills2026/menu">2026</a></li></ul></nav>
<main id="main" class="content"><h1 class="sr-only">Bills</h1>
<ul style="margin-top:0px"><h2>Current Bills (Alphabetical Order)</h2><div>{current}</div></ul>
<h2>Annual&nbsp;List of Bills</h2>
<div class="ui menu"><a class="item" href="{base}/tas/bills/bills-introduced-into-parliament-2026">2026</a><a class="item" href="/tas/bills/bills-introduced-into-parliament-2025">2025</a><a class="item" href="{base}/tas/bills/bills-introduced-into-parliament-2024">2024</a><a
          class="item" href="{base}/tas/bills/bills2022/billweb2022">
            2022
            </a></div>
<h2>Information</h2><ul><li><a href="https://www.legislation.tas.gov.au/">Current Tasmanian Legislation</a></li></ul>
</main>"#
                ));
            }
            "/tas/bills/bills-introduced-into-parliament-2025"
            | "/tas/bills/bills-introduced-into-parliament-2026" => {
                let bills = if path.ends_with("2025") {
                    [LAST_PARLIAMENT, LAPSED, NEGATIVED]
                } else {
                    [FROM_COUNCIL, PRIVATE, ASSENTED]
                };
                let items: String = bills
                    .iter()
                    .map(|path| format!(r#"<li><a href="{base}{path}">A bill</a></li>"#))
                    .collect();
                return Some(format!(
                    r#"<nav><ul><li><a href="{base}/tas/visit">Visit</a></li></ul></nav>
<main id="main"><h1 class="sr-only">Bills introduced into Parliament</h1>
<div><p>Bill are listed in alphabetical order.</p></div><div><ul>
{items}</ul></div></main>"#
                ));
            }
            ASSENTED => Page {
                heading: "Taxation and Related Legislation (Miscellaneous Amendments) Bill 2026 (21 0f 2026)",
                introduced_by: "Treasurer",
                parliament: "52nd",
                assembly: &[
                    ("First reading", "16/06/2026"),
                    ("2nd Reading moved", "11/08/2026"),
                    ("2nd Reading Agreed", "11/08/2026"),
                    ("Amended", ""),
                    ("Third reading", "11/08/2026"),
                    ("LC amendments agreed to", ""),
                    ("Royal Assent", "09/10/2026"),
                ],
                council: &[
                    ("First reading", "11/08/2026"),
                    ("2nd Reading moved", "02/09/2026"),
                    ("2nd Reading Agreed", "02/09/2026"),
                    ("Committee", "02/09/2026"),
                    ("Reported", "02/09/2026"),
                    ("Amended", "No"),
                    ("Third reading", "03/09/2026"),
                ],
                act: "19 of 2026",
                documents: &[
                    ("Text of Bill as introduced", &format!("{PDF}/0019/107191/21-of-2026.pdf")),
                    ("Reprint of Bill", &format!("{PDF}/0032/109859/Bill-21-of-2026-2nd-Print.pdf")),
                    ("Second reading (House of Assembly)", &format!("{PDF}/0021/107265/Second-Reading-Speech.pdf")),
                    ("Clause notes", &format!("{PDF}/0020/107264/Notes-on-Clauses.pdf")),
                    ("Fact sheet", &format!("{PDF}/0019/107263/Legislation-Fact-Sheet.pdf")),
                ],
            }
            .html(),
            PRIVATE => Page {
                heading: "Mineral Resources Legislation (Ban on Fracking) Bill 2026 (4 of 2026)",
                introduced_by: "Ms Tabatha Badger MP",
                parliament: "52nd",
                assembly: &[("First reading", "05/03/2026")],
                documents: &[
                    ("Text of Bill as introduced", &format!("{PDF}/0015/102651/4-of-2026.pdf")),
                    ("Clause notes", &format!("{PDF}/0016/102652/Clause-Notes.pdf")),
                    ("Fact sheet", &format!("{PDF}/0017/102653/Fact-Sheet.pdf")),
                ],
                ..Default::default()
            }
            .html(),
            FROM_COUNCIL => {
                let council = [
                    ("First reading", "01/04/2026"),
                    ("2nd Reading moved", "14/04/2026"),
                    ("2nd Reading Agreed", "14/04/2026"),
                    ("Third reading", "15/04/2026"),
                ];
                let law = [
                    ("First reading", "15/04/2026"),
                    ("2nd Reading moved", "28/04/2026"),
                    ("2nd Reading Agreed", "28/04/2026"),
                    ("Third reading", "29/04/2026"),
                    ("Royal Assent", "12/05/2026"),
                ];
                Page {
                    heading: "Example Council Bill 2026 (30 of 2026)",
                    introduced_by: "Leader of the Government in the Legislative Council",
                    parliament: "52nd",
                    assembly: if night == 1 { &law[..1] } else { &law },
                    council: &council,
                    act: if night == 1 { "" } else { "20 of 2026" },
                    ..Default::default()
                }
                .html()
            }
            NEGATIVED => Page {
                heading: "Example Negatived Bill 2025 (55 of 2025)",
                introduced_by: "Mr Craig Garland MP",
                parliament: "52nd",
                assembly: &[("First reading", "02/09/2025"), ("Third reading", "18/09/2025")],
                council: &[
                    ("First reading", "23/09/2025"),
                    ("2nd Reading moved", "14/10/2025"),
                    ("Negatived", "15/10/2025"),
                ],
                ..Default::default()
            }
            .html(),
            LAPSED => Page {
                heading: "Example Lapsed Bill 2025 (60 of 2025)",
                introduced_by: "Attorney-General",
                parliament: "52nd",
                assembly: &[("First reading", "04/11/2025")],
                ..Default::default()
            }
            .html(),
            LAST_PARLIAMENT => Page {
                heading: "Appropriation Bill (No. 1) 2025 (Bill 26 of 2025)",
                introduced_by: "Treasurer",
                parliament: "51st",
                assembly: &[("First reading", "29/05/2025")],
                ..Default::default()
            }
            .html(),
            _ => return None,
        };
        Some(page)
    }

    fn server(night: Arc<AtomicUsize>) -> TestServer {
        TestServer::start(move |req| {
            let base = req
                .header("host")
                .map(|h| format!("http://{h}"))
                .unwrap_or_default();
            match site(&base, &req.path, night.load(Ordering::SeqCst)) {
                Some(page) => Response::html(page),
                None => Response::status(404, "unexpected path"),
            }
        })
    }

    fn scratch_store(name: &str) -> Store {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/states-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        Store::Local(LocalStore::new(dir))
    }

    fn bill<'a>(roll: &'a BillRoll, number: &str) -> &'a RollBill {
        roll.bills
            .iter()
            .find(|b| b.number.as_deref() == Some(number))
            .unwrap_or_else(|| panic!("no bill {number} in {:?}", roll.bills))
    }

    fn step(date: &str, event: &str) -> TimelineStep {
        TimelineStep {
            date: date.to_string(),
            event: event.to_string(),
        }
    }

    #[tokio::test]
    async fn the_year_lists_lead_to_every_bill_of_the_current_parliament() {
        let server = server(Arc::new(AtomicUsize::new(1)));
        let store = scratch_store("tas-bills");
        let roll = read(&store, &Endpoints::at(&server.base), "2025-07-19")
            .await
            .expect("read");

        assert_eq!(roll.state, StateCode::TAS);
        assert_eq!(roll.publisher, "Parliament of Tasmania");
        assert_eq!(roll.source, format!("{}/tas/bills", server.base));
        let numbers: Vec<&str> = roll
            .bills
            .iter()
            .filter_map(|b| b.number.as_deref())
            .collect();
        assert_eq!(
            numbers,
            [
                "60 of 2025",
                "55 of 2025",
                "30 of 2026",
                "4 of 2026",
                "21 of 2026"
            ],
            "the last parliament's bill on the 2025 list is left out"
        );
        let paths: Vec<String> = server.requests().iter().map(|r| r.path.clone()).collect();
        assert!(
            paths.iter().all(|p| !p.contains("2024")
                && !p.contains("billweb")
                && !p.ends_with("/menu")
                && !p.ends_with("/visit")),
            "years before the parliament's, and menus, are not read: {paths:?}"
        );

        let assented = bill(&roll, "21 of 2026");
        assert_eq!(
            assented,
            &RollBill {
                title: "Taxation and Related Legislation (Miscellaneous Amendments) Bill 2026"
                    .to_string(),
                number: Some("21 of 2026".to_string()),
                origin: Some("House of Assembly".to_string()),
                sponsor: Some("Treasurer".to_string()),
                kind: Some("Government".to_string()),
                status: "Assented".to_string(),
                stage: 4,
                stopped: false,
                timeline: vec![
                    step("2026-06-16", "First reading (House of Assembly)"),
                    step("2026-08-11", "Second reading moved (House of Assembly)"),
                    step("2026-08-11", "Second reading agreed (House of Assembly)"),
                    step("2026-08-11", "Third reading (House of Assembly)"),
                    step("2026-08-11", "First reading (Legislative Council)"),
                    step("2026-09-02", "Second reading moved (Legislative Council)"),
                    step("2026-09-02", "Second reading agreed (Legislative Council)"),
                    step("2026-09-02", "Committee stage (Legislative Council)"),
                    step(
                        "2026-09-02",
                        "Reported from committee (Legislative Council)"
                    ),
                    step("2026-09-03", "Third reading (Legislative Council)"),
                    step("2026-10-09", "Royal Assent"),
                ],
                source: format!("{}{ASSENTED}", server.base),
                text: Some(format!("{PDF}/0019/107191/21-of-2026.pdf")),
                explanatory: Some(format!("{PDF}/0020/107264/Notes-on-Clauses.pdf")),
                act: Some("Act 19 of 2026".to_string()),
            }
        );

        let private = bill(&roll, "4 of 2026");
        assert_eq!(private.sponsor.as_deref(), Some("Ms Tabatha Badger MP"));
        assert_eq!(private.kind.as_deref(), Some("Private member's"));
        assert_eq!(private.origin.as_deref(), Some("House of Assembly"));
        assert_eq!(private.status, "Before the House of Assembly");
        assert_eq!((private.stage, private.stopped), (1, false));
        assert_eq!(
            private.timeline,
            [step("2026-03-05", "First reading (House of Assembly)")]
        );
        assert_eq!(
            private.text.as_deref(),
            Some(format!("{PDF}/0015/102651/4-of-2026.pdf").as_str())
        );
        assert_eq!(
            private.explanatory.as_deref(),
            Some(format!("{PDF}/0016/102652/Clause-Notes.pdf").as_str())
        );
        assert!(private.act.is_none(), "an empty \"Act\" is no Act");

        let from_council = bill(&roll, "30 of 2026");
        assert_eq!(
            from_council.origin.as_deref(),
            Some("Legislative Council"),
            "the right-hand column read it first"
        );
        assert_eq!(from_council.kind.as_deref(), Some("Government"));
        assert_eq!(from_council.status, "Before the House of Assembly");
        assert_eq!((from_council.stage, from_council.stopped), (2, false));
        assert_eq!(
            from_council.timeline[3..],
            [
                step("2026-04-15", "Third reading (Legislative Council)"),
                step("2026-04-15", "First reading (House of Assembly)"),
            ],
            "the same day's stages in the order the bill took them"
        );

        let negatived = bill(&roll, "55 of 2025");
        assert_eq!(negatived.status, "Negatived");
        assert_eq!((negatived.stage, negatived.stopped), (2, true));
        assert_eq!(
            negatived.timeline.last(),
            Some(&step("2025-10-15", "Negatived (Legislative Council)"))
        );

        let lapsed = bill(&roll, "60 of 2025");
        assert_eq!(lapsed.status, "No longer before Parliament");
        assert_eq!((lapsed.stage, lapsed.stopped), (1, true));
        assert_eq!(lapsed.kind.as_deref(), Some("Government"));

        let kept: BillPage = store
            .get_json(&format!(
                "{KEPT}bills2026/taxation-and-related-legislation-miscellaneous-amendments-bill-2026-21-0f-2026.json"
            ))
            .await
            .unwrap()
            .expect("the page is kept under its address");
        assert_eq!(kept.parliament, Some(52));
        assert_eq!(kept.last_updated.as_deref(), Some("15 Oct 2026"));
        assert!(!kept.listed);
    }

    #[tokio::test]
    async fn a_bill_off_the_current_list_is_read_once_more_then_kept() {
        let night = Arc::new(AtomicUsize::new(1));
        let server = server(Arc::clone(&night));
        let endpoints = Endpoints::at(&server.base);
        let store = scratch_store("tas-bills-nights");
        let paths_after = |seen: usize| -> Vec<String> {
            server.requests()[seen..]
                .iter()
                .map(|r| r.path.clone())
                .collect()
        };

        let first = read(&store, &endpoints, "2025-07-19").await.expect("first");
        assert_eq!(server.hits(), 9, "the bills page, two years and six bills");
        assert_eq!(
            bill(&first, "30 of 2026").status,
            "Before the House of Assembly"
        );

        // It became law and left the list; the copy read while it was on
        // the list is out of date, so the page is read again.
        night.store(2, Ordering::SeqCst);
        let seen = server.hits();
        let second = read(&store, &endpoints, "2025-07-19")
            .await
            .expect("second");
        assert_eq!(
            paths_after(seen),
            [
                "/tas/bills",
                "/tas/bills/bills-introduced-into-parliament-2025",
                "/tas/bills/bills-introduced-into-parliament-2026",
                FROM_COUNCIL,
                PRIVATE,
            ],
            "an assented bill's kept page stands; the current bill is read again"
        );
        let from_council = bill(&second, "30 of 2026");
        assert_eq!(from_council.status, "Assented");
        assert_eq!((from_council.stage, from_council.stopped), (4, false));
        assert_eq!(from_council.act.as_deref(), Some("Act 20 of 2026"));
        assert_eq!(bill(&second, "21 of 2026").status, "Assented");

        night.store(3, Ordering::SeqCst);
        let seen = server.hits();
        let third = read(&store, &endpoints, "2025-07-19").await.expect("third");
        assert_eq!(
            paths_after(seen),
            [
                "/tas/bills",
                "/tas/bills/bills-introduced-into-parliament-2025",
                "/tas/bills/bills-introduced-into-parliament-2026",
                PRIVATE,
            ],
            "once read off the list, a bill is not read again"
        );
        assert_eq!(third.bills, second.bills);

        night.store(4, Ordering::SeqCst);
        let fourth = read(&store, &endpoints, "2025-07-19")
            .await
            .expect("a current bill whose page will not load is no failure");
        assert_eq!(
            fourth.bills, third.bills,
            "the bill stands as it was last read"
        );
    }

    #[test]
    fn a_heading_gives_the_title_and_the_number_however_it_was_typed() {
        let cases = [
            (
                "Taxation and Related Legislation (Miscellaneous Amendments) Bill 2026 (21 0f 2026)",
                "Taxation and Related Legislation (Miscellaneous Amendments) Bill 2026",
                Some("21 of 2026"),
            ),
            (
                "Appropriation Bill (No. 2) 2025 (Bill 64 of 2025)",
                "Appropriation Bill (No. 2) 2025",
                Some("64 of 2025"),
            ),
            (
                "Community Protection  (Offender Reporting) Amendment Bill 2025 (61 of 2025)",
                "Community Protection (Offender Reporting) Amendment Bill 2025",
                Some("61 of 2025"),
            ),
            (
                "Residential Parks Bill 2026 (2of 2026)",
                "Residential Parks Bill 2026",
                Some("2 of 2026"),
            ),
            (
                "Macquarie Point Planning Permit Bill 2025",
                "Macquarie Point Planning Permit Bill 2025",
                None,
            ),
        ];
        for (heading, title, number) in cases {
            let (read_title, read_number) = split_heading(heading);
            assert_eq!(read_title, title, "{heading}");
            assert_eq!(read_number.as_deref(), number, "{heading}");
        }
    }

    #[test]
    fn a_portfolio_brings_in_government_bills_and_a_member_their_own() {
        for portfolio in [
            "Treasurer",
            "Premier",
            "Attorney-General",
            "Minister for Health",
            "Leader of the Government in the Legislative Council",
        ] {
            assert_eq!(kind(portfolio), "Government", "{portfolio}");
        }
        for member in [
            "Ms Tabatha Badger MP",
            "Dr Rosalie Woodruff MP",
            "Hon Ruth Forrest MLC",
            "Meg Webb MLC",
            "Leader of the Opposition",
        ] {
            assert_eq!(kind(member), "Private member's", "{member}");
        }
    }

    fn page_of(listed: bool, stages: &[(House, &str, &str)]) -> BillPage {
        BillPage {
            url: "https://www.parliament.tas.gov.au/bills/bills2026/example".to_string(),
            listed,
            heading: "Example Bill 2026 (1 of 2026)".to_string(),
            last_updated: None,
            introduced_by: Some("Treasurer".to_string()),
            parliament: Some(52),
            stages: stages
                .iter()
                .map(|(house, label, value)| Stage {
                    house: *house,
                    label: label.to_string(),
                    value: value.to_string(),
                })
                .collect(),
            documents: Vec::new(),
        }
    }

    #[test]
    fn the_house_a_bill_began_in_read_it_first_or_on_a_tie_passed_it_first() {
        use House::{Assembly, Council};
        let origin_of = |stages: &[(House, &str, &str)]| origin(&page_of(true, stages));
        assert_eq!(
            origin_of(&[
                (Assembly, "First reading", "01/05/2026"),
                (Council, "First reading", "20/05/2026"),
            ]),
            Some(Assembly)
        );
        assert_eq!(
            origin_of(&[
                (Assembly, "First reading", "20/05/2026"),
                (Council, "First reading", "1/5/2026"),
            ]),
            Some(Council),
            "the Council's column is on the right, and dates are compared, not places"
        );
        assert_eq!(
            origin_of(&[
                (Assembly, "First reading", "20/05/2026"),
                (Council, "First reading", "20/05/2026"),
                (Council, "Third reading", "20/05/2026"),
            ]),
            Some(Council),
            "both read it on one day, and the Council passed it"
        );
        assert_eq!(
            origin_of(&[
                (Assembly, "First reading", "20/05/2026"),
                (Council, "First reading", "20/05/2026"),
            ]),
            Some(Assembly)
        );
        assert_eq!(origin_of(&[(Assembly, "First reading", "&nbsp;")]), None);
    }

    #[test]
    fn a_bill_both_houses_have_passed_waits_on_assent_or_the_council_s_amendments() {
        use House::{Assembly, Council};
        let passed = [
            (Assembly, "First reading", "01/05/2026"),
            (Assembly, "Third reading", "07/05/2026"),
            (Council, "First reading", "07/05/2026"),
            (Council, "Third reading", "20/05/2026"),
        ];
        let waiting = roll_bill(&page_of(true, &passed));
        assert_eq!(
            (waiting.stage, waiting.status.as_str()),
            (3, "Awaiting Royal Assent")
        );

        let amended = [&passed[..], &[(Council, "Amended", "Yes")]].concat();
        assert_eq!(
            roll_bill(&page_of(true, &amended)).status,
            "Before the House of Assembly for the Council's amendments"
        );

        let agreed = [
            &amended[..],
            &[(Assembly, "LC amendments agreed to", "20/05/2026")],
        ]
        .concat();
        let agreed = roll_bill(&page_of(true, &agreed));
        assert_eq!(agreed.status, "Awaiting Royal Assent");
        assert_eq!(
            agreed.timeline[3..],
            [
                step("2026-05-20", "Third reading (Legislative Council)"),
                step(
                    "2026-05-20",
                    "Legislative Council's amendments agreed to (House of Assembly)"
                ),
            ]
        );
    }

    #[test]
    fn a_page_that_names_no_parliament_is_told_by_its_first_reading() {
        let mut page = page_of(false, &[(House::Assembly, "First reading", "04/09/2025")]);
        page.parliament = None;
        assert!(belongs(&page, 52, "2025-07-19"));
        assert!(!belongs(&page, 52, "2025-09-05"));
        page.parliament = Some(51);
        assert!(!belongs(&page, 52, "2025-07-19"));
        page.listed = true;
        assert!(
            belongs(&page, 52, "2025-07-19"),
            "a current bill is always kept"
        );
    }

    #[test]
    fn a_bills_page_without_its_lists_is_refused() {
        let source = "https://www.parliament.tas.gov.au/bills";
        assert!(current_bills("<main><h2>Bills</h2></main>", source).is_err());
        let years = r#"<main><a class="item" href="/bills/bills-introduced-into-parliament-2026">2026</a></main>"#;
        assert_eq!(
            year_lists(years, source, 2026).unwrap(),
            ["https://www.parliament.tas.gov.au/bills/bills-introduced-into-parliament-2026"]
        );
        assert!(
            year_lists(years, source, 2025).is_err(),
            "the parliament's first year must be listed"
        );
    }
}
