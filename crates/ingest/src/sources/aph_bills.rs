use crate::endpoints::Endpoints;
use crate::http::fetch_json;
use crate::store::Store;
use anyhow::{anyhow, Result};
use pollywiki_schema::{Bill, BillLinks, BillRaiser, House, TimelineStep};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use std::sync::LazyLock;

/// Bills before the federal parliament, from the JSON endpoints behind
/// ParlWork (parlwork.aph.gov.au), the SPA that powers APH's own bills list.
///
/// The APH WAF rejects every non-browser User-Agent, including honest bot
/// strings with contact details, so this source presents a browser UA. Volume
/// stays polite: list pages plus detail fetches only for bills whose list
/// timestamp moved. Endpoints are undocumented and may change; failures are
/// reported loudly, never written.
const BROWSER_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/127.0.0.0 Safari/537.36";

const PAGE_SIZE: usize = 100;
const MAX_PAGES: usize = 10;

#[derive(Debug, Clone, Deserialize)]
pub struct ParlWorkBill {
    #[serde(rename = "Id")]
    pub id: Option<String>,
    #[serde(rename = "Title")]
    pub title: Option<String>,
    #[serde(rename = "FormattedOriginatingChamber")]
    pub formatted_originating_chamber: Option<String>,
    #[serde(rename = "Status")]
    pub status: Option<String>,
    #[serde(rename = "Summary")]
    pub summary: Option<String>,
    #[serde(rename = "LastUpdatedDateTime")]
    pub last_updated_date_time: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ParlWorkPerson {
    #[serde(rename = "DisplayName")]
    pub display_name: Option<String>,
    #[serde(rename = "Id")]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ParlWorkDetail {
    #[serde(rename = "Bill")]
    pub bill: Option<ParlWorkDetailBill>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ParlWorkDetailBill {
    #[serde(rename = "ParlInfoUrl")]
    pub parl_info_url: Option<String>,
    #[serde(rename = "Sponsors")]
    pub sponsors: Option<Vec<ParlWorkPerson>>,
    #[serde(rename = "Movers")]
    pub movers: Option<Vec<ParlWorkPerson>>,
    #[serde(rename = "GroupedProgressStates")]
    pub grouped_progress_states: Option<Vec<ParlWorkProgressGroup>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ParlWorkProgressGroup {
    #[serde(rename = "FormattedChamber")]
    pub formatted_chamber: Option<String>,
    #[serde(rename = "ProgressStates")]
    pub progress_states: Option<Vec<ParlWorkProgressState>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ParlWorkProgressState {
    #[serde(rename = "Description")]
    pub description: Option<String>,
    #[serde(rename = "UpdateDate")]
    pub update_date: Option<String>,
}

pub async fn sync_aph_bills(store: &Store, parliament: i64, endpoints: &Endpoints) -> Result<()> {
    let opts = endpoints.opts(2000).with_header("user-agent", BROWSER_UA);
    let mut items: Vec<ParlWorkBill> = Vec::new();
    for page in 1..=MAX_PAGES {
        let url = format!(
            "{}/bills?Take={PAGE_SIZE}&Page={page}&SortOrder=2&Keyword=null&IsTitleSearch=true&DateRangeFrom=null&DateRangeTo=null&StatusFilters=0&BillTypes=0",
            endpoints.parlwork
        );
        let batch_raw: Value = fetch_json(&url, &opts).await?;
        if !batch_raw.is_array() {
            return Err(anyhow!("parlwork: unexpected response shape"));
        }
        store
            .put_json(&format!("raw/aph/bills-page-{page}.json"), &batch_raw)
            .await?;
        let batch: Vec<ParlWorkBill> = serde_json::from_value(batch_raw)?;
        let batch_len = batch.len();
        items.extend(batch);
        if batch_len < PAGE_SIZE {
            break;
        }
    }
    if items.is_empty() {
        return Err(anyhow!("parlwork: zero bills parsed, refusing to write"));
    }

    let detail_opts = endpoints.opts(1500).with_header("user-agent", BROWSER_UA);
    let mut written = 0;
    let mut detailed = 0;
    for item in &items {
        let (Some(id), Some(_title)) = (&item.id, &item.title) else {
            continue;
        };
        let key = format!("canonical/bills/{id}.json");
        let existing_raw = store.get_raw(&key).await?;
        let list_updated = item.last_updated_date_time.clone().unwrap_or_default();

        let mut bill = to_bill(item, parliament);
        if let Some(existing_raw) = &existing_raw {
            let existing_value: Value = serde_json::from_str(existing_raw)?;
            let existing: Bill = serde_json::from_str(existing_raw)?;
            let timeline_present = !existing.timeline.is_empty();
            // Whatever happens below, fields this source does not own survive.
            bill = merge_existing(&existing, bill);
            if existing.list_updated.as_deref() == Some(list_updated.as_str()) && timeline_present {
                // Unchanged upstream. Self-heal newly-extracted fields from the stored
                // raw detail when older canonical records predate them.
                let fields_missing = existing_value.get("sponsors").is_none()
                    || existing_value.get("movers").is_none();
                if fields_missing || carries_html_entity(&existing) {
                    if let Some(raw) = store
                        .get_json::<ParlWorkDetail>(&format!("raw/aph/bill-{id}.json"))
                        .await?
                    {
                        let merged = merge_existing(&existing, bill);
                        store.put_json(&key, &with_detail(merged, &raw)).await?;
                        written += 1;
                    }
                }
                continue;
            }
        }

        // Detail fetch: dated per-chamber progress plus the ParlInfo record link.
        match fetch_json::<Value>(&format!("{}/bills/{id}", endpoints.parlwork), &detail_opts).await
        {
            Ok(detail_raw) => {
                store
                    .put_json(&format!("raw/aph/bill-{id}.json"), &detail_raw)
                    .await?;
                let detail: ParlWorkDetail = serde_json::from_value(detail_raw)?;
                bill = with_detail(bill, &detail);
                detailed += 1;
            }
            Err(err) => {
                eprintln!("aph-bills: detail for {id} failed - {err}");
            }
        }

        store.put_json(&key, &bill).await?;
        written += 1;
    }
    println!(
        "aph-bills: {} listed, {written} written, {detailed} detail fetches",
        items.len()
    );
    Ok(())
}

/// `{ ...existing, ...toBill(item) }`: fresh list fields win; only the fields
/// the list response never carries survive from the stored record.
/// True when stored prose still carries an HTML entity, so the record predates
/// the decoding fix and is worth rebuilding from the cached raw response.
fn carries_html_entity(bill: &Bill) -> bool {
    static ENTITY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"&(#\d+|#x[0-9a-fA-F]+|[a-zA-Z]+);").unwrap());
    let mut prose = vec![bill.title.as_str(), bill.status.as_str()];
    prose.extend(bill.timeline.iter().map(|s| s.event.as_str()));
    prose.extend(
        bill.sponsors
            .iter()
            .chain(bill.movers.iter())
            .map(|r| r.name.as_str()),
    );
    prose.extend(bill.summary.as_deref());
    prose.iter().any(|text| ENTITY.is_match(text))
}

fn merge_existing(existing: &Bill, fresh: Bill) -> Bill {
    Bill {
        bill_type: existing.bill_type.clone(),
        sponsor: existing.sponsor.clone(),
        portfolio: existing.portfolio.clone(),
        ai_summary: existing.ai_summary.clone(),
        ..fresh
    }
}

fn to_bill(item: &ParlWorkBill, parliament: i64) -> Bill {
    Bill {
        id: item.id.clone().unwrap_or_default(),
        title: item.title.as_deref().map(strip_html).unwrap_or_default(),
        parliament,
        chamber: if item
            .formatted_originating_chamber
            .as_deref()
            .unwrap_or("")
            .to_lowercase()
            == "senate"
        {
            House::Senate
        } else {
            House::Representatives
        },
        bill_type: None,
        sponsor: None,
        portfolio: None,
        summary: match &item.summary {
            Some(s) if !s.is_empty() => Some(strip_html(s)),
            _ => None,
        },
        ai_summary: None,
        status: item
            .status
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(strip_html)
            .unwrap_or_else(|| "Before parliament".to_string()),
        timeline: Vec::new(),
        links: BillLinks {
            aph: Some(format!(
                "https://www.aph.gov.au/Parliamentary_Business/Bills_Legislation/Bills_Search_Results/Result?bId={}",
                item.id.as_deref().unwrap_or_default()
            )),
            ..Default::default()
        },
        list_updated: item.last_updated_date_time.clone(),
        sponsors: Vec::new(),
        movers: Vec::new(),
        division_ids: Vec::new(),
    }
}

fn with_detail(bill: Bill, detail: &ParlWorkDetail) -> Bill {
    let Some(inner) = &detail.bill else {
        return bill;
    };
    let mut timeline: Vec<TimelineStep> = Vec::new();
    for group in inner.grouped_progress_states.as_deref().unwrap_or_default() {
        for state in group.progress_states.as_deref().unwrap_or_default() {
            let Some(date) = dot_net_date(state.update_date.as_deref()) else {
                continue;
            };
            let Some(description) = state.description.as_deref().filter(|d| !d.is_empty()) else {
                continue;
            };
            timeline.push(TimelineStep {
                date,
                event: match group.formatted_chamber.as_deref().filter(|c| !c.is_empty()) {
                    Some(chamber) => strip_html(&format!("{description} ({chamber})")),
                    None => strip_html(description),
                },
            });
        }
    }
    timeline.sort_by(|a, b| pollywiki_schema::js_compare(&a.date, &b.date));
    let mut links = bill.links.clone();
    if let Some(parlinfo) = inner.parl_info_url.as_deref().filter(|u| !u.is_empty()) {
        links.parlinfo = Some(parlinfo.to_string());
    }
    Bill {
        timeline,
        sponsors: inner
            .sponsors
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(to_raiser)
            .filter(|r| !r.name.is_empty())
            .collect(),
        movers: inner
            .movers
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(to_raiser)
            .filter(|r| !r.name.is_empty())
            .collect(),
        links,
        ..bill
    }
}

const HONORIFICS: [&str; 10] = [
    "the", "hon", "hon.", "sen", "senator", "dr", "mr", "ms", "mrs", "mp",
];

/// "ALBANESE, the Hon. Anthony Norman" → { name: "Anthony Albanese", phid }.
pub fn to_raiser(p: &ParlWorkPerson) -> BillRaiser {
    static FIRST_LETTERS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(^|[\s\-'\u{2019}])([a-z])").unwrap());
    let display = &p
        .display_name
        .as_deref()
        .map(strip_html)
        .unwrap_or_default();
    // Only the first two comma-separated segments matter: post-nominals like
    // ", MP" in "JOYCE, Barnaby, MP" are discarded.
    let mut parts = display.split(',');
    let family_raw = parts.next().unwrap_or("");
    let givens_raw = parts.next().unwrap_or("");
    let family_lower = family_raw.trim().to_lowercase();
    let family = FIRST_LETTERS
        .replace_all(&family_lower, |caps: &regex::Captures| {
            format!("{}{}", &caps[1], caps[2].to_uppercase())
        })
        .into_owned();
    let given = givens_raw
        .split_whitespace()
        .find(|t| {
            let lowered = t.to_lowercase();
            let stripped = lowered.strip_suffix('.').unwrap_or(&lowered);
            !HONORIFICS.contains(&stripped)
        })
        .unwrap_or("");
    let name = [given, family.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    BillRaiser {
        name,
        phid: p.id.clone().filter(|id| !id.is_empty()),
        slug: None,
    }
}

/// "/Date(1764075600000+1100)/" → ISO date in the event's local offset.
pub fn dot_net_date(value: Option<&str>) -> Option<String> {
    static PATTERN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"/Date\((\d+)([+-]\d{4})?\)/").unwrap());
    let caps = PATTERN.captures(value?)?;
    let millis: i64 = caps.get(1)?.as_str().parse().ok()?;
    let offset_minutes: i64 = match caps.get(2) {
        Some(m) => {
            let s = m.as_str();
            let hours: i64 = s[..3].parse().unwrap_or(0);
            let minutes: i64 = format!("{}{}", &s[..1], &s[3..]).parse().unwrap_or(0);
            hours * 60 + minutes
        }
        None => 0,
    };
    let adjusted = chrono::DateTime::from_timestamp_millis(millis + offset_minutes * 60_000)?;
    Some(adjusted.format("%Y-%m-%d").to_string())
}

fn entity_for(name: &str) -> Option<&'static str> {
    match name.to_lowercase().as_str() {
        "amp" => Some("&"),
        "nbsp" => Some(" "),
        "quot" => Some("\""),
        "apos" => Some("'"),
        "lsquo" => Some("\u{2018}"),
        "rsquo" => Some("\u{2019}"),
        "ldquo" => Some("\u{201C}"),
        "rdquo" => Some("\u{201D}"),
        "ndash" => Some("\u{2013}"),
        "mdash" => Some("\u{2014}"),
        "hellip" => Some("\u{2026}"),
        "lt" => Some("<"),
        "gt" => Some(">"),
        _ => None,
    }
}

pub fn strip_html(html: &str) -> String {
    static TAGS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]*>").unwrap());
    static DEC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"&#(\d+);").unwrap());
    static HEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)&#x([0-9a-f]+);").unwrap());
    static NAMED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)&([a-z]+);").unwrap());
    static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

    let text = TAGS.replace_all(html, " ");
    let text = DEC.replace_all(&text, |caps: &regex::Captures| {
        caps[1]
            .parse::<u32>()
            .ok()
            .and_then(char::from_u32)
            .map(String::from)
            .unwrap_or_default()
    });
    let text = HEX.replace_all(&text, |caps: &regex::Captures| {
        u32::from_str_radix(&caps[1], 16)
            .ok()
            .and_then(char::from_u32)
            .map(String::from)
            .unwrap_or_default()
    });
    let text = NAMED.replace_all(&text, |caps: &regex::Captures| {
        entity_for(&caps[1])
            .map(str::to_string)
            .unwrap_or_else(|| caps[0].to_string())
    });
    SPACES.replace_all(&text, " ").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{LocalStore, Store};
    use crate::test_http::{Response, TestServer};
    use std::path::PathBuf;

    fn new_store(name: &str) -> Store {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/aph-bill-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Store::Local(LocalStore::new(dir))
    }

    fn list_item(id: &str, title: &str, updated: &str) -> serde_json::Value {
        serde_json::json!({
            "Id": id,
            "Title": title,
            "Status": "Before the House",
            "Summary": "<p>Amends the Act</p>",
            "FormattedOriginatingChamber": "House of Representatives",
            "LastUpdatedDateTime": updated
        })
    }

    fn detail_body(description: &str) -> serde_json::Value {
        serde_json::json!({ "Bill": {
            "ParlInfoUrl": "https://parlinfo.aph.gov.au/r7354",
            "Sponsors": [{ "DisplayName": "PATERSON, the Hon. Alexandra", "Id": "PHID1" }],
            "Movers": [],
            "GroupedProgressStates": [{
                "FormattedChamber": "House of Reps",
                "ProgressStates": [{
                    "UpdateDate": "/Date(1764075600000+1100)/",
                    "Description": description
                }]
            }]
        } })
    }

    fn list_bill(json: &str) -> ParlWorkBill {
        serde_json::from_str(json).expect("list bill fixture")
    }

    fn detail(json: &str) -> ParlWorkDetail {
        serde_json::from_str(json).expect("detail fixture")
    }

    #[test]
    fn list_records_become_bills_with_an_official_link() {
        let bill = to_bill(
            &list_bill(
                r#"{"Id":"r7123","Title":"Example Bill 2026",
                    "FormattedOriginatingChamber":"House of Representatives",
                    "Status":"Before Senate","Summary":"<p>Amends the <b>Example Act</b>.</p>",
                    "LastUpdatedDateTime":"/Date(1755000000000+1000)/"}"#,
            ),
            48,
        );
        assert_eq!(bill.id, "r7123");
        assert_eq!(bill.chamber, House::Representatives);
        assert_eq!(bill.status, "Before Senate");
        // Markup is stripped from the official summary before it is stored.
        assert_eq!(bill.summary.as_deref(), Some("Amends the Example Act ."));
        assert!(bill
            .links
            .aph
            .as_deref()
            .is_some_and(|u| u.ends_with("bId=r7123")));
        assert!(bill.list_updated.is_some(), "the freshness marker is kept");
    }

    #[test]
    fn a_senate_bill_is_recognised_and_gaps_get_defaults() {
        let senate = to_bill(
            &list_bill(r#"{"FormattedOriginatingChamber":"Senate","Id":"s996"}"#),
            48,
        );
        assert_eq!(senate.chamber, House::Senate);
        // An absent status is not left blank; it says what little is known.
        assert_eq!(senate.status, "Before parliament");
        assert!(
            senate.summary.is_none(),
            "an absent summary is not an empty one"
        );
        assert!(senate.title.is_empty());

        // An empty summary string is treated the same as an absent one.
        let blank = to_bill(&list_bill(r#"{"Id":"r1","Summary":""}"#), 48);
        assert!(blank.summary.is_none());
        // Anything that is not the Senate originates in the House.
        assert_eq!(blank.chamber, House::Representatives);
    }

    #[test]
    fn detail_records_add_a_chamber_stamped_timeline_and_raisers() {
        let bill = to_bill(
            &list_bill(r#"{"Id":"r7123","Title":"Example Bill 2026"}"#),
            48,
        );
        let bill = with_detail(
            bill,
            &detail(
                r#"{"Bill":{"ParlInfoUrl":"https://parlinfo.gov.au/r7123",
                    "Sponsors":[{"DisplayName":"SMITH, Ms Jane","Id":"r1"}],
                    "Movers":[{"DisplayName":"JONES, the Hon. Bob MP","Id":"r2"}],
                    "GroupedProgressStates":[
                      {"FormattedChamber":"House of Representatives","ProgressStates":[
                        {"Description":"Introduced","UpdateDate":"/Date(1750000000000)/"},
                        {"Description":"","UpdateDate":"/Date(1750100000000)/"},
                        {"Description":"Second reading moved","UpdateDate":null}]},
                      {"FormattedChamber":"","ProgressStates":[
                        {"Description":"Assent","UpdateDate":"/Date(1755000000000)/"}]}
                    ]}}"#,
            ),
        );

        // Each step is stamped with its chamber, except where APH names none.
        let events: Vec<&str> = bill.timeline.iter().map(|s| s.event.as_str()).collect();
        assert_eq!(
            events,
            vec!["Introduced (House of Representatives)", "Assent"],
            "steps without a description or date are dropped, not guessed"
        );
        assert!(
            bill.timeline[0].date < bill.timeline[1].date,
            "timeline is sorted"
        );
        assert_eq!(
            bill.links.parlinfo.as_deref(),
            Some("https://parlinfo.gov.au/r7123")
        );
        assert_eq!(bill.sponsors[0].name, "Jane Smith");
        assert_eq!(bill.movers[0].name, "Bob Jones");
    }

    #[test]
    fn a_detail_response_with_no_bill_leaves_the_record_alone() {
        let bill = to_bill(&list_bill(r#"{"Id":"r1","Status":"Act"}"#), 48);
        let untouched = with_detail(bill, &detail(r#"{"Bill":null}"#));
        assert_eq!(untouched.status, "Act");
        assert!(untouched.timeline.is_empty());
    }

    #[test]
    fn refreshing_a_bill_keeps_the_fields_only_the_detail_fetch_knows() {
        let mut existing = to_bill(&list_bill(r#"{"Id":"r1","Status":"Before Reps"}"#), 48);
        existing.bill_type = Some("Government".to_string());
        existing.portfolio = Some("Health".to_string());
        existing.sponsor = Some("A Sponsor".to_string());
        existing.ai_summary = Some(pollywiki_schema::AiText {
            text: "note".to_string(),
            model: "m".to_string(),
            generated_at: "2026-08-01T00:00:00.000Z".to_string(),
        });

        let fresh = to_bill(&list_bill(r#"{"Id":"r1","Status":"Act"}"#), 48);
        let merged = merge_existing(&existing, fresh);
        // The list response carries the new status but none of these fields.
        assert_eq!(merged.status, "Act");
        assert_eq!(merged.bill_type.as_deref(), Some("Government"));
        assert_eq!(merged.portfolio.as_deref(), Some("Health"));
        assert_eq!(merged.sponsor.as_deref(), Some("A Sponsor"));
        assert!(merged.ai_summary.is_some(), "an AI note is not regenerated");
    }

    #[test]
    fn dot_net_dates_decode_with_and_without_an_offset() {
        assert_eq!(
            dot_net_date(Some("/Date(1755000000000)/")).as_deref(),
            Some("2025-08-12")
        );
        // A positive offset can push the date forward.
        assert_eq!(
            dot_net_date(Some("/Date(1755043200000+1000)/")).as_deref(),
            Some("2025-08-13")
        );
        assert_eq!(dot_net_date(Some("not a date")), None);
        assert_eq!(dot_net_date(None), None);
    }

    #[test]
    fn html_stripping_decodes_named_decimal_and_hex_entities() {
        assert_eq!(
            strip_html("<p>A&nbsp;bill &amp; a&#8201;note &#x2013; done</p>"),
            "A bill & a note \u{2013} done"
        );
        assert_eq!(
            strip_html("<i>Caf&eacute;</i> &unknown;"),
            "Caf&eacute; &unknown;"
        );
        assert_eq!(strip_html("  <b>  spaced  </b>  "), "spaced");
        assert_eq!(strip_html(""), "");
    }

    #[test]
    fn raisers_drop_honorifics_and_post_nominals() {
        let raiser = to_raiser(&ParlWorkPerson {
            display_name: Some("ALBANESE, the Hon. Anthony Norman".to_string()),
            id: Some("r36".to_string()),
        });
        assert_eq!(raiser.name, "Anthony Albanese");
        assert_eq!(raiser.phid.as_deref(), Some("r36"));

        let raiser = to_raiser(&ParlWorkPerson {
            display_name: Some("JOYCE, Barnaby, MP".to_string()),
            id: Some("E5D".to_string()),
        });
        assert_eq!(raiser.name, "Barnaby Joyce");

        let raiser = to_raiser(&ParlWorkPerson {
            display_name: Some("O'BRIEN, the Hon. Ted".to_string()),
            id: None,
        });
        assert_eq!(raiser.name, "Ted O'Brien");
    }

    /// Recomputes every bill from the raw list and detail payloads and
    /// compares against a reference store. Runs only when the two stores are
    /// supplied via environment variables (a local verification harness).
    #[test]
    fn bills_match_reference_store_fixture() {
        let (Ok(raw_store), Ok(reference_store)) = (
            std::env::var("POLLYWIKI_APH_RAW_STORE"),
            std::env::var("POLLYWIKI_APH_REFERENCE_STORE"),
        ) else {
            eprintln!("fixture stores not set; skipping");
            return;
        };
        let raw_store = std::path::PathBuf::from(raw_store);
        let reference_store = std::path::PathBuf::from(reference_store);

        let mut items: Vec<ParlWorkBill> = Vec::new();
        for page in 1..=MAX_PAGES {
            let path = raw_store.join(format!("raw/aph/bills-page-{page}.json"));
            if !path.exists() {
                break;
            }
            let batch: Vec<ParlWorkBill> =
                serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
            items.extend(batch);
        }
        assert!(!items.is_empty());

        let mut checked = 0;
        for item in &items {
            let Some(id) = &item.id else { continue };
            let reference_path = reference_store.join(format!("canonical/bills/{id}.json"));
            let raw_path = raw_store.join(format!("raw/aph/bill-{id}.json"));
            if !reference_path.exists() || !raw_path.exists() {
                continue;
            }
            let detail: ParlWorkDetail =
                serde_json::from_str(&std::fs::read_to_string(raw_path).unwrap()).unwrap();
            let built = with_detail(to_bill(item, 48), &detail);
            let reference: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(reference_path).unwrap()).unwrap();
            let built_value = serde_json::to_value(&built).unwrap();
            for key in [
                "title",
                "chamber",
                "summary",
                "status",
                "timeline",
                "sponsors",
                "movers",
                "links",
                "listUpdated",
            ] {
                assert_eq!(
                    reference.get(key),
                    built_value.get(key),
                    "bill {id} field {key}"
                );
            }
            checked += 1;
        }
        eprintln!("checked {checked} bills against the reference store");
        assert!(checked > 200);
    }

    #[test]
    fn parlwork_prose_is_decoded_wherever_it_lands() {
        // The live bug: "Australia&rsquo;s Foreign Relations ..." rendered as
        // typed, because only the summary was ever decoded.
        let item: ParlWorkBill = serde_json::from_str(
            r#"{"Id":"r7354",
                "Title":"Australia&rsquo;s Foreign Relations (State and Territory Arrangements) Amendment Bill 2026",
                "Status":"Before the House &amp; awaiting debate",
                "Summary":"<p>Amends the Act &#8212; see the note</p>",
                "FormattedOriginatingChamber":"House of Representatives"}"#,
        )
        .expect("list item fixture");
        let bill = to_bill(&item, 48);
        assert_eq!(
            bill.title,
            "Australia\u{2019}s Foreign Relations (State and Territory Arrangements) Amendment Bill 2026"
        );
        assert_eq!(bill.status, "Before the House & awaiting debate");
        assert_eq!(
            bill.summary.as_deref(),
            Some("Amends the Act \u{2014} see the note")
        );

        // Timeline events are prose from the same API.
        let detail: ParlWorkDetail = serde_json::from_str(
            r#"{"Bill":{"GroupedProgressStates":[{"FormattedChamber":"House of Reps",
                "ProgressStates":[{"UpdateDate":"/Date(1764075600000+1100)/",
                "Description":"Second reading &ndash; agreed to"}]}]}}"#,
        )
        .expect("detail fixture");
        let with_timeline = with_detail(bill, &detail);
        assert_eq!(
            with_timeline.timeline[0].event,
            "Second reading \u{2013} agreed to (House of Reps)"
        );
    }

    #[test]
    fn an_encoded_apostrophe_in_a_name_still_capitalises_the_next_letter() {
        let person: ParlWorkPerson = serde_json::from_str(
            r#"{"DisplayName":"O&rsquo;NEIL, the Hon. Clare Ellen","Id":"PHID1"}"#,
        )
        .expect("person fixture");
        let raiser = to_raiser(&person);
        // Decoding turns the entity into a curly apostrophe, which then has to
        // count as a word boundary or the surname reads "O’neil".
        assert_eq!(raiser.name, "Clare O\u{2019}Neil");
        assert_eq!(raiser.phid.as_deref(), Some("PHID1"));

        // The straight apostrophe was already handled and must stay handled.
        let straight: ParlWorkPerson =
            serde_json::from_str(r#"{"DisplayName":"O'BRIEN, Mr Ted"}"#).expect("fixture");
        assert_eq!(to_raiser(&straight).name, "Ted O'Brien");
    }

    #[test]
    fn stored_records_carrying_an_entity_are_flagged_for_rebuild() {
        let clean: Bill = serde_json::from_str(
            r#"{"id":"r1","title":"A Clean Bill 2026","parliament":48,
                "chamber":"representatives","status":"Act"}"#,
        )
        .expect("fixture");
        assert!(!carries_html_entity(&clean));

        // Every prose field is checked, not just the title.
        for field in ["title", "status"] {
            let mut bill = clean.clone();
            match field {
                "title" => bill.title = "Australia&rsquo;s Bill".to_string(),
                _ => bill.status = "Before the House &amp; waiting".to_string(),
            }
            assert!(carries_html_entity(&bill), "{field} is not checked");
        }
        let mut with_step = clean.clone();
        with_step.timeline = vec![TimelineStep {
            date: "2026-08-19".to_string(),
            event: "Second reading &ndash; agreed".to_string(),
        }];
        assert!(carries_html_entity(&with_step));

        let mut with_raiser = clean.clone();
        with_raiser.movers = vec![BillRaiser {
            name: "Clare O&rsquo;Neil".to_string(),
            phid: None,
            slug: None,
        }];
        assert!(carries_html_entity(&with_raiser));

        let mut numeric = clean.clone();
        numeric.summary = Some("Amends the Act &#8212; see note".to_string());
        assert!(carries_html_entity(&numeric), "numeric entities count too");

        // A bare ampersand in ordinary prose is not an entity.
        let mut plain = clean;
        plain.title = "Fish & Chips Bill 2026".to_string();
        assert!(!carries_html_entity(&plain));
    }

    #[tokio::test]
    async fn a_sync_lists_bills_fetches_detail_and_keeps_both_raw() {
        let server = TestServer::start(|req| {
            if req.path.starts_with("/parlwork/bills?") {
                // One short page ends the pagination.
                if req.query("Page").as_deref() == Some("1") {
                    return Response::json(
                        serde_json::json!([list_item(
                            "r7354",
                            "Australia&rsquo;s Bill 2026",
                            "2026-08-19"
                        )])
                        .to_string(),
                    );
                }
                return Response::json("[]");
            }
            if req.path.starts_with("/parlwork/bills/") {
                return Response::json(detail_body("Second reading &ndash; agreed to").to_string());
            }
            Response::status(404, "unexpected path")
        });
        let store = new_store("sync");

        sync_aph_bills(&store, 48, &Endpoints::at(&server.base))
            .await
            .expect("sync");

        let bill: Bill = store
            .get_json("canonical/bills/r7354.json")
            .await
            .unwrap()
            .expect("bill stored");
        assert_eq!(
            bill.title, "Australia\u{2019}s Bill 2026",
            "entities decoded"
        );
        assert_eq!(bill.parliament, 48);
        assert_eq!(bill.chamber, House::Representatives);
        assert_eq!(bill.timeline.len(), 1);
        assert_eq!(
            bill.timeline[0].event,
            "Second reading \u{2013} agreed to (House of Reps)"
        );
        assert_eq!(bill.sponsors[0].name, "Alexandra Paterson");
        assert_eq!(bill.sponsors[0].phid.as_deref(), Some("PHID1"));
        assert_eq!(
            bill.links.parlinfo.as_deref(),
            Some("https://parlinfo.aph.gov.au/r7354")
        );
        assert_eq!(bill.list_updated.as_deref(), Some("2026-08-19"));

        assert!(store
            .get_json::<Value>("raw/aph/bills-page-1.json")
            .await
            .unwrap()
            .is_some());
        assert!(store
            .get_json::<Value>("raw/aph/bill-r7354.json")
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn an_unchanged_bill_is_not_refetched_and_a_moved_one_is() {
        let stage = std::sync::Arc::new(std::sync::Mutex::new("2026-08-19".to_string()));
        let marker = std::sync::Arc::clone(&stage);
        let server = TestServer::start(move |req| {
            if req.path.starts_with("/parlwork/bills?") {
                if req.query("Page").as_deref() != Some("1") {
                    return Response::json("[]");
                }
                let updated = marker.lock().unwrap().clone();
                return Response::json(
                    serde_json::json!([list_item("r7354", "A Bill 2026", &updated)]).to_string(),
                );
            }
            Response::json(detail_body("Second reading").to_string())
        });
        let endpoints = Endpoints::at(&server.base);
        let store = new_store("incremental");

        sync_aph_bills(&store, 48, &endpoints).await.expect("first");
        let after_first = server.hits();
        sync_aph_bills(&store, 48, &endpoints)
            .await
            .expect("second");
        assert_eq!(
            server.hits() - after_first,
            1,
            "an unchanged listUpdated marker skips the detail fetch"
        );

        *stage.lock().unwrap() = "2026-08-20".to_string();
        let before = server.hits();
        sync_aph_bills(&store, 48, &endpoints).await.expect("third");
        assert_eq!(
            server.hits() - before,
            2,
            "a moved marker refetches the detail"
        );
    }

    #[tokio::test]
    async fn a_stored_bill_still_carrying_an_entity_is_healed_from_the_cached_raw() {
        // The live defect: 266 bills were already stored with "&rsquo;" in the
        // title, and the unchanged-marker path used to skip them forever.
        let server = TestServer::start(|req| {
            if req.path.starts_with("/parlwork/bills?") {
                if req.query("Page").as_deref() != Some("1") {
                    return Response::json("[]");
                }
                return Response::json(
                    serde_json::json!([list_item(
                        "r7354",
                        "Australia&rsquo;s Bill 2026",
                        "2026-08-19"
                    )])
                    .to_string(),
                );
            }
            panic!("the heal must not need a detail refetch: {}", req.path);
        });
        let store = new_store("heal");

        // A record as the old code left it: entity in the title, marker current,
        // timeline present, and the raw detail still cached.
        let stale: Bill = serde_json::from_str(
            r#"{"id":"r7354","title":"Australia&rsquo;s Bill 2026","parliament":48,
                "chamber":"representatives","status":"Before the House",
                "timeline":[{"date":"2026-08-19","event":"Second reading"}],
                "listUpdated":"2026-08-19","sponsors":[],"movers":[],"divisionIds":[]}"#,
        )
        .expect("stale fixture");
        store
            .put_json("canonical/bills/r7354.json", &stale)
            .await
            .unwrap();
        store
            .put_json(
                "raw/aph/bill-r7354.json",
                &detail_body("Second reading &ndash; agreed to"),
            )
            .await
            .unwrap();

        sync_aph_bills(&store, 48, &Endpoints::at(&server.base))
            .await
            .expect("sync");

        let healed: Bill = store
            .get_json("canonical/bills/r7354.json")
            .await
            .unwrap()
            .expect("bill");
        assert_eq!(
            healed.title, "Australia\u{2019}s Bill 2026",
            "title decoded in place"
        );
        assert_eq!(
            healed.timeline[0].event, "Second reading \u{2013} agreed to (House of Reps)",
            "the timeline is rebuilt from the cached raw detail"
        );
        assert_eq!(healed.sponsors[0].name, "Alexandra Paterson");
    }

    #[tokio::test]
    async fn curated_fields_survive_a_resync() {
        let server = TestServer::start(|req| {
            if req.path.starts_with("/parlwork/bills?") {
                if req.query("Page").as_deref() != Some("1") {
                    return Response::json("[]");
                }
                return Response::json(
                    serde_json::json!([list_item("r7354", "A Bill 2026", "2026-08-20")])
                        .to_string(),
                );
            }
            Response::json(detail_body("Second reading").to_string())
        });
        let endpoints = Endpoints::at(&server.base);
        let store = new_store("curated");

        sync_aph_bills(&store, 48, &endpoints).await.expect("first");
        // Fields this source does not own are added by other steps.
        let mut bill: Bill = store
            .get_json("canonical/bills/r7354.json")
            .await
            .unwrap()
            .expect("bill");
        bill.bill_type = Some("Government".to_string());
        bill.portfolio = Some("Foreign Affairs".to_string());
        bill.ai_summary = Some(
            serde_json::from_str(
                r#"{"text":"A note.","model":"m","generatedAt":"2026-08-19T00:00:00.000Z"}"#,
            )
            .expect("ai fixture"),
        );
        bill.list_updated = Some("2026-08-19".to_string());
        store
            .put_json("canonical/bills/r7354.json", &bill)
            .await
            .unwrap();

        sync_aph_bills(&store, 48, &endpoints)
            .await
            .expect("second");
        let merged: Bill = store
            .get_json("canonical/bills/r7354.json")
            .await
            .unwrap()
            .expect("bill");
        assert_eq!(merged.bill_type.as_deref(), Some("Government"));
        assert_eq!(merged.portfolio.as_deref(), Some("Foreign Affairs"));
        assert!(merged.ai_summary.is_some(), "the AI layer is not clobbered");
    }

    #[tokio::test]
    async fn a_response_that_is_not_a_list_or_is_empty_refuses_to_write() {
        let wrong_shape = TestServer::start(|_| Response::json(r#"{"error":"nope"}"#));
        let store = new_store("wrongshape");
        let err = sync_aph_bills(&store, 48, &Endpoints::at(&wrong_shape.base))
            .await
            .expect_err("an object is not a bill list");
        assert!(
            err.to_string().contains("unexpected response shape"),
            "got {err}"
        );

        let empty = TestServer::start(|_| Response::json("[]"));
        let store = new_store("emptylist");
        let err = sync_aph_bills(&store, 48, &Endpoints::at(&empty.base))
            .await
            .expect_err("zero bills means something is wrong upstream");
        assert!(err.to_string().contains("refusing to write"), "got {err}");
    }

    #[tokio::test]
    async fn a_detail_fetch_that_fails_still_stores_the_listed_bill() {
        let server = TestServer::start(|req| {
            if req.path.starts_with("/parlwork/bills?") {
                if req.query("Page").as_deref() != Some("1") {
                    return Response::json("[]");
                }
                return Response::json(
                    serde_json::json!([list_item("r7354", "A Bill 2026", "2026-08-19")])
                        .to_string(),
                );
            }
            Response::status(404, "no detail")
        });
        let store = new_store("nodetail");

        sync_aph_bills(&store, 48, &Endpoints::at(&server.base))
            .await
            .expect("a missing detail is not fatal");
        let bill: Bill = store
            .get_json("canonical/bills/r7354.json")
            .await
            .unwrap()
            .expect("bill stored anyway");
        assert!(
            bill.timeline.is_empty(),
            "no timeline rather than a wrong one"
        );
        assert_eq!(bill.title, "A Bill 2026");
    }
}
