use crate::endpoints::Endpoints;
use crate::http::fetch_json;
use crate::js_url::encode_uri_component;
use crate::store::Store;
use anyhow::Result;
use indexmap::IndexMap;
use pollywiki_schema::{slugify, Background, Person, PositionKind, PositionRecord};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Handbook data moves slowly; per-person detail refreshes weekly.
const REFRESH_DAYS: f64 = 7.0;
/// The API uses this sentinel for "still serving".
const OPEN_END: &str = "1900-01-01";

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandbookProfile {
    pub phid: String,
    pub stored_at: String,
    pub background: Background,
    pub positions: Vec<PositionRecord>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct HandbookIndividual {
    #[serde(rename = "PHID")]
    pub phid: Option<String>,
    #[serde(rename = "GivenName")]
    pub given_name: Option<String>,
    #[serde(rename = "PreferredName")]
    pub preferred_name: Option<String>,
    #[serde(rename = "FamilyName")]
    pub family_name: Option<String>,
    #[serde(rename = "DateOfBirth")]
    pub date_of_birth: Option<String>,
    #[serde(rename = "PlaceOfBirth")]
    pub place_of_birth: Option<String>,
    #[serde(rename = "StateOfBirth")]
    pub state_of_birth: Option<String>,
    #[serde(rename = "CountryOfBirth")]
    pub country_of_birth: Option<String>,
    #[serde(rename = "Electorate")]
    pub electorate: Option<String>,
    #[serde(rename = "StateAbbrev")]
    pub state_abbrev: Option<String>,
    #[serde(rename = "Occupations")]
    pub occupations: Option<Vec<String>>,
    #[serde(rename = "Qualifications")]
    pub qualifications: Option<Vec<String>>,
    #[serde(rename = "Honours")]
    pub honours: Option<Vec<String>>,
    #[serde(rename = "ParliamentaryPositions")]
    pub parliamentary_positions: Option<Vec<String>>,
    #[serde(rename = "RepresentedParliaments")]
    pub represented_parliaments: Option<Vec<i64>>,
    #[serde(rename = "ServiceHistory_Start")]
    pub service_history_start: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoleRecord {
    #[serde(rename = "Role")]
    pub role: Option<String>,
    #[serde(rename = "Prep")]
    pub prep: Option<String>,
    #[serde(rename = "Entity")]
    pub entity: Option<String>,
    #[serde(rename = "Ministry")]
    pub ministry: Option<String>,
    #[serde(rename = "RDateStart")]
    pub r_date_start: Option<String>,
    #[serde(rename = "RDateEnd")]
    pub r_date_end: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ODataPage<T> {
    #[serde(default = "Vec::new")]
    value: Vec<T>,
}

impl<T> Default for ODataPage<T> {
    fn default() -> Self {
        ODataPage { value: Vec::new() }
    }
}

/// Career, biographical and position data from the official Parliamentary
/// Handbook OData API. One bulk call lists current parliamentarians; dated
/// ministry and shadow-ministry records are fetched per person and cached.
pub async fn sync_handbook(
    store: &Store,
    people: &mut [Person],
    endpoints: &Endpoints,
) -> Result<()> {
    // The API caps $top at 100, so page through the current members.
    let mut individuals: Vec<HandbookIndividual> = Vec::new();
    let mut raw_values: Vec<Value> = Vec::new();
    let mut skip = 0;
    loop {
        let page: Value = fetch_odata(
            endpoints,
            &format!("{}/individuals", endpoints.handbook),
            "InCurrentParliament eq 'True'",
            &format!("&$top=100&$skip={skip}"),
        )
        .await?;
        let values = page
            .get("value")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let count = values.len();
        for value in values {
            individuals.push(serde_json::from_value(value.clone())?);
            raw_values.push(value);
        }
        if count < 100 {
            break;
        }
        skip += 100;
    }
    store
        .put_json(
            "raw/handbook/individuals.json",
            &serde_json::json!({ "value": raw_values }),
        )
        .await?;

    let by_slug = match_people(&individuals, people);
    let mut fetched = 0;
    let mut skipped = 0;
    for (slug, individual) in &by_slug {
        let Some(phid) = individual.phid.as_deref().filter(|p| !p.is_empty()) else {
            continue;
        };
        let key = format!("canonical/handbook/{slug}.json");
        let existing: Option<HandbookProfile> = store.get_json(&key).await?;
        if let Some(existing) = &existing {
            if existing.phid == phid && age_days(&existing.stored_at) < REFRESH_DAYS {
                skipped += 1;
                continue;
            }
        }

        let ministries = fetch_roles(
            endpoints,
            &format!("{}/ministryrecords", endpoints.handbook),
            &format!("PHID eq '{phid}'"),
        )
        .await?;
        let shadows = fetch_roles(
            endpoints,
            &format!("{}/shadowministryrecords", endpoints.handbook),
            &format!("PHID eq '{phid}'"),
        )
        .await?;

        let mut positions: Vec<PositionRecord> = Vec::new();
        positions.extend(
            ministries
                .iter()
                .map(|r| to_position(r, PositionKind::Ministry)),
        );
        positions.extend(shadows.iter().map(|r| to_position(r, PositionKind::Shadow)));
        positions.extend(
            individual
                .parliamentary_positions
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|role| PositionRecord {
                    role: role.clone(),
                    ministry: None,
                    kind: PositionKind::Position,
                    from: None,
                    to: None,
                }),
        );
        positions.sort_by(|a, b| {
            pollywiki_schema::js_compare(
                b.from.as_deref().unwrap_or(""),
                a.from.as_deref().unwrap_or(""),
            )
        });

        let profile = HandbookProfile {
            phid: phid.to_string(),
            stored_at: crate::now_iso(),
            background: Background {
                born: individual.date_of_birth.clone().filter(|s| !s.is_empty()),
                birthplace: {
                    let state_or_country = individual
                        .state_of_birth
                        .clone()
                        .filter(|s| !s.is_empty())
                        .or_else(|| {
                            individual
                                .country_of_birth
                                .clone()
                                .filter(|s| !s.is_empty())
                        });
                    let joined = [
                        individual.place_of_birth.clone().filter(|s| !s.is_empty()),
                        state_or_country,
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(", ");
                    if joined.is_empty() {
                        None
                    } else {
                        Some(joined)
                    }
                },
                occupations: individual.occupations.clone().unwrap_or_default(),
                qualifications: individual.qualifications.clone().unwrap_or_default(),
                honours: individual.honours.clone().unwrap_or_default(),
                service_start: individual
                    .service_history_start
                    .clone()
                    .filter(|s| !s.is_empty()),
                parliaments: individual
                    .represented_parliaments
                    .clone()
                    .unwrap_or_default(),
            },
            positions,
        };
        store.put_json(&key, &profile).await?;
        fetched += 1;

        // Persist the Handbook id onto the person (the wikidata sync merges ids).
        if let Some(person) = people.iter_mut().find(|p| &p.slug == slug) {
            if person.ids.aph.as_deref() != Some(phid) {
                person.ids.aph = Some(phid.to_string());
                store
                    .put_json(&format!("canonical/people/{}.json", person.slug), person)
                    .await?;
            }
        }
    }
    println!(
        "handbook: {fetched} profiles refreshed, {skipped} current, {}/{} matched",
        by_slug.len(),
        people.len()
    );
    Ok(())
}

/// The API accepts percent-encoded OData queries from some networks and
/// rejects them with 400 from others (observed from GitHub-hosted runners),
/// so fall back to the literal-$ form when the encoded form is refused.
async fn fetch_odata<T: serde::de::DeserializeOwned>(
    endpoints: &Endpoints,
    endpoint: &str,
    filter: &str,
    extra: &str,
) -> Result<T> {
    let encoded = format!(
        "{endpoint}?%24filter={}{}",
        encode_uri_component(filter),
        extra.replace('$', "%24")
    );
    match fetch_json(&encoded, &endpoints.opts(400)).await {
        Ok(value) => Ok(value),
        Err(err) if err.to_string().contains(" 400 ") => {
            let literal = format!(
                "{endpoint}?$filter={}{extra}",
                filter.replace(' ', "%20").replace('\'', "%27")
            );
            fetch_json(&literal, &endpoints.opts(400)).await
        }
        Err(err) => Err(err),
    }
}

async fn fetch_roles(
    endpoints: &Endpoints,
    endpoint: &str,
    filter: &str,
) -> Result<Vec<RoleRecord>> {
    // The API caps $top at 100; no individual approaches that many records.
    let data: ODataPage<RoleRecord> = fetch_odata(endpoints, endpoint, filter, "&$top=100").await?;
    Ok(data.value)
}

fn to_position(r: &RoleRecord, kind: PositionKind) -> PositionRecord {
    let role = [r.role.as_deref(), r.prep.as_deref(), r.entity.as_deref()]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();
    PositionRecord {
        role,
        ministry: r.ministry.clone().filter(|m| !m.is_empty()),
        kind,
        from: clean_date(r.r_date_start.as_deref()),
        to: clean_date(r.r_date_end.as_deref()),
    }
}

fn clean_date(iso: Option<&str>) -> Option<String> {
    let iso = iso?;
    if iso.is_empty() || iso.starts_with(OPEN_END) {
        return None;
    }
    Some(iso.chars().take(10).collect())
}

fn age_days(iso: &str) -> f64 {
    let Ok(then) = chrono::DateTime::parse_from_rfc3339(iso) else {
        return f64::INFINITY;
    };
    (chrono::Utc::now().timestamp_millis() - then.timestamp_millis()) as f64 / 86_400_000.0
}

pub fn match_people<'a>(
    individuals: &'a [HandbookIndividual],
    people: &[Person],
) -> IndexMap<String, &'a HandbookIndividual> {
    let mut out: IndexMap<String, &HandbookIndividual> = IndexMap::new();
    let mut unmatched: Vec<String> = Vec::new();
    for ind in individuals {
        let family = ind.family_name.as_deref().unwrap_or("");
        let given = ind.given_name.as_deref().unwrap_or("");
        let preferred = ind
            .preferred_name
            .as_deref()
            .unwrap_or("")
            .replace(['(', ')'], "")
            .trim()
            .to_string();
        let mut candidates: Vec<String> = vec![slugify(&format!("{given} {family}"))];
        if !preferred.is_empty() {
            candidates.push(slugify(&format!("{preferred} {family}")));
        }
        candidates.retain(|c| !c.is_empty());

        let mut matched = people.iter().find(|p| candidates.contains(&p.slug));
        if matched.is_none() {
            // Fall back to family name + seat when first names diverge.
            let family_slug = slugify(family);
            let electorate_slug = slugify(ind.electorate.as_deref().unwrap_or(""));
            let state_upper = ind.state_abbrev.as_deref().unwrap_or("").to_uppercase();
            let seat_matches: Vec<&Person> = people
                .iter()
                .filter(|p| {
                    slugify(&p.name).ends_with(&format!("-{family_slug}"))
                        && (p.electorate.as_deref() == Some(electorate_slug.as_str())
                            || p.state.is_some_and(|s| s.as_str() == state_upper))
                })
                .collect();
            if seat_matches.len() == 1 {
                matched = Some(seat_matches[0]);
            }
        }
        match matched {
            Some(p) => {
                out.insert(p.slug.clone(), ind);
            }
            None => unmatched.push(format!("{given} {family}")),
        }
    }
    if !unmatched.is_empty() {
        eprintln!(
            "handbook: {} unmatched: {}",
            unmatched.len(),
            unmatched
                .iter()
                .take(8)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::LocalStore;
    use crate::test_http::{Response, TestServer};
    use pollywiki_schema::{House, StateCode};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn new_store(name: &str) -> Store {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/handbook-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Store::Local(LocalStore::new(dir))
    }

    fn person(slug: &str, name: &str) -> Person {
        serde_json::from_str(&format!(
            r#"{{"slug":"{slug}","name":"{name}","house":"representatives",
                 "group":"Example","groupSlug":"example","ids":{{}},"links":{{}}}}"#
        ))
        .expect("person fixture")
    }

    fn individual(json: &str) -> HandbookIndividual {
        serde_json::from_str(json).expect("individual fixture")
    }

    #[test]
    fn roles_join_their_parts_and_keep_their_ministry() {
        let record: RoleRecord = serde_json::from_str(
            r#"{"Role":"Minister","Prep":"for","Entity":"Finance",
                "Ministry":"Example Ministry","RDateStart":"2025-05-03T00:00:00",
                "RDateEnd":null}"#,
        )
        .expect("role fixture");
        let position = to_position(&record, PositionKind::Ministry);
        assert_eq!(position.role, "Minister for Finance");
        assert_eq!(position.ministry.as_deref(), Some("Example Ministry"));
        assert_eq!(position.from.as_deref(), Some("2025-05-03"));
        assert!(position.to.is_none(), "an open role has no end date");
    }

    #[test]
    fn role_parts_that_are_absent_or_blank_are_skipped() {
        let record: RoleRecord = serde_json::from_str(
            r#"{"Role":"Speaker","Prep":"","Entity":null,"Ministry":"",
                "RDateStart":"","RDateEnd":""}"#,
        )
        .expect("role fixture");
        let position = to_position(&record, PositionKind::Position);
        assert_eq!(position.role, "Speaker");
        assert!(
            position.ministry.is_none(),
            "an empty ministry is not a ministry"
        );
        assert!(position.from.is_none());
        assert!(position.to.is_none());
    }

    #[test]
    fn the_open_end_sentinel_reads_as_no_end_date() {
        // The Handbook writes 1900-01-01 for roles that have not ended.
        assert_eq!(clean_date(Some("1900-01-01T00:00:00")), None);
        assert_eq!(
            clean_date(Some("2022-05-23T00:00:00")),
            Some("2022-05-23".to_string())
        );
        assert_eq!(clean_date(Some("")), None);
        assert_eq!(clean_date(None), None);
    }

    #[test]
    fn unparseable_timestamps_read_as_infinitely_old() {
        assert!(age_days("not-a-date").is_infinite());
        assert!(age_days("2020-01-01T00:00:00Z") > 1000.0);
    }

    #[test]
    fn individuals_match_on_given_or_preferred_name() {
        let people = vec![
            person("alex-paterson", "Alex Paterson"),
            person("jordan-nguyen", "Jordan Nguyen"),
        ];
        let individuals = vec![
            individual(
                r#"{"GivenName":"Alexander","PreferredName":"(Alex)","FamilyName":"Paterson"}"#,
            ),
            individual(r#"{"GivenName":"Jordan","FamilyName":"Nguyen"}"#),
        ];
        let matched = match_people(&individuals, &people);
        assert_eq!(matched.len(), 2);
        // The preferred name in brackets is what the canonical slug uses.
        assert!(matched.contains_key("alex-paterson"));
        assert!(matched.contains_key("jordan-nguyen"));
    }

    #[test]
    fn a_diverging_first_name_falls_back_to_family_name_and_seat() {
        let mut mp = person("samantha-kelly", "Samantha Kelly");
        mp.electorate = Some("sampleford".to_string());
        let mut senator = person("morgan-rossi", "Morgan Rossi");
        senator.house = House::Senate;
        senator.state = Some(StateCode::TAS);
        senator.electorate = None;
        let people = vec![mp, senator];

        let individuals = vec![
            // Recorded as "Sam", matched on Kelly plus the electorate.
            individual(r#"{"GivenName":"Sam","FamilyName":"Kelly","Electorate":"Sampleford"}"#),
            // Matched on Rossi plus the state.
            individual(r#"{"GivenName":"M","FamilyName":"Rossi","StateAbbrev":"tas"}"#),
            // Nothing to match on, so left out rather than guessed at.
            individual(r#"{"GivenName":"Unknown","FamilyName":"Person"}"#),
        ];
        let matched = match_people(&individuals, &people);
        assert_eq!(matched.len(), 2);
        assert!(matched.contains_key("samantha-kelly"));
        assert!(matched.contains_key("morgan-rossi"));
    }

    #[test]
    fn an_ambiguous_family_name_is_left_unmatched() {
        let mut one = person("chris-smith", "Chris Smith");
        one.state = Some(StateCode::VIC);
        let mut two = person("dana-smith", "Dana Smith");
        two.state = Some(StateCode::VIC);
        let people = vec![one, two];
        let individuals = vec![individual(
            r#"{"GivenName":"Robin","FamilyName":"Smith","StateAbbrev":"VIC"}"#,
        )];
        // Two seat matches is not a match, so nothing is asserted about either.
        assert!(match_people(&individuals, &people).is_empty());
    }

    /// One current member with every biographical field populated, plus a
    /// ministry and a shadow record.
    fn handbook_server() -> TestServer {
        TestServer::start(|req| {
            if req.path.contains("/individuals") {
                // $skip past the first page must come back empty or the loop
                // would never end.
                let skipped = req
                    .path
                    .split("skip=")
                    .nth(1)
                    .and_then(|s| s.split('&').next())
                    .and_then(|s| s.parse::<i64>().ok())
                    .unwrap_or(0);
                if skipped > 0 {
                    return Response::json(r#"{"value":[]}"#);
                }
                return Response::json(
                    serde_json::json!({ "value": [{
                        "PHID": "ABC123",
                        "GivenName": "Alexandra",
                        "PreferredName": "Alex",
                        "FamilyName": "Paterson",
                        "DateOfBirth": "1975-04-02",
                        "PlaceOfBirth": "Ballarat",
                        "StateOfBirth": "Vic",
                        "CountryOfBirth": "Australia",
                        "Electorate": "Sampleford",
                        "StateAbbrev": "VIC",
                        "Occupations": ["Teacher"],
                        "Qualifications": ["BA, University of Melbourne"],
                        "Honours": ["AM"],
                        "ParliamentaryPositions": ["Member, Standing Committee on Economics"],
                        "RepresentedParliaments": [47, 48],
                        "ServiceHistory_Start": "2022-05-21"
                    }] })
                    .to_string(),
                );
            }
            if req.path.contains("/ministryrecords") {
                return Response::json(
                    serde_json::json!({ "value": [{
                        "Role": "Minister",
                        "Prep": "for",
                        "Entity": "Education",
                        "Ministry": "Second Albanese Ministry",
                        "RDateStart": "2025-05-13",
                        "RDateEnd": "1900-01-01"
                    }] })
                    .to_string(),
                );
            }
            if req.path.contains("/shadowministryrecords") {
                return Response::json(
                    serde_json::json!({ "value": [{
                        "Role": "Shadow Minister",
                        "Prep": "for",
                        "Entity": "Health",
                        "RDateStart": "2019-06-01",
                        "RDateEnd": "2022-05-21"
                    }] })
                    .to_string(),
                );
            }
            Response::status(404, "unexpected path")
        })
    }

    #[tokio::test]
    async fn a_sync_writes_the_profile_and_stamps_the_handbook_id_on_the_person() {
        let server = handbook_server();
        let store = new_store("profile");
        let mut people = vec![person("alex-paterson", "Alex Paterson")];

        sync_handbook(&store, &mut people, &Endpoints::at(&server.base))
            .await
            .expect("sync");

        let profile: HandbookProfile = store
            .get_json("canonical/handbook/alex-paterson.json")
            .await
            .unwrap()
            .expect("profile stored");
        assert_eq!(profile.phid, "ABC123");
        assert_eq!(profile.background.born.as_deref(), Some("1975-04-02"));
        // Birthplace joins the town to the state, preferring state over country.
        assert_eq!(
            profile.background.birthplace.as_deref(),
            Some("Ballarat, Vic")
        );
        assert_eq!(profile.background.occupations, vec!["Teacher"]);
        assert_eq!(profile.background.honours, vec!["AM"]);
        assert_eq!(profile.background.parliaments, vec![47, 48]);
        assert_eq!(
            profile.background.service_start.as_deref(),
            Some("2022-05-21")
        );

        // Ministry, shadow and parliamentary positions all land, newest first.
        assert_eq!(profile.positions.len(), 3);
        assert_eq!(profile.positions[0].role, "Minister for Education");
        assert_eq!(profile.positions[0].kind, PositionKind::Ministry);
        assert_eq!(profile.positions[0].from.as_deref(), Some("2025-05-13"));
        assert!(
            profile.positions[0].to.is_none(),
            "the open-end sentinel means still serving"
        );
        assert_eq!(profile.positions[1].role, "Shadow Minister for Health");
        assert_eq!(profile.positions[1].to.as_deref(), Some("2022-05-21"));
        assert_eq!(profile.positions[2].kind, PositionKind::Position);

        // The id is written back onto the person and persisted.
        assert_eq!(people[0].ids.aph.as_deref(), Some("ABC123"));
        let stored: Person = store
            .get_json("canonical/people/alex-paterson.json")
            .await
            .unwrap()
            .expect("person persisted");
        assert_eq!(stored.ids.aph.as_deref(), Some("ABC123"));

        // The raw listing is kept for replay.
        assert!(store
            .get_json::<Value>("raw/handbook/individuals.json")
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn a_fresh_profile_is_not_refetched_but_a_stale_one_is() {
        let server = handbook_server();
        let endpoints = Endpoints::at(&server.base);
        let store = new_store("refresh");
        let mut people = vec![person("alex-paterson", "Alex Paterson")];

        sync_handbook(&store, &mut people, &endpoints)
            .await
            .expect("first");
        let after_first = server.hits();
        sync_handbook(&store, &mut people, &endpoints)
            .await
            .expect("second");
        assert_eq!(
            server.hits(),
            after_first + 1,
            "only the listing is refetched while the profile is fresh"
        );

        // Age the stored profile past the refresh window and it is fetched again.
        let mut profile: HandbookProfile = store
            .get_json("canonical/handbook/alex-paterson.json")
            .await
            .unwrap()
            .expect("profile");
        profile.stored_at = "2020-01-01T00:00:00.000Z".to_string();
        store
            .put_json("canonical/handbook/alex-paterson.json", &profile)
            .await
            .unwrap();
        sync_handbook(&store, &mut people, &endpoints)
            .await
            .expect("third");
        assert!(
            server.hits() > after_first + 2,
            "a stale profile is refetched"
        );
    }

    #[tokio::test]
    async fn the_encoded_odata_form_falls_back_to_literal_dollars_on_a_400() {
        // Some networks get a bare 400 for the percent-encoded $filter form.
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let server = TestServer::start(move |req| {
            if req.path.contains("%24filter") {
                counter.fetch_add(1, Ordering::SeqCst);
                return Response::status(400, "encoded form refused");
            }
            assert!(req.path.contains("$filter"), "expected the literal form");
            Response::json(r#"{"value":[]}"#)
        });

        let store = new_store("fallback");
        let mut people: Vec<Person> = Vec::new();
        sync_handbook(&store, &mut people, &Endpoints::at(&server.base))
            .await
            .expect("the fallback carries the sync");
        assert!(
            calls.load(Ordering::SeqCst) >= 1,
            "the encoded form was tried first"
        );
    }

    #[tokio::test]
    async fn an_individual_with_no_phid_is_skipped_rather_than_fetched() {
        let server = TestServer::start(|req| {
            if req.path.contains("/individuals") && !req.path.contains("skip=100") {
                return Response::json(
                    serde_json::json!({ "value": [
                        { "PHID": "", "GivenName": "Alex", "FamilyName": "Paterson" },
                        { "GivenName": "Quiet", "FamilyName": "Member" }
                    ] })
                    .to_string(),
                );
            }
            if req.path.contains("/individuals") {
                return Response::json(r#"{"value":[]}"#);
            }
            panic!("no role records should be fetched: {}", req.path);
        });
        let store = new_store("nophid");
        let mut people = vec![person("alex-paterson", "Alex Paterson")];
        sync_handbook(&store, &mut people, &Endpoints::at(&server.base))
            .await
            .expect("sync");
        assert!(store
            .get_json::<HandbookProfile>("canonical/handbook/alex-paterson.json")
            .await
            .unwrap()
            .is_none());
        assert!(people[0].ids.aph.is_none());
    }

    #[tokio::test]
    async fn a_source_that_is_down_surfaces_as_an_error() {
        let server = TestServer::start(|_| Response::status(403, "blocked"));
        let store = new_store("down");
        let mut people = vec![person("alex-paterson", "Alex Paterson")];
        let err = sync_handbook(&store, &mut people, &Endpoints::at(&server.base))
            .await
            .expect_err("a blocked API is a failed sync");
        assert!(err.to_string().contains("403"), "got {err}");
    }
}
