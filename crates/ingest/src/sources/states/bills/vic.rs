//! Victoria. Bills are kept by the legislation website, which the Office of
//! the Chief Parliamentary Counsel runs, not by the Parliament's. The site's
//! "All bills" page is a search it sends to an index of its own; the same
//! search, narrowed to the years since the election, lists the term's bills,
//! and each bill's page data dates its steps through both houses, names who
//! introduced it and links its prints.
//!
//! The site's copyright is restrictive, so only facts and links are kept: the
//! search leaves the documents' text out, as the site's own does, and a page
//! is kept without the notes and speech links that come with its steps.

use super::{BillRoll, RollBill};
use crate::endpoints::Endpoints;
use crate::http::{fetch_json, FetchOpts};
use crate::sources::aph_bills::strip_html;
use crate::store::Store;
use anyhow::{anyhow, bail, Context, Result};
use chrono::Datelike;
use pollywiki_schema::{StateCode, TimelineStep};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::LazyLock;

const ASSEMBLY: &str = "Legislative Assembly";
const COUNCIL: &str = "Legislative Council";

/// The index behind the site's searches.
const INDEX: &str = "content-legislation-vic-gov-au__production__sapi_node";

/// The site's own page of results.
const PAGE_SIZE: usize = 40;

/// The site's robots.txt asks for two seconds between requests.
const SPACING_MS: u64 = 2000;

/// "Introduced by Hon Jane Citizen", once the page's markup is gone.
static INTRODUCED_BY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^introduced by\s+(.+)$").unwrap());

/// "Act number 13/2024", undated, in the page's Royal Assent part.
static ACT_NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^act\s+(?:number|no\.?)\s*(\d.*)$").unwrap());

/// The status words for a bill that went no further.
static STOPPED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:lapsed|withdrawn|negatived|discharged|defeated)\b").unwrap()
});

#[derive(Deserialize)]
struct Search {
    hits: Hits,
}

#[derive(Deserialize)]
struct Hits {
    total: Total,
    #[serde(default)]
    hits: Vec<Hit>,
}

#[derive(Deserialize)]
struct Total {
    value: usize,
}

#[derive(Deserialize)]
struct Hit {
    #[serde(rename = "_source")]
    source: Listed,
}

/// A bill as the search lists it. The index holds every field as a list.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Listed {
    title: Vec<String>,
    nid: Vec<u64>,
    /// "/site-6/bills/<slug>": the index files each page under its site.
    url: Vec<String>,
    field_legislation_status: Vec<String>,
    bill_assembly_section_introduced_by: Vec<String>,
    bill_council_section_introduced_by: Vec<String>,
    /// When the bill's page last changed, which is when it is due a read.
    changed: Vec<String>,
}

impl Listed {
    /// The bill's page on the site, "/bills/<slug>".
    fn path(&self) -> Option<String> {
        static PATH: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^(?:/site-\d+)?(/bills/[^/?#\s]+)/?$").unwrap());
        PATH.captures(self.url.first()?.trim())
            .map(|found| found[1].to_string())
    }
}

/// A bill's page data, as much of it as is kept. A reply with no title is
/// not a bill's page, so it is refused rather than kept as an empty one.
#[derive(Serialize, Deserialize)]
struct Page {
    title: String,
    #[serde(default)]
    status: String,
    changed: Option<String>,
    #[serde(default)]
    documents: Vec<Document>,
    #[serde(default)]
    sections: Vec<Section>,
}

/// One of the bill's prints, "Introduction print – Bill", in a format or two.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Document {
    title: String,
    docs: Vec<Doc>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Doc {
    url: String,
    extension: String,
}

/// The bill's way through one house, "First House: Legislative Assembly",
/// or its Royal Assent.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Section {
    title: String,
    label: String,
    readings: Vec<Reading>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Reading {
    fields: Vec<Field>,
}

/// A line of a reading: "Second reading passed" and its date, or an undated
/// fact such as who introduced the bill.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Field {
    title: String,
    date: String,
}

/// A bill's page as kept between runs, with the "changed" stamp it was read
/// at, so it is read again only once the list's stamp moves on.
#[derive(Serialize, Deserialize)]
struct Kept {
    changed: String,
    page: Page,
}

/// One line of a bill's page, flattened: what happened, in which house, and
/// the day, where the page dates it.
struct Line {
    house: Option<&'static str>,
    what: String,
    day: Option<String>,
}

impl Page {
    fn lines(&self) -> Vec<Line> {
        self.sections
            .iter()
            .flat_map(|section| {
                let house = section.house();
                section
                    .readings
                    .iter()
                    .flat_map(|reading| &reading.fields)
                    .map(move |field| Line {
                        house,
                        what: strip_html(&field.title),
                        day: day(&field.date),
                    })
            })
            .filter(|line| !line.what.is_empty())
            .collect()
    }

    /// A print's link, from the first document whose title, lowercased and
    /// with plain hyphens for its dashes, `wanted` accepts.
    fn print(&self, wanted: impl Fn(&str) -> bool) -> Option<String> {
        self.documents
            .iter()
            .filter(|document| {
                wanted(
                    &document
                        .title
                        .to_lowercase()
                        .replace(['\u{2013}', '\u{2014}'], "-"),
                )
            })
            .find_map(Document::link)
    }
}

impl Document {
    /// The PDF where there is one, as anyone can open it, else the first
    /// format given.
    fn link(&self) -> Option<String> {
        let linked = || self.docs.iter().filter(|doc| !doc.url.is_empty());
        linked()
            .find(|doc| {
                doc.extension.eq_ignore_ascii_case("pdf")
                    || doc.url.to_lowercase().ends_with(".pdf")
            })
            .or_else(|| linked().next())
            .map(|doc| doc.url.clone())
    }
}

impl Section {
    /// The house a part of the page follows: labelled "Assembly" or
    /// "Council" and titled "First House: Legislative Assembly" and the like.
    /// The Royal Assent part follows neither.
    fn house(&self) -> Option<&'static str> {
        let named = format!("{} {}", self.label, self.title).to_lowercase();
        if named.contains("assembly") {
            Some(ASSEMBLY)
        } else if named.contains("council") {
            Some(COUNCIL)
        } else {
            None
        }
    }
}

pub async fn read(store: &Store, endpoints: &Endpoints, since: &str) -> Result<BillRoll> {
    let first_year: i32 = since
        .get(..4)
        .and_then(|year| year.parse().ok())
        .ok_or_else(|| anyhow!("billsSince \"{since}\" does not begin with a year"))?;
    let years: Vec<String> = (first_year..=melbourne_year())
        .map(|year| year.to_string())
        .collect();
    let listed = list(endpoints, &years).await?;
    store
        .put_json("raw/states/vic/bills-list.json", &listed)
        .await?;

    let mut bills = Vec::new();
    let mut pages_read = 0;
    for entry in &listed {
        let (Some(nid), Some(path)) = (entry.nid.first(), entry.path()) else {
            eprintln!(
                "vic-bills: {} has no page to read",
                entry.title.first().map_or("a listed bill", String::as_str)
            );
            continue;
        };
        let changed = entry.changed.first().map(String::as_str);
        let Some(page) = bill_page(store, endpoints, *nid, &path, changed, &mut pages_read).await?
        else {
            continue;
        };
        let bill = to_bill(entry, &page, format!("{}{path}", endpoints.vic_legislation));
        // The years searched reach back past the election to the start of
        // its year; a bill is the term's when it was first read in it.
        if bill
            .timeline
            .first()
            .is_some_and(|step| step.date.as_str() >= since)
        {
            bills.push(bill);
        }
    }
    println!(
        "vic-bills: {} listed, {pages_read} pages read, {} since {since}",
        listed.len(),
        bills.len()
    );
    Ok(BillRoll {
        state: StateCode::VIC,
        listed_at: crate::now_iso(),
        publisher: "Victorian Legislation".to_string(),
        source: format!("{}/bills/all-bills", endpoints.vic_legislation),
        bills,
    })
}

/// The year in Melbourne, which always turns over in summer time, eleven
/// hours ahead of UTC.
fn melbourne_year() -> i32 {
    (chrono::Utc::now() + chrono::Duration::hours(11)).year()
}

/// The search the "All bills" page sends, with "Year introduced" set to
/// every year of the term. The documents' text and the rendered pages stay
/// out of the results, as they do for the site's own.
fn search(years: &[String], from: usize) -> String {
    serde_json::json!({
        "query": { "bool": { "filter": [
            { "terms": { "type": ["bill"] } },
            { "terms": { "field_node_site": [6] } },
            { "terms": { "legislation_year": years } }
        ] } },
        "size": PAGE_SIZE,
        "from": from,
        "sort": [{ "title_az": "asc" }],
        "track_total_hits": true,
        "_source": { "excludes": ["es_attachment", "rendered_item"] }
    })
    .to_string()
}

/// Every bill the search lists for the given years, a page at a time until
/// its total is reached. A list that stops short is an error, so a search
/// that changes under the reader cannot pass for a shorter term.
async fn list(endpoints: &Endpoints, years: &[String]) -> Result<Vec<Listed>> {
    let url = format!(
        "{}/api/tide/elasticsearch/{INDEX}/_search",
        endpoints.vic_legislation
    );
    let mut listed: Vec<Listed> = Vec::new();
    let mut offset = 0;
    loop {
        let opts = FetchOpts {
            post_json: Some(search(years, offset)),
            ..endpoints.opts(SPACING_MS)
        };
        let found: Search = fetch_json(&url, &opts)
            .await
            .with_context(|| format!("searching {url}"))?;
        let total = found.hits.total.value;
        let count = found.hits.hits.len();
        offset += count;
        listed.extend(found.hits.hits.into_iter().map(|hit| hit.source));
        if count == 0 || offset >= total {
            // Paging by offset can repeat a bill across a page break. It is
            // kept once, and whichever bill it pushed out counts as missing.
            let mut seen = HashSet::new();
            listed.retain(|bill| bill.nid.first().is_none_or(|nid| seen.insert(*nid)));
            if listed.len() != total {
                bail!("{} of {total} bills came back", listed.len());
            }
            return Ok(listed);
        }
    }
}

/// A bill's page: the copy kept from an earlier run while the list's
/// "changed" stamp still matches it, else a fresh read. A page that cannot
/// be read falls back on the copy kept, however old, and is asked for again
/// next run; with no copy kept, the bill is left out until its page reads.
async fn bill_page(
    store: &Store,
    endpoints: &Endpoints,
    nid: u64,
    path: &str,
    changed: Option<&str>,
    pages_read: &mut usize,
) -> Result<Option<Page>> {
    let key = format!("raw/states/vic/bills/{nid}.json");
    // A copy that no longer parses is as good as none.
    let kept: Option<Kept> = store
        .get_raw(&key)
        .await?
        .and_then(|raw| serde_json::from_str(&raw).ok());
    if kept
        .as_ref()
        .is_some_and(|kept| changed == Some(kept.changed.as_str()))
    {
        return Ok(kept.map(|kept| kept.page));
    }
    let url = format!(
        "{}/api/tide/page?path={path}&site=6",
        endpoints.vic_legislation
    );
    match fetch_json::<Page>(&url, &endpoints.opts(SPACING_MS)).await {
        Ok(page) => {
            *pages_read += 1;
            // The page's own stamp is kept, not the list's: a page served
            // stale from a cache carries an older stamp than the list, so it
            // is read again next run.
            let fresh = Kept {
                changed: page
                    .changed
                    .clone()
                    .or_else(|| changed.map(str::to_string))
                    .unwrap_or_default(),
                page,
            };
            store.put_json(&key, &fresh).await?;
            Ok(Some(fresh.page))
        }
        Err(err) => {
            eprintln!("vic-bills: page for {path} failed - {err:#}");
            Ok(kept.map(|kept| kept.page))
        }
    }
}

fn to_bill(entry: &Listed, page: &Page, source: String) -> RollBill {
    let lines = page.lines();
    // The house it began in: the page's "First House", else the first house
    // it follows at all.
    let origin = page
        .sections
        .iter()
        .find(|section| section.title.to_lowercase().starts_with("first house"))
        .and_then(Section::house)
        .or_else(|| lines.iter().find_map(|line| line.house));
    // Whoever introduced it in the house it began in; the second house's
    // minister introduces it there in turn.
    let sponsor = lines
        .iter()
        .filter(|line| origin.is_none_or(|origin| line.house == Some(origin)))
        .find_map(|line| {
            INTRODUCED_BY
                .captures(&line.what)
                .map(|found| found[1].to_string())
        })
        .or_else(|| {
            match origin? {
                ASSEMBLY => entry.bill_assembly_section_introduced_by.first(),
                _ => entry.bill_council_section_introduced_by.first(),
            }
            .map(|name| strip_html(name))
        })
        .filter(|name| !name.is_empty());
    let act = lines.iter().find_map(|line| {
        ACT_NUMBER
            .captures(&line.what)
            .map(|found| format!("Act No. {}", found[1].trim()))
    });
    let status = entry
        .field_legislation_status
        .first()
        .map(|status| strip_html(status))
        .filter(|status| !status.is_empty())
        .unwrap_or_else(|| strip_html(&page.status));

    let dated = || lines.iter().filter(|line| line.day.is_some());
    let passed = |house: &str| {
        dated().any(|line| {
            let what = line.what.to_lowercase();
            line.house == Some(house) && what.contains("third reading") && what.contains("passed")
        })
    };
    let assented =
        act.is_some() || dated().any(|line| line.what.to_lowercase().contains("royal assent"));
    let stage = if assented {
        4
    } else if origin.is_some_and(|origin| passed(other_house(origin))) {
        3
    } else if origin.is_some_and(passed) {
        2
    } else {
        1
    };

    let mut timeline: Vec<TimelineStep> = dated()
        .filter_map(|line| {
            Some(TimelineStep {
                date: line.day.clone()?,
                event: match line.house {
                    Some(house) => format!("{} ({house})", line.what),
                    None => line.what.clone(),
                },
            })
        })
        .collect();
    // Oldest first. Steps of one day keep the page's order, which runs
    // through the first house before the second.
    timeline.sort_by(|a, b| a.date.cmp(&b.date));

    // The bill as introduced, and the explanatory memorandum that came with
    // it; a later print only where the first is missing.
    let text = page
        .print(|title| title.starts_with("introduction print") && !title.contains("explanatory"))
        .or_else(|| page.print(|title| title.ends_with("bill")));
    let explanatory = page
        .print(|title| title.starts_with("introduction print") && title.contains("explanatory"))
        .or_else(|| page.print(|title| title.contains("explanatory")));

    RollBill {
        title: entry
            .title
            .first()
            .map(|title| strip_html(title))
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| strip_html(&page.title)),
        // The site gives a bill no number of its own.
        number: None,
        origin: origin.map(str::to_string),
        sponsor,
        // Nor does it say whether a bill is the government's.
        kind: None,
        stopped: STOPPED.is_match(&status),
        // A page kept from an earlier run can be behind the list, whose
        // status words then say the bill has come further.
        stage: stage.max(stage_in(&status)),
        status,
        timeline,
        source,
        text,
        explanatory,
        act,
    }
}

fn other_house(house: &str) -> &'static str {
    if house == ASSEMBLY {
        COUNCIL
    } else {
        ASSEMBLY
    }
}

/// How far the status words alone say a bill has come: "Council - second
/// reading (passed Assembly)" has passed the house it began in.
fn stage_in(status: &str) -> u8 {
    let status = status.to_lowercase();
    if status.contains("assented") {
        4
    } else if status.contains("passed both") {
        3
    } else if status.contains("(passed assembly)") || status.contains("(passed council)") {
        2
    } else {
        1
    }
}

/// The day of a step as the page dates it: "2026-08-11T10:00:00+10:00",
/// in Melbourne time, is 2026-08-11. Undated lines carry "".
fn day(date: &str) -> Option<String> {
    static DAY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}").unwrap());
    DAY.find(date.trim())
        .map(|found| found.as_str().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::LocalStore;
    use crate::test_http::{Response, TestServer};
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};

    const SEARCH: &str = "/vic-legislation/api/tide/elasticsearch/content-legislation-vic-gov-au__production__sapi_node/_search";

    fn new_store(name: &str) -> Store {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/states-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        Store::Local(LocalStore::new(dir))
    }

    /// A bill as the site holds it: its entry in the search, and its page.
    struct OnSite {
        hit: Value,
        page: Value,
    }

    fn on_site(
        nid: u64,
        title: &str,
        status: &str,
        changed: &str,
        sections: Vec<Value>,
        documents: Vec<Value>,
    ) -> OnSite {
        let slug = title.to_lowercase().replace(' ', "-");
        OnSite {
            hit: json!({
                "_index": INDEX,
                "_id": format!("entity:node/{nid}:en"),
                "_score": null,
                "_source": {
                    "title": [title],
                    "nid": [nid],
                    "url": [format!("/site-6/bills/{slug}")],
                    "field_legislation_status": [status],
                    "type": ["bill"],
                    "field_node_site": [6],
                    "changed": [changed]
                }
            }),
            page: json!({
                "title": title,
                "changed": changed,
                "type": "bill",
                "status": status,
                "meta": { "url": format!("/bills/{slug}") },
                "documents": documents,
                "sections": sections
            }),
        }
    }

    fn house(title: &str, label: &str, readings: Vec<Value>) -> Value {
        json!({ "title": title, "label": label, "readings": readings })
    }

    fn reading(title: &str, fields: &[(&str, &str)]) -> Value {
        let fields: Vec<Value> = fields
            .iter()
            .map(|(what, date)| json!({ "title": what, "date": date, "description": "" }))
            .collect();
        json!({ "title": title, "fields": fields })
    }

    fn print(title: &str, files: &[&str]) -> Value {
        let docs: Vec<Value> = files
            .iter()
            .map(|file| {
                json!({
                    "name": file,
                    "url": format!("https://content.legislation.vic.gov.au/sites/default/files/bills/{file}"),
                    "extension": file.rsplit('.').next().unwrap_or("").to_lowercase()
                })
            })
            .collect();
        json!({ "title": title, "docs": docs })
    }

    fn pdf(file: &str) -> Option<String> {
        Some(format!(
            "https://content.legislation.vic.gov.au/sites/default/files/bills/{file}"
        ))
    }

    fn steps(steps: &[(&str, &str)]) -> Vec<TimelineStep> {
        steps
            .iter()
            .map(|(date, event)| TimelineStep {
                date: date.to_string(),
                event: event.to_string(),
            })
            .collect()
    }

    /// Through both houses and assented, with its Act number. Its prints
    /// give the Word file before the PDF, and a business program note comes
    /// after a later debate, as real pages have them.
    fn assented() -> OnSite {
        let mut bill = on_site(
            21001,
            "Statute Law Revision Bill 2024",
            "Passed and Assented to",
            "2024-05-03T11:55:54+10:00",
            vec![
                house(
                    "First House: Legislative Assembly",
                    "Assembly",
                    vec![
                        reading(
                            "First reading",
                            &[
                                ("Introduced by <strong>Hon Riley Okafor</strong>", ""),
                                ("First reading passed", "2024-02-20T11:00:00+11:00"),
                            ],
                        ),
                        reading(
                            "Second reading",
                            &[
                                ("Second reading moved", "2024-02-21T11:00:00+11:00"),
                                ("Debate resumed ", "2024-03-06T11:00:00+11:00"),
                                ("Government business program", "2024-03-05T11:00:00+11:00"),
                                ("Second reading passed", "2024-03-07T11:00:00+11:00"),
                            ],
                        ),
                        reading(
                            "Third reading",
                            &[("Third reading passed", "2024-03-07T11:00:00+11:00")],
                        ),
                    ],
                ),
                house(
                    "Second House: Legislative Council",
                    "Council",
                    vec![
                        reading(
                            "Message received from Assembly",
                            &[(
                                "Message received from Assembly",
                                "2024-03-07T11:00:00+11:00",
                            )],
                        ),
                        reading(
                            "First reading",
                            &[
                                ("Introduced by <strong>Hon Mei Tanaka</strong>", ""),
                                ("First reading passed", "2024-03-07T11:00:00+11:00"),
                            ],
                        ),
                        reading(
                            "Third reading",
                            &[("Third reading passed", "2024-04-18T10:00:00+10:00")],
                        ),
                    ],
                ),
                house(
                    "Royal Assent",
                    "",
                    vec![reading(
                        "",
                        &[
                            ("Royal Assent given", "2024-04-23T10:00:00+10:00"),
                            ("Act number 13/2024", ""),
                        ],
                    )],
                ),
            ],
            vec![
                print(
                    "Introduction print \u{2013} Bill",
                    &["601129bi1.DOCX", "601129bi1.pdf"],
                ),
                print(
                    "Introduction print \u{2013} Explanatory Memorandum",
                    &["601129exi1.DOCX", "601129exi1.pdf"],
                ),
                print("As passed print \u{2013} Bill", &["601129bs1.pdf"]),
            ],
        );
        bill.page["sections"][0]["readings"][1]["fields"][0]["description"] = json!(
            "<p><a href=\"https://hansard.parliament.vic.gov.au/x\">Click Here to View the Second Reading Speech</a></p>"
        );
        bill
    }

    /// Passed by the Assembly and now before the Council, where a minister
    /// of that house has introduced it in turn.
    fn in_the_council() -> OnSite {
        on_site(
            22911,
            "Associations Reform Amendment Bill 2026",
            "Council - second reading (passed Assembly)",
            "2026-08-28T14:16:02+10:00",
            vec![
                house(
                    "First House: Legislative Assembly",
                    "Assembly",
                    vec![
                        reading(
                            "First reading",
                            &[
                                ("Introduced by <strong>Hon Priya Raman</strong>", ""),
                                ("First reading passed", "2026-08-11T10:00:00+10:00"),
                            ],
                        ),
                        reading(
                            "Third reading",
                            &[
                                ("Third reading moved ", "2026-08-27T10:00:00+10:00"),
                                ("Third reading passed", "2026-08-27T10:00:00+10:00"),
                            ],
                        ),
                    ],
                ),
                house(
                    "Second House: Legislative Council",
                    "Council",
                    vec![
                        reading(
                            "First reading",
                            &[
                                ("Introduced by <strong>Hon Mei Tanaka</strong>", ""),
                                ("First reading passed", "2026-08-27T10:00:00+10:00"),
                            ],
                        ),
                        reading(
                            "Second reading",
                            &[("Second reading moved", "2026-08-27T10:00:00+10:00")],
                        ),
                    ],
                ),
            ],
            vec![print(
                "Introduction print \u{2013} Bill",
                &["601340bi1.pdf", "601340bi1.DOCX"],
            )],
        )
    }

    /// A private member's bill begun in the Council, with no prints yet.
    fn from_the_council() -> OnSite {
        on_site(
            22892,
            "Plebiscite Bill 2026",
            "Council - second reading",
            "2026-07-30T16:30:39+10:00",
            vec![house(
                "First House: Legislative Council",
                "Council",
                vec![
                    reading(
                        "First reading",
                        &[
                            ("Introduced by <strong>Mr Erin Blackwood</strong>", ""),
                            ("First reading passed", "2026-07-21T10:00:00+10:00"),
                        ],
                    ),
                    reading(
                        "Second reading",
                        &[("Second reading moved", "2026-07-30T10:00:00+10:00")],
                    ),
                ],
            )],
            vec![],
        )
    }

    /// Withdrawn after its first reading, in the term's first sitting week.
    fn withdrawn() -> OnSite {
        on_site(
            20100,
            "Sample Inquiry Bill 2022",
            "Withdrawn",
            "2023-02-07T09:00:00+11:00",
            vec![house(
                "First House: Legislative Assembly",
                "Assembly",
                vec![reading(
                    "First reading",
                    &[
                        ("Introduced by <strong>Hon Riley Okafor</strong>", ""),
                        ("First reading passed", "2022-12-20T10:00:00+11:00"),
                    ],
                )],
            )],
            vec![],
        )
    }

    /// The last parliament's: first read before the election, in its year.
    fn before_the_term() -> OnSite {
        on_site(
            19800,
            "Example Amendment Bill 2022",
            "Assembly - second reading",
            "2022-10-04T12:00:00+11:00",
            vec![house(
                "First House: Legislative Assembly",
                "Assembly",
                vec![reading(
                    "First reading",
                    &[("First reading passed", "2022-08-02T10:00:00+10:00")],
                )],
            )],
            vec![],
        )
    }

    /// The site. Its search answers two bills at a time, so a reader asking
    /// for forty has to page by what comes back; each bill's page answers at
    /// its path.
    fn site(bills: Arc<Mutex<Vec<OnSite>>>) -> TestServer {
        TestServer::start(move |req| {
            let bills = bills.lock().unwrap();
            if req.method == "POST" && req.path == SEARCH {
                let asked: Value = serde_json::from_str(&req.body).unwrap_or_default();
                let from = asked["from"].as_u64().unwrap_or(0) as usize;
                let hits: Vec<&Value> = bills.iter().skip(from).take(2).map(|b| &b.hit).collect();
                return Response::json(
                    json!({
                        "took": 3,
                        "timed_out": false,
                        "hits": {
                            "total": { "value": bills.len(), "relation": "eq" },
                            "max_score": null,
                            "hits": hits
                        }
                    })
                    .to_string(),
                );
            }
            if req.path.starts_with("/vic-legislation/api/tide/page?")
                && req.query("site").as_deref() == Some("6")
            {
                let path = req.query("path");
                if let Some(bill) = bills
                    .iter()
                    .find(|b| b.page["meta"]["url"].as_str() == path.as_deref())
                {
                    return Response::json(bill.page.to_string());
                }
            }
            Response::status(404, "unexpected request")
        })
    }

    fn pages_read(server: &TestServer, path: &str) -> usize {
        server
            .requests()
            .iter()
            .filter(|r| r.path.contains("/api/tide/page?") && r.path.contains(path))
            .count()
    }

    #[tokio::test]
    async fn the_term_s_bills_are_paged_through_and_read_from_their_pages() {
        let bills = vec![
            assented(),
            in_the_council(),
            before_the_term(),
            from_the_council(),
            withdrawn(),
        ];
        let server = site(Arc::new(Mutex::new(bills)));
        let store = new_store("vic-bills");

        let roll = read(&store, &Endpoints::at(&server.base), "2022-11-26")
            .await
            .expect("read");
        assert_eq!(roll.state, StateCode::VIC);
        assert_eq!(roll.publisher, "Victorian Legislation");
        assert_eq!(
            roll.source,
            format!("{}/vic-legislation/bills/all-bills", server.base)
        );

        let searches: Vec<Value> = server
            .requests()
            .iter()
            .filter(|r| r.method == "POST")
            .map(|r| serde_json::from_str(&r.body).expect("a JSON search"))
            .collect();
        let offsets: Vec<u64> = searches
            .iter()
            .map(|s| s["from"].as_u64().unwrap())
            .collect();
        assert_eq!(offsets, [0, 2, 4], "paged by what came back, to the total");
        assert_eq!(searches[0]["size"], 40);
        assert_eq!(
            searches[0]["_source"]["excludes"],
            json!(["es_attachment", "rendered_item"]),
            "never the documents' text"
        );
        let filters = searches[0]["query"]["bool"]["filter"]
            .as_array()
            .expect("filters");
        let years: Vec<&str> = filters
            .iter()
            .find_map(|f| f["terms"]["legislation_year"].as_array())
            .expect("a year filter")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let this_year = melbourne_year().to_string();
        assert_eq!(years.first(), Some(&"2022"), "from the election's year");
        assert_eq!(years.last(), Some(&this_year.as_str()), "to this year");
        assert!(
            !filters
                .iter()
                .any(|f| f.to_string().contains("parliament_current")),
            "bills no longer before Parliament are the term's too"
        );

        let titles: Vec<&str> = roll.bills.iter().map(|b| b.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "Statute Law Revision Bill 2024",
                "Associations Reform Amendment Bill 2026",
                "Plebiscite Bill 2026",
                "Sample Inquiry Bill 2022"
            ],
            "the last parliament's bill is left out, this one's from December 2022 kept"
        );

        assert_eq!(
            roll.bills[0],
            RollBill {
                title: "Statute Law Revision Bill 2024".to_string(),
                number: None,
                origin: Some("Legislative Assembly".to_string()),
                sponsor: Some("Hon Riley Okafor".to_string()),
                kind: None,
                status: "Passed and Assented to".to_string(),
                stage: 4,
                stopped: false,
                timeline: steps(&[
                    ("2024-02-20", "First reading passed (Legislative Assembly)"),
                    ("2024-02-21", "Second reading moved (Legislative Assembly)"),
                    (
                        "2024-03-05",
                        "Government business program (Legislative Assembly)"
                    ),
                    ("2024-03-06", "Debate resumed (Legislative Assembly)"),
                    ("2024-03-07", "Second reading passed (Legislative Assembly)"),
                    ("2024-03-07", "Third reading passed (Legislative Assembly)"),
                    (
                        "2024-03-07",
                        "Message received from Assembly (Legislative Council)"
                    ),
                    ("2024-03-07", "First reading passed (Legislative Council)"),
                    ("2024-04-18", "Third reading passed (Legislative Council)"),
                    ("2024-04-23", "Royal Assent given"),
                ]),
                source: format!(
                    "{}/vic-legislation/bills/statute-law-revision-bill-2024",
                    server.base
                ),
                text: pdf("601129bi1.pdf"),
                explanatory: pdf("601129exi1.pdf"),
                act: Some("Act No. 13/2024".to_string()),
            }
        );

        let in_council = &roll.bills[1];
        assert_eq!(in_council.origin.as_deref(), Some("Legislative Assembly"));
        assert_eq!(
            in_council.sponsor.as_deref(),
            Some("Hon Priya Raman"),
            "the first house's member, not the second's"
        );
        assert_eq!(
            in_council.status,
            "Council - second reading (passed Assembly)"
        );
        assert_eq!(in_council.stage, 2, "passed the Assembly only");
        assert!(!in_council.stopped);
        assert_eq!(
            in_council.timeline.last().map(|s| s.event.as_str()),
            Some("Second reading moved (Legislative Council)")
        );
        assert!(in_council
            .timeline
            .iter()
            .any(|s| s.event == "Third reading moved (Legislative Assembly)"));
        assert_eq!(in_council.text, pdf("601340bi1.pdf"));
        assert_eq!(in_council.explanatory, None);
        assert_eq!(in_council.act, None);

        let from_council = &roll.bills[2];
        assert_eq!(from_council.origin.as_deref(), Some("Legislative Council"));
        assert_eq!(from_council.sponsor.as_deref(), Some("Mr Erin Blackwood"));
        assert_eq!(from_council.stage, 1);
        assert_eq!(from_council.text, None, "no prints yet");

        let gone = &roll.bills[3];
        assert!(gone.stopped, "withdrawn");
        assert_eq!(gone.stage, 1);
        assert_eq!(gone.status, "Withdrawn");

        let kept: Value = serde_json::from_str(
            &store
                .get_raw("raw/states/vic/bills/21001.json")
                .await
                .unwrap()
                .expect("the page is kept"),
        )
        .unwrap();
        assert_eq!(kept["changed"], "2024-05-03T11:55:54+10:00");
        assert!(
            !kept.to_string().contains("Second Reading Speech"),
            "facts and links are kept, not the notes with them"
        );
    }

    #[tokio::test]
    async fn a_page_is_read_again_only_once_the_list_says_it_changed() {
        let bills = Arc::new(Mutex::new(vec![assented(), in_the_council()]));
        let server = site(Arc::clone(&bills));
        let endpoints = Endpoints::at(&server.base);
        let store = new_store("vic-bills-kept");

        read(&store, &endpoints, "2022-11-26").await.expect("first");
        assert_eq!(pages_read(&server, "/bills/"), 2);

        read(&store, &endpoints, "2022-11-26")
            .await
            .expect("second");
        assert_eq!(
            pages_read(&server, "/bills/"),
            2,
            "unchanged pages come from the store"
        );

        // The Council passes the bill, and its page and the list move on.
        {
            let mut bills = bills.lock().unwrap();
            let moved = "2026-09-10T15:00:00+10:00";
            bills[1].hit["_source"]["changed"] = json!([moved]);
            bills[1].page["changed"] = json!(moved);
            bills[1].page["sections"][1]["readings"]
                .as_array_mut()
                .unwrap()
                .push(reading(
                    "Third reading",
                    &[("Third reading passed", "2026-09-10T10:00:00+10:00")],
                ));
        }
        let roll = read(&store, &endpoints, "2022-11-26").await.expect("third");
        assert_eq!(
            pages_read(&server, "/bills/"),
            3,
            "only the page that changed is read again"
        );
        assert_eq!(
            pages_read(&server, "associations-reform-amendment-bill-2026"),
            2
        );
        assert_eq!(roll.bills[1].stage, 3, "passed both houses");
        assert_eq!(
            roll.bills[1].timeline.last().map(|s| s.event.as_str()),
            Some("Third reading passed (Legislative Council)")
        );
    }

    #[tokio::test]
    async fn a_page_that_cannot_be_read_falls_back_on_the_copy_kept() {
        let bills = Arc::new(Mutex::new(vec![in_the_council()]));
        let server = site(Arc::clone(&bills));
        let endpoints = Endpoints::at(&server.base);
        let store = new_store("vic-bills-fallback");
        read(&store, &endpoints, "2022-11-26").await.expect("first");

        // The list moves on but the page has gone missing; and a new bill's
        // page is missing from the start.
        {
            let mut bills = bills.lock().unwrap();
            bills[0].hit["_source"]["changed"] = json!(["2026-09-10T15:00:00+10:00"]);
            bills[0].page["meta"]["url"] = json!("/bills/somewhere-else");
            let mut unread = from_the_council();
            unread.page["meta"]["url"] = json!("/bills/nowhere");
            bills.push(unread);
        }
        let roll = read(&store, &endpoints, "2022-11-26")
            .await
            .expect("second");
        let titles: Vec<&str> = roll.bills.iter().map(|b| b.title.as_str()).collect();
        assert_eq!(
            titles,
            ["Associations Reform Amendment Bill 2026"],
            "the copy kept stands in; a page never read leaves its bill out"
        );
        assert_eq!(roll.bills[0].stage, 2);

        read(&store, &endpoints, "2022-11-26").await.expect("third");
        assert_eq!(
            pages_read(&server, "associations-reform-amendment-bill-2026"),
            3,
            "and it is asked for again each run until it reads"
        );
    }

    #[tokio::test]
    async fn a_list_that_stops_short_of_its_total_is_an_error() {
        let server = TestServer::start(|req| {
            let asked: Value = serde_json::from_str(&req.body).unwrap_or_default();
            let hits = match asked["from"].as_u64() {
                Some(0) => vec![assented().hit],
                _ => Vec::new(),
            };
            Response::json(json!({ "hits": { "total": { "value": 5 }, "hits": hits } }).to_string())
        });
        let err = read(
            &new_store("vic-bills-short"),
            &Endpoints::at(&server.base),
            "2022-11-26",
        )
        .await
        .expect_err("a short list is refused");
        assert_eq!(err.to_string(), "1 of 5 bills came back");
    }

    #[test]
    fn status_words_stop_a_bill_and_never_hold_its_stage_back() {
        for status in [
            "Lapsed",
            "Withdrawn",
            "Assembly - second reading negatived",
            "Council - order of the day discharged",
            "Defeated",
        ] {
            assert!(STOPPED.is_match(status), "{status}");
        }
        for status in [
            "Council - second reading (passed Assembly)",
            "Council - Committee of the whole",
            "Passed and Assented to",
        ] {
            assert!(!STOPPED.is_match(status), "{status}");
        }
        assert_eq!(stage_in("Passed and Assented to"), 4);
        assert_eq!(stage_in("Council - second reading (passed Assembly)"), 2);
        assert_eq!(stage_in("Assembly - second reading (passed Council)"), 2);
        assert_eq!(stage_in("Council - second reading"), 1);
    }
}
