use crate::http::{fetch_bytes, fetch_json, FetchOpts};
use crate::js_url::encode_uri_component;
use crate::store::Store;
use anyhow::Result;
use indexmap::IndexMap;
use pollywiki_schema::{
    slugify, House, PartyFacts, Person, PersonIds, PersonLinks, Photo, StateCode,
};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use std::sync::LazyLock;

const SPARQL_ENDPOINT: &str = "https://query.wikidata.org/sparql";
const COMMONS_API: &str = "https://commons.wikimedia.org/w/api.php";

fn house_position(q: &str) -> Option<House> {
    match q {
        "Q18912794" => Some(House::Representatives),
        "Q6814428" => Some(House::Senate),
        _ => None,
    }
}

// Current members: an open P39 (position held) statement with no end date.
const MEMBERS_QUERY: &str = r#"
SELECT ?person ?personLabel ?houseQ ?partyLabel ?electorateLabel ?img ?start ?article WHERE {
  VALUES ?houseQ { wd:Q18912794 wd:Q6814428 }
  ?person p:P39 ?ps .
  ?ps ps:P39 ?houseQ .
  FILTER NOT EXISTS { ?ps pq:P582 ?end . }
  OPTIONAL { ?ps pq:P580 ?start . }
  OPTIONAL { ?ps pq:P768 ?electorate . }
  OPTIONAL { ?ps pq:P4100 ?party . }
  OPTIONAL { ?person wdt:P18 ?img . }
  OPTIONAL { ?article schema:about ?person ; schema:isPartOf <https://en.wikipedia.org/> . }
  SERVICE wikibase:label { bd:serviceParam wikibase:language "en" . }
}"#;

#[derive(Debug, Clone)]
pub struct RawMember {
    pub wikidata: String,
    pub name: String,
    pub house: House,
    pub group: Option<String>,
    pub district: Option<String>,
    pub commons_file: Option<String>,
    pub since: Option<String>,
    pub wikipedia: Option<String>,
}

static FREE_LICENCES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(cc0|cc.by(.sa)?.\d|public domain|pd|no restrictions|attribution)").unwrap()
});

fn binding_value<'a>(binding: &'a Value, key: &str) -> Option<&'a str> {
    binding.get(key)?.get("value")?.as_str()
}

pub async fn sync_wikidata(store: &Store) -> Result<Vec<Person>> {
    let url = format!(
        "{SPARQL_ENDPOINT}?query={}",
        encode_uri_component(MEMBERS_QUERY)
    );
    let mut opts = FetchOpts::min_interval(2000);
    opts.accept = Some("application/sparql-results+json".to_string());
    let data: Value = fetch_json(&url, &opts).await?;
    store.put_json("raw/wikidata/members.json", &data).await?;

    let bindings = data
        .pointer("/results/bindings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut members = dedupe(&bindings);
    let overrides = load_overrides();
    static BARE_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^Q\d+$").unwrap());
    for m in &mut members {
        if let Some(name) = overrides
            .get(&m.wikidata)
            .and_then(|o| o.get("name"))
            .and_then(Value::as_str)
        {
            m.name = name.to_string();
        }
        // A brand-new item (e.g. after a by-election) can lack an English label,
        // in which case the label service hands back the bare entity id.
        if BARE_ID.is_match(&m.name) {
            eprintln!(
                "wikidata: {} ({}) has no English label; add an entry to data/reference/people-overrides.json",
                m.wikidata,
                m.district.as_deref().unwrap_or(m.house.as_str()),
            );
        }
    }

    let files: Vec<String> = members
        .iter()
        .filter_map(|m| m.commons_file.clone())
        .collect();
    let licences = fetch_commons_licences(&files).await?;
    let mut people = people_from_members(&members, &licences);

    for person in &mut people {
        if person.photo.is_some() {
            mirror_photo(store, person).await?;
        }
        // Other sources enrich people (e.g. the TVFY id crosswalk); merge rather
        // than clobber what this source does not own.
        let key = format!("canonical/people/{}.json", person.slug);
        if let Some(existing) = store.get_json::<Person>(&key).await? {
            let mut ids = existing.ids;
            if person.ids.wikidata.is_some() {
                ids.wikidata = person.ids.wikidata.clone();
            }
            person.ids = ids;
        }
        store.put_json(&key, person).await?;
    }

    // People are owned entirely by this source; prune entries that no longer
    // correspond to a current member (departures, renames, label fixes).
    let current: std::collections::HashSet<String> = people
        .iter()
        .map(|p| format!("canonical/people/{}.json", p.slug))
        .collect();
    for key in store.list("canonical/people/").await? {
        if !current.contains(&key) {
            println!("wikidata: pruning stale {key}");
            store.delete(&key).await?;
        }
    }

    sync_party_facts(store, &people).await?;
    Ok(people)
}

/// Founding dates, websites and Wikipedia links for parliamentary groups.
async fn sync_party_facts(store: &Store, people: &[Person]) -> Result<()> {
    let mut groups: IndexMap<String, String> = IndexMap::new();
    for p in people {
        if p.group_slug != "independent" {
            groups.insert(p.group_slug.clone(), p.group.clone());
        }
    }
    let labels = groups
        .values()
        .map(|n| format!("\"{}\"@en", n.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" ");
    let query = format!(
        r#"
SELECT ?label ?inception ?website ?article WHERE {{
  VALUES ?label {{ {labels} }}
  ?item rdfs:label ?label .
  ?item wdt:P31 ?type .
  FILTER(?type IN (wd:Q7278, wd:Q124964, wd:Q1140229))
  OPTIONAL {{ ?item wdt:P571 ?inception . }}
  OPTIONAL {{ ?item wdt:P856 ?website . }}
  OPTIONAL {{ ?article schema:about ?item ; schema:isPartOf <https://en.wikipedia.org/> . }}
}}"#
    );
    let mut opts = FetchOpts::min_interval(2000);
    opts.accept = Some("application/sparql-results+json".to_string());
    let data: Value = fetch_json(
        &format!("{SPARQL_ENDPOINT}?query={}", encode_uri_component(&query)),
        &opts,
    )
    .await?;

    let bindings = data
        .pointer("/results/bindings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut by_slug: IndexMap<String, PartyFacts> = IndexMap::new();
    for b in &bindings {
        let Some(name) = binding_value(b, "label") else {
            continue;
        };
        let slug = slugify(name);
        if slug.is_empty() || !groups.contains_key(&slug) || by_slug.contains_key(&slug) {
            continue;
        }
        by_slug.insert(
            slug,
            PartyFacts {
                founded: binding_value(b, "inception").map(|v| v.chars().take(10).collect()),
                website: binding_value(b, "website").map(str::to_string),
                wikipedia: binding_value(b, "article").map(str::to_string),
            },
        );
    }
    for (slug, facts) in &by_slug {
        store
            .put_json(&format!("canonical/party-facts/{slug}.json"), facts)
            .await?;
    }
    let missing: Vec<&str> = groups
        .keys()
        .filter(|s| !by_slug.contains_key(*s))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        eprintln!(
            "wikidata: no party facts matched for {}",
            missing.join(", ")
        );
    }
    println!(
        "wikidata: party facts for {}/{} groups",
        by_slug.len(),
        groups.len()
    );
    Ok(())
}

fn load_overrides() -> IndexMap<String, Value> {
    let path = crate::reference_path("people-overrides.json");
    match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
        Err(_) => IndexMap::new(),
    }
}

pub fn people_from_members(
    members: &[RawMember],
    licences: &IndexMap<String, CommonsLicence>,
) -> Vec<Person> {
    let mut people: Vec<Person> = Vec::new();
    let mut taken: IndexMap<String, ()> = IndexMap::new();
    for m in members {
        let mut slug = slugify(&m.name);
        if taken.contains_key(&slug) {
            let extra = m
                .district
                .clone()
                .unwrap_or_else(|| m.house.as_str().to_string());
            slug = slugify(&format!("{} {extra}", m.name));
        }
        taken.insert(slug.clone(), ());

        let group = m.group.clone().unwrap_or_else(|| "Independent".to_string());
        let is_senator = m.house == House::Senate;
        let state = if is_senator {
            as_state(m.district.as_deref())
        } else {
            None
        };
        let licence = m.commons_file.as_ref().and_then(|f| licences.get(f));

        let photo = match (&m.commons_file, licence) {
            (Some(file), Some(lic)) if FREE_LICENCES.is_match(&lic.licence) => Some(Photo {
                commons_file: file.clone(),
                url: thumb_url(file, 400),
                licence: lic.licence.clone(),
                attribution: lic.attribution.clone(),
                thumb: None,
                thumb_large: None,
            }),
            _ => None,
        };

        people.push(Person {
            slug,
            name: m.name.clone(),
            house: m.house,
            state,
            electorate: match (&m.district, is_senator) {
                (Some(d), false) => Some(slugify(d)),
                _ => None,
            },
            group: group.clone(),
            group_slug: slugify(&group),
            since: m.since.as_deref().map(|s| s.chars().take(10).collect()),
            ids: PersonIds {
                wikidata: Some(m.wikidata.clone()),
                ..Default::default()
            },
            photo,
            links: PersonLinks {
                wikipedia: m.wikipedia.clone(),
            },
            ai_note: None,
            background: None,
            positions: None,
            elections: None,
            stats: None,
        });
    }
    people
}

pub fn dedupe(bindings: &[Value]) -> Vec<RawMember> {
    struct Entry {
        start: String,
        member: RawMember,
    }
    let mut by_id: IndexMap<String, Entry> = IndexMap::new();
    for b in bindings {
        let Some(uri) = binding_value(b, "person") else {
            continue;
        };
        let Some(name) = binding_value(b, "personLabel") else {
            continue;
        };
        let house_q = binding_value(b, "houseQ")
            .and_then(|v| v.rsplit('/').next())
            .unwrap_or("");
        let Some(house) = house_position(house_q) else {
            continue;
        };
        let start = binding_value(b, "start").unwrap_or("").to_string();
        // A person can carry several open statements; keep the most recent seat.
        if let Some(existing) = by_id.get(uri) {
            if existing.start >= start {
                continue;
            }
        }
        let commons_file = binding_value(b, "img").map(|img| {
            let after = img
                .split("/Special:FilePath/")
                .last()
                .unwrap_or("")
                .to_string();
            percent_encoding::percent_decode_str(&after)
                .decode_utf8_lossy()
                .into_owned()
        });
        by_id.insert(
            uri.to_string(),
            Entry {
                start: start.clone(),
                member: RawMember {
                    wikidata: uri.rsplit('/').next().unwrap_or(uri).to_string(),
                    name: name.to_string(),
                    house,
                    group: binding_value(b, "partyLabel").map(str::to_string),
                    district: binding_value(b, "electorateLabel").map(str::to_string),
                    commons_file,
                    since: if start.is_empty() { None } else { Some(start) },
                    wikipedia: binding_value(b, "article").map(str::to_string),
                },
            },
        );
    }
    by_id.into_values().map(|e| e.member).collect()
}

fn as_state(label: Option<&str>) -> Option<StateCode> {
    let label = label?;
    let code = match label.to_lowercase().as_str() {
        "new south wales" => Some(StateCode::NSW),
        "victoria" => Some(StateCode::VIC),
        "queensland" => Some(StateCode::QLD),
        "western australia" => Some(StateCode::WA),
        "south australia" => Some(StateCode::SA),
        "tasmania" => Some(StateCode::TAS),
        "australian capital territory" => Some(StateCode::ACT),
        "northern territory" => Some(StateCode::NT),
        _ => None,
    };
    code.or_else(|| StateCode::parse(label))
}

fn thumb_url(file: &str, width: u32) -> String {
    format!(
        "https://commons.wikimedia.org/wiki/Special:FilePath/{}?width={width}",
        encode_uri_component(file)
    )
}

const THUMB_SIZES: [u32; 2] = [96, 320];

#[derive(Deserialize)]
struct PhotoMarker {
    #[serde(rename = "commonsFile")]
    commons_file: String,
}

/// Mirrors Commons thumbnails into the store so CloudFront serves them
/// same-origin (the deploy job copies derived/img/ to the site prefix).
/// Hotlinking Commons meant a redirect chain per avatar and no edge caching.
async fn mirror_photo(store: &Store, person: &mut Person) -> Result<()> {
    let Some(photo) = person.photo.as_mut() else {
        return Ok(());
    };
    let marker_key = format!("derived/img/people/{}.src.json", person.slug);
    let marker: Option<PhotoMarker> = store.get_json(&marker_key).await?;

    if marker.map(|m| m.commons_file) != Some(photo.commons_file.clone()) {
        let mut ok = true;
        for size in THUMB_SIZES {
            match fetch_bytes(
                &thumb_url(&photo.commons_file, size),
                &FetchOpts::min_interval(400),
            )
            .await
            {
                Ok(bytes) => {
                    store
                        .put_raw(
                            &format!("derived/img/people/{}-{size}.jpg", person.slug),
                            &bytes,
                        )
                        .await?;
                }
                Err(err) => {
                    eprintln!("wikidata: photo mirror failed for {}: {err}", person.slug);
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            return Ok(()); // keep the Commons URL fallback; retried next sync
        }
        store
            .put_json(
                &marker_key,
                &serde_json::json!({ "commonsFile": photo.commons_file }),
            )
            .await?;
    }
    photo.thumb = Some(format!("/img/people/{}-96.jpg", person.slug));
    photo.thumb_large = Some(format!("/img/people/{}-320.jpg", person.slug));
    Ok(())
}

pub struct CommonsLicence {
    pub licence: String,
    pub attribution: String,
}

async fn fetch_commons_licences(files: &[String]) -> Result<IndexMap<String, CommonsLicence>> {
    let mut out = IndexMap::new();
    for batch in files.chunks(40) {
        let titles = batch
            .iter()
            .map(|f| format!("File:{f}"))
            .collect::<Vec<_>>()
            .join("|");
        let url = format!(
            "{COMMONS_API}?action=query&prop=imageinfo&iiprop=extmetadata&iiextmetadatafilter=LicenseShortName|Artist&format=json&titles={}",
            encode_uri_component(&titles)
        );
        let data: Value = fetch_json(&url, &FetchOpts::min_interval(1500)).await?;
        let pages = data
            .pointer("/query/pages")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for page in pages.values() {
            let Some(file) = page
                .get("title")
                .and_then(Value::as_str)
                .map(|t| t.strip_prefix("File:").unwrap_or(t).to_string())
            else {
                continue;
            };
            let Some(meta) = page.pointer("/imageinfo/0/extmetadata") else {
                continue;
            };
            out.insert(
                file,
                CommonsLicence {
                    licence: meta
                        .pointer("/LicenseShortName/value")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string(),
                    attribution: strip_html(
                        meta.pointer("/Artist/value")
                            .and_then(Value::as_str)
                            .unwrap_or("Wikimedia Commons"),
                    ),
                },
            );
        }
    }
    Ok(out)
}

fn strip_html(html: &str) -> String {
    static TAGS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]*>").unwrap());
    let stripped = TAGS.replace_all(html, "").trim().to_string();
    if stripped.is_empty() {
        "Wikimedia Commons".to_string()
    } else {
        stripped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bindings(json: &str) -> Vec<Value> {
        serde_json::from_str(json).expect("bindings fixture")
    }

    fn licence(licence: &str, attribution: &str) -> CommonsLicence {
        CommonsLicence {
            licence: licence.to_string(),
            attribution: attribution.to_string(),
        }
    }

    #[test]
    fn only_the_two_chamber_positions_are_recognised() {
        assert_eq!(house_position("Q18912794"), Some(House::Representatives));
        assert_eq!(house_position("Q6814428"), Some(House::Senate));
        // Any other P39 position is not a seat in this parliament.
        assert_eq!(house_position("Q123456"), None);
    }

    #[test]
    fn state_labels_and_codes_both_resolve() {
        assert_eq!(as_state(Some("New South Wales")), Some(StateCode::NSW));
        assert_eq!(as_state(Some("tasmania")), Some(StateCode::TAS));
        // A bare code still parses, which is what the fallback is for.
        assert_eq!(as_state(Some("QLD")), Some(StateCode::QLD));
        assert_eq!(as_state(Some("Wentworth")), None);
        assert_eq!(as_state(None), None);
    }

    #[test]
    fn thumbnails_percent_encode_the_commons_file_name() {
        assert_eq!(
            thumb_url("Jane Smith 2026.jpg", 96),
            "https://commons.wikimedia.org/wiki/Special:FilePath/Jane%20Smith%202026.jpg?width=96"
        );
        assert!(thumb_url("Caf\u{e9}.jpg", 320).contains("Caf%C3%A9.jpg?width=320"));
    }

    #[test]
    fn attribution_falls_back_when_the_credit_is_only_markup() {
        assert_eq!(strip_html("<a href=\"x\">Jane Smith</a>"), "Jane Smith");
        assert_eq!(strip_html("<span></span>"), "Wikimedia Commons");
        assert_eq!(strip_html("   "), "Wikimedia Commons");
    }

    #[test]
    fn dedupe_keeps_the_most_recent_open_seat_per_person() {
        let members = dedupe(&bindings(
            r#"[
              {"person":{"value":"http://www.wikidata.org/entity/Q1"},
               "personLabel":{"value":"Jane Smith"},
               "houseQ":{"value":"http://www.wikidata.org/entity/Q18912794"},
               "partyLabel":{"value":"Example Party"},
               "electorateLabel":{"value":"Sampleford"},
               "start":{"value":"2019-05-18T00:00:00Z"}},
              {"person":{"value":"http://www.wikidata.org/entity/Q1"},
               "personLabel":{"value":"Jane Smith"},
               "houseQ":{"value":"http://www.wikidata.org/entity/Q18912794"},
               "partyLabel":{"value":"Example Party"},
               "electorateLabel":{"value":"Placeholder Bay"},
               "start":{"value":"2025-05-03T00:00:00Z"}},
              {"person":{"value":"http://www.wikidata.org/entity/Q2"},
               "personLabel":{"value":"Bob Jones"},
               "houseQ":{"value":"http://www.wikidata.org/entity/Q6814428"},
               "electorateLabel":{"value":"Tasmania"},
               "img":{"value":"http://commons.wikimedia.org/wiki/Special:FilePath/Bob%20Jones.jpg"},
               "article":{"value":"https://en.wikipedia.org/wiki/Bob_Jones"}},
              {"person":{"value":"http://www.wikidata.org/entity/Q3"},
               "personLabel":{"value":"Not A Member"},
               "houseQ":{"value":"http://www.wikidata.org/entity/Q999"}},
              {"personLabel":{"value":"No Uri"}}
            ]"#,
        ));

        assert_eq!(
            members.len(),
            2,
            "non-seat positions and headless rows drop"
        );
        let jane = members.iter().find(|m| m.wikidata == "Q1").expect("Q1");
        assert_eq!(
            jane.district.as_deref(),
            Some("Placeholder Bay"),
            "the later open statement wins"
        );
        assert_eq!(jane.since.as_deref(), Some("2025-05-03T00:00:00Z"));

        let bob = members.iter().find(|m| m.wikidata == "Q2").expect("Q2");
        // The Commons file name is percent-decoded out of the image URL.
        assert_eq!(bob.commons_file.as_deref(), Some("Bob Jones.jpg"));
        assert!(bob.since.is_none(), "an absent start date stays absent");
        assert_eq!(
            bob.wikipedia.as_deref(),
            Some("https://en.wikipedia.org/wiki/Bob_Jones")
        );
    }

    #[test]
    fn members_become_people_with_seats_slugs_and_free_photos_only() {
        let members = dedupe(&bindings(
            r#"[
              {"person":{"value":"http://www.wikidata.org/entity/Q1"},
               "personLabel":{"value":"Jane Smith"},
               "houseQ":{"value":"http://www.wikidata.org/entity/Q18912794"},
               "partyLabel":{"value":"Example Party"},
               "electorateLabel":{"value":"Sampleford"},
               "img":{"value":"http://commons.wikimedia.org/wiki/Special:FilePath/Free.jpg"}},
              {"person":{"value":"http://www.wikidata.org/entity/Q2"},
               "personLabel":{"value":"Bob Jones"},
               "houseQ":{"value":"http://www.wikidata.org/entity/Q6814428"},
               "electorateLabel":{"value":"Tasmania"},
               "img":{"value":"http://commons.wikimedia.org/wiki/Special:FilePath/Restricted.jpg"}},
              {"person":{"value":"http://www.wikidata.org/entity/Q3"},
               "personLabel":{"value":"Casey Doe"},
               "houseQ":{"value":"http://www.wikidata.org/entity/Q6814428"},
               "electorateLabel":{"value":"Victoria"}}
            ]"#,
        ));
        let mut licences = IndexMap::new();
        licences.insert(
            "Free.jpg".to_string(),
            licence("CC BY-SA 4.0", "A Photographer"),
        );
        licences.insert(
            "Restricted.jpg".to_string(),
            licence("All rights reserved", "Someone"),
        );

        let people = people_from_members(&members, &licences);
        assert_eq!(people.len(), 3);

        // A member gets an electorate slug; a senator gets a state instead.
        let jane = &people[0];
        assert_eq!(jane.slug, "jane-smith");
        assert_eq!(jane.electorate.as_deref(), Some("sampleford"));
        assert!(jane.state.is_none());
        assert_eq!(jane.group_slug, "example-party");
        assert_eq!(jane.ids.wikidata.as_deref(), Some("Q1"));
        // Only a free licence is reproduced.
        let photo = jane.photo.as_ref().expect("free photo kept");
        assert_eq!(photo.attribution, "A Photographer");

        let bob = &people[1];
        assert_eq!(bob.state, Some(StateCode::TAS));
        assert!(bob.electorate.is_none());
        assert!(bob.photo.is_none(), "a non-free image is never reproduced");

        // No party statement means Independent, not a blank group.
        let casey = &people[2];
        assert_eq!(casey.group, "Independent");
        assert_eq!(casey.group_slug, "independent");
        assert!(casey.photo.is_none(), "no image at all means no photo");
    }

    #[test]
    fn people_sharing_a_name_get_distinct_slugs() {
        let members = dedupe(&bindings(
            r#"[
              {"person":{"value":"http://www.wikidata.org/entity/Q1"},
               "personLabel":{"value":"Jane Smith"},
               "houseQ":{"value":"http://www.wikidata.org/entity/Q18912794"},
               "electorateLabel":{"value":"Sampleford"}},
              {"person":{"value":"http://www.wikidata.org/entity/Q2"},
               "personLabel":{"value":"Jane Smith"},
               "houseQ":{"value":"http://www.wikidata.org/entity/Q6814428"},
               "electorateLabel":{"value":"Tasmania"}}
            ]"#,
        ));
        let people = people_from_members(&members, &IndexMap::new());
        let slugs: Vec<&str> = people.iter().map(|p| p.slug.as_str()).collect();
        assert_eq!(slugs, vec!["jane-smith", "jane-smith-tasmania"]);
    }

    /// Replays the cached SPARQL response through dedupe + people_from_members
    /// and checks every non-photo field against the canonical store the
    /// reference implementation wrote from the same input. Skips when the
    /// local development store is absent (e.g. in CI).
    #[test]
    fn people_match_canonical_store_fixture() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let raw_path = root.join(".store/raw/wikidata/members.json");
        let people_dir = root.join(".store/canonical/people");
        if !raw_path.exists() || !people_dir.exists() {
            eprintln!("fixture store missing; skipping");
            return;
        }
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(raw_path).unwrap()).unwrap();
        let bindings = raw
            .pointer("/results/bindings")
            .and_then(Value::as_array)
            .cloned()
            .unwrap();
        let mut members = dedupe(&bindings);
        let overrides: IndexMap<String, Value> =
            std::fs::read_to_string(root.join("data/reference/people-overrides.json"))
                .ok()
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_default();
        for m in &mut members {
            if let Some(name) = overrides
                .get(&m.wikidata)
                .and_then(|o| o.get("name"))
                .and_then(Value::as_str)
            {
                m.name = name.to_string();
            }
        }
        let people = people_from_members(&members, &IndexMap::new());

        // Index the canonical store by wikidata id so a name-override added
        // after the store was written (a rename) still pairs up.
        let mut canonical_by_id: IndexMap<String, Value> = IndexMap::new();
        for entry in std::fs::read_dir(&people_dir).unwrap() {
            let value: Value =
                serde_json::from_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap())
                    .unwrap();
            if let Some(id) = value.pointer("/ids/wikidata").and_then(Value::as_str) {
                canonical_by_id.insert(id.to_string(), value);
            }
        }

        let mut checked = 0;
        let mut renamed = 0;
        for person in &people {
            let canonical = canonical_by_id
                .get(person.ids.wikidata.as_deref().unwrap())
                .unwrap_or_else(|| panic!("no canonical entry for {}", person.slug));
            if canonical["name"].as_str().unwrap() != person.name {
                renamed += 1; // override landed after the store was written
                continue;
            }
            assert_eq!(canonical["slug"].as_str().unwrap(), person.slug);
            assert_eq!(
                canonical["house"].as_str().unwrap(),
                person.house.as_str(),
                "{}",
                person.slug
            );
            assert_eq!(
                canonical.get("state").and_then(Value::as_str),
                person.state.map(|s| s.as_str()),
                "{}",
                person.slug
            );
            assert_eq!(
                canonical.get("electorate").and_then(Value::as_str),
                person.electorate.as_deref(),
                "{}",
                person.slug
            );
            assert_eq!(
                canonical["group"].as_str().unwrap(),
                person.group,
                "{}",
                person.slug
            );
            assert_eq!(
                canonical["groupSlug"].as_str().unwrap(),
                person.group_slug,
                "{}",
                person.slug
            );
            assert_eq!(
                canonical.get("since").and_then(Value::as_str),
                person.since.as_deref(),
                "{}",
                person.slug
            );
            assert_eq!(
                canonical.pointer("/ids/wikidata").and_then(Value::as_str),
                person.ids.wikidata.as_deref(),
                "{}",
                person.slug
            );
            assert_eq!(
                canonical
                    .pointer("/links/wikipedia")
                    .and_then(Value::as_str),
                person.links.wikipedia.as_deref(),
                "{}",
                person.slug
            );
            checked += 1;
        }
        assert!(renamed <= 1, "unexpected renames: {renamed}");
        assert_eq!(
            checked + renamed,
            226,
            "expected the full current membership"
        );
    }
}
