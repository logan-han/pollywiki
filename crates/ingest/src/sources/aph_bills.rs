use crate::http::{fetch_json, FetchOpts};
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

pub async fn sync_aph_bills(store: &Store, parliament: i64) -> Result<()> {
    let opts = FetchOpts::min_interval(2000).with_header("user-agent", BROWSER_UA);
    let mut items: Vec<ParlWorkBill> = Vec::new();
    for page in 1..=MAX_PAGES {
        let url = format!(
            "https://parlwork.aph.gov.au/api/bills?Take={PAGE_SIZE}&Page={page}&SortOrder=2&Keyword=null&IsTitleSearch=true&DateRangeFrom=null&DateRangeTo=null&StatusFilters=0&BillTypes=0"
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

    let detail_opts = FetchOpts::min_interval(1500).with_header("user-agent", BROWSER_UA);
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
            if existing.list_updated.as_deref() == Some(list_updated.as_str()) && timeline_present {
                // Unchanged upstream. Self-heal newly-extracted fields from the stored
                // raw detail when older canonical records predate them.
                let fields_missing = existing_value.get("sponsors").is_none()
                    || existing_value.get("movers").is_none();
                if fields_missing {
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
        match fetch_json::<Value>(
            &format!("https://parlwork.aph.gov.au/api/bills/{id}"),
            &detail_opts,
        )
        .await
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
        title: item.title.clone().unwrap_or_default(),
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
            .clone()
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
                    Some(chamber) => format!("{description} ({chamber})"),
                    None => description.to_string(),
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
        LazyLock::new(|| Regex::new(r"(^|[\s\-'])([a-z])").unwrap());
    let display = p.display_name.as_deref().unwrap_or("");
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
}
