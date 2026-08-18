use crate::http::{fetch_json, FetchOpts};
use crate::js_url::encode_uri_component;
use crate::store::Store;
use anyhow::Result;
use indexmap::IndexMap;
use pollywiki_schema::{slugify, Background, Person, PositionKind, PositionRecord};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const BASE: &str = "https://handbookapi.aph.gov.au/api";
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
pub async fn sync_handbook(store: &Store, people: &mut [Person]) -> Result<()> {
    // The API caps $top at 100, so page through the current members.
    let mut individuals: Vec<HandbookIndividual> = Vec::new();
    let mut raw_values: Vec<Value> = Vec::new();
    let mut skip = 0;
    loop {
        let page: Value = fetch_odata(
            &format!("{BASE}/individuals"),
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
            &format!("{BASE}/ministryrecords"),
            &format!("PHID eq '{phid}'"),
        )
        .await?;
        let shadows = fetch_roles(
            &format!("{BASE}/shadowministryrecords"),
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
    endpoint: &str,
    filter: &str,
    extra: &str,
) -> Result<T> {
    let encoded = format!(
        "{endpoint}?%24filter={}{}",
        encode_uri_component(filter),
        extra.replace('$', "%24")
    );
    match fetch_json(&encoded, &FetchOpts::min_interval(400)).await {
        Ok(value) => Ok(value),
        Err(err) if err.to_string().contains(" 400 ") => {
            let literal = format!(
                "{endpoint}?$filter={}{extra}",
                filter.replace(' ', "%20").replace('\'', "%27")
            );
            fetch_json(&literal, &FetchOpts::min_interval(400)).await
        }
        Err(err) => Err(err),
    }
}

async fn fetch_roles(endpoint: &str, filter: &str) -> Result<Vec<RoleRecord>> {
    // The API caps $top at 100; no individual approaches that many records.
    let data: ODataPage<RoleRecord> = fetch_odata(endpoint, filter, "&$top=100").await?;
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
    use pollywiki_schema::{House, StateCode};

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
}
