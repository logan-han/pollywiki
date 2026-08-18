use crate::http::{fetch_text, FetchOpts};
use crate::sources::aec::parse_csv;
use crate::store::Store;
use anyhow::{anyhow, Result};
use indexmap::IndexMap;
use pollywiki_schema::{Electorate, ElectorateProfileFacts};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

/// Profile prose changes rarely; refresh weekly.
const REFRESH_DAYS: f64 = 7.0;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElectorateProfile {
    pub stored_at: String,
    pub profile: ElectorateProfileFacts,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enrolment: Option<i64>,
}

fn state_path(state: &str) -> &'static str {
    match state {
        "NSW" => "nsw",
        "VIC" => "vic",
        "QLD" => "qld",
        "WA" => "wa",
        "SA" => "sa",
        "TAS" => "tas",
        "ACT" => "act",
        "NT" => "nt",
        _ => "",
    }
}

/// The AEC's official electorate profiles: name derivation, location, area
/// and gazettal facts, scraped from the dt/dd pairs on aec.gov.au, plus
/// enrolment figures from the current election's enrolment download.
pub async fn sync_aec_profiles(store: &Store, current_event: &str) -> Result<()> {
    let mut electorates: Vec<Electorate> = Vec::new();
    for key in store.list("canonical/electorates/").await? {
        if let Some(e) = store.get_json::<Electorate>(&key).await? {
            electorates.push(e);
        }
    }
    if electorates.is_empty() {
        return Err(anyhow!("aec-profiles: no electorates in store yet"));
    }

    let enrolment = fetch_enrolment(store, current_event).await;

    let mut fetched = 0;
    let mut skipped = 0;
    let mut failed = 0;
    for electorate in &electorates {
        let key = format!("canonical/electorate-profiles/{}.json", electorate.slug);
        let existing: Option<ElectorateProfile> = store.get_json(&key).await?;
        if let Some(existing) = &existing {
            if age_days(&existing.stored_at) < REFRESH_DAYS {
                skipped += 1;
                continue;
            }
        }
        let result: Result<()> = async {
            let url = format!(
                "https://www.aec.gov.au/profiles/{}/{}.htm",
                state_path(electorate.state.as_str()),
                electorate.slug
            );
            let html = fetch_text(&url, &FetchOpts::min_interval(700)).await?;
            let pairs = parse_dt_dd(&html);
            let profile = ElectorateProfile {
                stored_at: crate::now_iso(),
                profile: ElectorateProfileFacts {
                    name_derivation: pairs.get("name derivation").cloned(),
                    location: pairs.get("location description").cloned(),
                    area: pairs.get("area").cloned(),
                    gazetted: pairs
                        .get("date this name and boundary was gazetted")
                        .cloned(),
                    first_contested: pairs.get("first election this name was used at").cloned(),
                    demographic: pairs.get("demographic rating").cloned(),
                },
                enrolment: enrolment.get(&electorate.name.to_lowercase()).copied(),
            };
            store.put_json(&key, &profile).await
        }
        .await;
        match result {
            Ok(()) => fetched += 1,
            Err(err) => {
                failed += 1;
                eprintln!("aec-profiles: {} failed - {err}", electorate.slug);
                if failed >= 10 {
                    return Err(anyhow!("aec-profiles: too many failures, aborting"));
                }
            }
        }
    }
    println!("aec-profiles: {fetched} refreshed, {skipped} current, {failed} failed");
    Ok(())
}

async fn fetch_enrolment(store: &Store, event_id: &str) -> IndexMap<String, i64> {
    let mut out = IndexMap::new();
    let result: Result<()> = async {
        let csv = fetch_text(
            &format!(
                "https://results.aec.gov.au/{event_id}/Website/Downloads/GeneralEnrolmentByDivisionDownload-{event_id}.csv"
            ),
            &FetchOpts::min_interval(1500),
        )
        .await?;
        store
            .put_raw(
                &format!("raw/aec/{event_id}/GeneralEnrolmentByDivisionDownload.csv"),
                csv.as_bytes(),
            )
            .await?;
        let body = match csv.find('\n') {
            Some(at) => &csv[at + 1..],
            None => "",
        };
        for row in parse_csv(body)? {
            let name = row
                .get("DivisionNm")
                .map(|n| n.to_lowercase())
                .unwrap_or_default();
            let value = row
                .get("Enrolment")
                .or_else(|| row.get("CloseOfRollsEnrolment"))
                .and_then(|v| v.trim().parse::<i64>().ok())
                .unwrap_or(0);
            if !name.is_empty() && value > 0 {
                out.insert(name, value);
            }
        }
        Ok(())
    }
    .await;
    if let Err(err) = result {
        eprintln!("aec-profiles: enrolment fetch failed - {err}");
    }
    out
}

/// Extracts dt/dd label-value pairs, flattening markup inside values.
pub fn parse_dt_dd(html: &str) -> IndexMap<String, String> {
    static PAIRS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)<dt[^>]*>(.*?)</dt>\s*<dd[^>]*>(.*?)</dd>").unwrap());
    static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
    let mut out = IndexMap::new();
    for caps in PAIRS.captures_iter(html) {
        let label = clean(&caps[1]);
        let label = label.strip_suffix(':').unwrap_or(&label);
        let label = SPACES.replace_all(label, " ").to_lowercase();
        let value = clean(&caps[2]);
        if !label.is_empty() && !value.is_empty() {
            out.insert(label, value);
        }
    }
    out
}

/// The named entities AEC profile prose actually uses. &amp; is decoded last,
/// so an escaped entity such as "&amp;ndash;" survives as text instead of
/// being decoded twice.
const NAMED_ENTITIES: [(&str, &str); 12] = [
    ("&nbsp;", " "),
    ("&ndash;", "\u{2013}"),
    ("&mdash;", "\u{2014}"),
    ("&lsquo;", "\u{2018}"),
    ("&rsquo;", "\u{2019}"),
    ("&ldquo;", "\u{201c}"),
    ("&rdquo;", "\u{201d}"),
    ("&hellip;", "\u{2026}"),
    ("&quot;", "\""),
    ("&apos;", "'"),
    ("&lt;", "<"),
    ("&gt;", ">"),
];

fn clean(html: &str) -> String {
    static TAGS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]*>").unwrap());
    static DEC: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"&#(?:([0-9]+)|[xX]([0-9a-fA-F]+));").unwrap());
    static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
    let text = TAGS.replace_all(html, " ");
    let mut text = text.into_owned();
    for (entity, replacement) in NAMED_ENTITIES {
        if text.contains(entity) {
            text = text.replace(entity, replacement);
        }
    }
    let text = DEC.replace_all(&text, |caps: &regex::Captures| {
        let code = match (caps.get(1), caps.get(2)) {
            (Some(dec), _) => dec.as_str().parse::<u32>().ok(),
            (None, Some(hex)) => u32::from_str_radix(hex.as_str(), 16).ok(),
            _ => None,
        };
        code.and_then(char::from_u32)
            .map(String::from)
            .unwrap_or_default()
    });
    let text = text.replace("&amp;", "&");
    SPACES.replace_all(&text, " ").trim().to_string()
}

fn age_days(iso: &str) -> f64 {
    let Ok(then) = chrono::DateTime::parse_from_rfc3339(iso) else {
        return f64::INFINITY;
    };
    (chrono::Utc::now().timestamp_millis() - then.timestamp_millis()) as f64 / 86_400_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_text_decodes_the_entities_aec_pages_use() {
        let pairs = parse_dt_dd(
            "<dl><dt>Demographic Rating:</dt><dd>Inner Metropolitan &ndash; well-established \
             suburbs</dd><dt>Name derivation:</dt><dd>Named for the O&rsquo;Brien family \
             &amp; others &#8212; see the note &#x2013; 1949</dd></dl>",
        );
        assert_eq!(
            pairs.get("demographic rating").map(String::as_str),
            Some("Inner Metropolitan \u{2013} well-established suburbs")
        );
        assert_eq!(
            pairs.get("name derivation").map(String::as_str),
            Some(
                "Named for the O\u{2019}Brien family & others \u{2014} see the note \u{2013} 1949"
            )
        );
    }

    #[test]
    fn escaped_entities_are_not_decoded_twice() {
        // "&amp;ndash;" is the text "&ndash;", not an en dash.
        let pairs = parse_dt_dd("<dl><dt>Area:</dt><dd>writes &amp;ndash; verbatim</dd></dl>");
        assert_eq!(
            pairs.get("area").map(String::as_str),
            Some("writes &ndash; verbatim")
        );
    }
}
