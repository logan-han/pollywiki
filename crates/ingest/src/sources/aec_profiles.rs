use crate::endpoints::Endpoints;
use crate::http::fetch_text;
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
pub async fn sync_aec_profiles(
    store: &Store,
    current_event: &str,
    endpoints: &Endpoints,
) -> Result<()> {
    let mut electorates: Vec<Electorate> = Vec::new();
    for key in store.list("canonical/electorates/").await? {
        if let Some(e) = store.get_json::<Electorate>(&key).await? {
            electorates.push(e);
        }
    }
    if electorates.is_empty() {
        return Err(anyhow!("aec-profiles: no electorates in store yet"));
    }

    let enrolment = fetch_enrolment(store, current_event, endpoints).await;

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
                "{}/profiles/{}/{}.htm",
                endpoints.aec_profiles,
                state_path(electorate.state.as_str()),
                electorate.slug
            );
            let html = fetch_text(&url, &endpoints.opts(700)).await?;
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

async fn fetch_enrolment(
    store: &Store,
    event_id: &str,
    endpoints: &Endpoints,
) -> IndexMap<String, i64> {
    let mut out = IndexMap::new();
    let result: Result<()> = async {
        let csv = fetch_text(
            &format!(
                "{}/{event_id}/Website/Downloads/GeneralEnrolmentByDivisionDownload-{event_id}.csv",
                endpoints.aec_results
            ),
            &endpoints.opts(1500),
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
    use crate::store::LocalStore;
    use crate::test_http::{Response, TestServer};
    use std::path::PathBuf;

    fn new_store(name: &str) -> Store {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/aec-profile-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Store::Local(LocalStore::new(dir))
    }

    async fn seed_electorate(store: &Store, slug: &str, name: &str, state: &str) {
        let electorate: Electorate = serde_json::from_str(&format!(
            r#"{{"slug":"{slug}","name":"{name}","state":"{state}"}}"#
        ))
        .expect("electorate fixture");
        store
            .put_json(&format!("canonical/electorates/{slug}.json"), &electorate)
            .await
            .expect("seed");
    }

    const PROFILE_HTML: &str = "<dl><dt>Name derivation:</dt><dd>Named for a sample</dd>\
        <dt>Location description:</dt><dd>Inner suburbs</dd><dt>Area:</dt><dd>52 sq km</dd>\
        <dt>Date this name and boundary was gazetted:</dt><dd>31 July 2024</dd>\
        <dt>First election this name was used at:</dt><dd>1949</dd>\
        <dt>Demographic rating:</dt><dd>Inner Metropolitan</dd></dl>";

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

    #[tokio::test]
    async fn a_sync_scrapes_the_profile_facts_and_joins_the_enrolment_figure() {
        let server = TestServer::start(|req| {
            if req.path.contains("GeneralEnrolmentByDivisionDownload") {
                return Response::text(
                    "Enrolment as at some date\nStateAb,DivisionID,DivisionNm,Enrolment\nVIC,101,Sampleford,118432",
                );
            }
            if req.path.contains("/profiles/vic/sampleford.htm") {
                return Response::html(PROFILE_HTML);
            }
            Response::status(404, "unexpected path")
        });
        let store = new_store("scrape");
        seed_electorate(&store, "sampleford", "Sampleford", "VIC").await;

        sync_aec_profiles(&store, "31496", &Endpoints::at(&server.base))
            .await
            .expect("sync");

        let profile: ElectorateProfile = store
            .get_json("canonical/electorate-profiles/sampleford.json")
            .await
            .unwrap()
            .expect("profile stored");
        assert_eq!(
            profile.profile.name_derivation.as_deref(),
            Some("Named for a sample")
        );
        assert_eq!(profile.profile.location.as_deref(), Some("Inner suburbs"));
        assert_eq!(profile.profile.area.as_deref(), Some("52 sq km"));
        assert_eq!(profile.profile.gazetted.as_deref(), Some("31 July 2024"));
        assert_eq!(profile.profile.first_contested.as_deref(), Some("1949"));
        assert_eq!(
            profile.profile.demographic.as_deref(),
            Some("Inner Metropolitan")
        );
        // Enrolment is matched case-insensitively by division name.
        assert_eq!(profile.enrolment, Some(118432));

        // The enrolment download is kept raw.
        assert!(store
            .get_raw("raw/aec/31496/GeneralEnrolmentByDivisionDownload.csv")
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn a_fresh_profile_is_left_alone_and_a_stale_one_is_refetched() {
        let server = TestServer::start(|req| match req.path.contains("/profiles/") {
            true => Response::html(PROFILE_HTML),
            false => Response::text("h\nStateAb,DivisionNm,Enrolment\nVIC,Sampleford,10"),
        });
        let endpoints = Endpoints::at(&server.base);
        let store = new_store("refresh");
        seed_electorate(&store, "sampleford", "Sampleford", "VIC").await;

        sync_aec_profiles(&store, "31496", &endpoints)
            .await
            .expect("first");
        let after_first = server.hits();
        sync_aec_profiles(&store, "31496", &endpoints)
            .await
            .expect("second");
        // Only the enrolment download is refetched; the profile page is current.
        assert_eq!(server.hits(), after_first + 1);

        let mut profile: ElectorateProfile = store
            .get_json("canonical/electorate-profiles/sampleford.json")
            .await
            .unwrap()
            .expect("profile");
        profile.stored_at = "2020-01-01T00:00:00.000Z".to_string();
        store
            .put_json("canonical/electorate-profiles/sampleford.json", &profile)
            .await
            .unwrap();
        sync_aec_profiles(&store, "31496", &endpoints)
            .await
            .expect("third");
        assert_eq!(
            server.hits(),
            after_first + 3,
            "a stale profile is refetched"
        );
    }

    #[tokio::test]
    async fn a_sync_with_no_electorates_yet_is_an_error() {
        let server = TestServer::start(|_| Response::html(PROFILE_HTML));
        let store = new_store("empty");
        let err = sync_aec_profiles(&store, "31496", &Endpoints::at(&server.base))
            .await
            .expect_err("nothing to profile");
        assert!(err.to_string().contains("no electorates"), "got {err}");
    }

    #[tokio::test]
    async fn a_missing_profile_page_is_survivable_until_ten_of_them() {
        // One failure among many leaves the rest of the run intact.
        let server = TestServer::start(|req| {
            if req.path.contains("/profiles/vic/missing-seat.htm") {
                return Response::status(404, "no profile");
            }
            if req.path.contains("/profiles/") {
                return Response::html(PROFILE_HTML);
            }
            Response::text("h\nStateAb,DivisionNm,Enrolment\nVIC,Sampleford,10")
        });
        let store = new_store("onefailure");
        seed_electorate(&store, "missing-seat", "Missing Seat", "VIC").await;
        seed_electorate(&store, "sampleford", "Sampleford", "VIC").await;

        sync_aec_profiles(&store, "31496", &Endpoints::at(&server.base))
            .await
            .expect("one 404 is not fatal");
        assert!(store
            .get_json::<ElectorateProfile>("canonical/electorate-profiles/sampleford.json")
            .await
            .unwrap()
            .is_some());
        assert!(store
            .get_json::<ElectorateProfile>("canonical/electorate-profiles/missing-seat.json")
            .await
            .unwrap()
            .is_none());

        // Ten failures means something systemic, so the run stops.
        let all_gone = TestServer::start(|req| match req.path.contains("/profiles/") {
            true => Response::status(404, "no profile"),
            false => Response::text("h\nStateAb,DivisionNm,Enrolment\nVIC,Sampleford,10"),
        });
        let store = new_store("tenfailures");
        for n in 0..12 {
            seed_electorate(&store, &format!("seat-{n}"), &format!("Seat {n}"), "VIC").await;
        }
        let err = sync_aec_profiles(&store, "31496", &Endpoints::at(&all_gone.base))
            .await
            .expect_err("ten failures abort");
        assert!(err.to_string().contains("too many failures"), "got {err}");
    }

    #[tokio::test]
    async fn a_seat_in_an_unknown_state_asks_for_an_empty_state_path() {
        let server = TestServer::start(|req| match req.path.contains("/profiles/") {
            // state_path returns "" for anything unexpected, so the URL has an
            // empty segment; the AEC answers 404 and the seat is skipped.
            true => Response::status(404, "no such profile"),
            false => Response::text("h\nStateAb,DivisionNm,Enrolment\nNSW,Elsewhere,10"),
        });
        let store = new_store("unknownstate");
        seed_electorate(&store, "elsewhere", "Elsewhere", "NSW").await;
        sync_aec_profiles(&store, "31496", &Endpoints::at(&server.base))
            .await
            .expect("skipped, not fatal");
        assert_eq!(state_path("NSW"), "nsw");
        assert_eq!(state_path("XX"), "");
    }

    #[tokio::test]
    async fn an_enrolment_download_that_fails_still_leaves_the_profiles_usable() {
        let server = TestServer::start(|req| match req.path.contains("/profiles/") {
            true => Response::html(PROFILE_HTML),
            false => Response::status(500, "enrolment unavailable"),
        });
        let store = new_store("noenrolment");
        seed_electorate(&store, "sampleford", "Sampleford", "VIC").await;

        let mut endpoints = Endpoints::at(&server.base);
        endpoints.backoff_ms = Some(1);
        sync_aec_profiles(&store, "31496", &endpoints)
            .await
            .expect("enrolment is optional");

        let profile: ElectorateProfile = store
            .get_json("canonical/electorate-profiles/sampleford.json")
            .await
            .unwrap()
            .expect("profile stored anyway");
        assert!(
            profile.enrolment.is_none(),
            "no figure rather than a wrong one"
        );
        assert!(
            profile.profile.area.is_some(),
            "the scraped facts still land"
        );
    }
}
