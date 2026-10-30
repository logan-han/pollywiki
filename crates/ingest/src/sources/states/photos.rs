//! Members' photos, from Wikimedia Commons by way of Wikidata, as federal
//! members' are. The parliaments' own portraits are not licensed for reuse,
//! so a member with no freely licensed photo on Commons keeps the initials
//! stand-in.

use super::{plain_name, roll_key, StateReferenceEntry, StateRoll};
use crate::endpoints::Endpoints;
use crate::http::fetch_json;
use crate::js_url::encode_uri_component;
use crate::sources::wikidata::{
    binding_value, commons_file, fetch_commons_licences, mirror_photo, thumb_url, FREE_LICENCES,
};
use crate::store::Store;
use anyhow::Result;
use indexmap::IndexMap;
use pollywiki_schema::{slugify, Photo, StateCode, STATES};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub const PREFIX: &str = "canonical/state-photos/";

/// A sitting member's photo, filed under the chamber and name their
/// parliament's list gives them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatePhoto {
    pub chamber: String,
    pub name: String,
    pub wikidata: String,
    pub photo: Photo,
}

pub fn photos_key(state: StateCode) -> String {
    format!("{PREFIX}{}.json", state.as_str().to_lowercase())
}

// Living people who have held a seat in one of the chambers, with the seats
// they held. Wikidata leaves many past terms open-ended, so an open term
// alone does not make a sitting member: the parliament's list says who sits,
// and this only has to find each one's item.
fn members_query(positions: &[&str]) -> String {
    let values = positions
        .iter()
        .map(|q| format!("wd:{q}"))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        r#"
SELECT ?pos ?person ?personLabel ?alt ?seatLabel ?end ?img WHERE {{
  VALUES ?pos {{ {values} }}
  ?person p:P39 ?ps .
  ?ps ps:P39 ?pos .
  FILTER NOT EXISTS {{ ?person wdt:P570 ?died . }}
  OPTIONAL {{ ?ps pq:P768 ?seat . }}
  OPTIONAL {{ ?ps pq:P582 ?end . }}
  OPTIONAL {{ ?person wdt:P18 ?img . }}
  OPTIONAL {{ ?person skos:altLabel ?alt . FILTER(LANG(?alt) = "en") }}
  SERVICE wikibase:label {{ bd:serviceParam wikibase:language "en" . }}
}}"#
    )
}

/// Someone Wikidata has holding a chamber's position.
#[derive(Debug, Default)]
struct Candidate {
    /// Their label and English aliases, slugged.
    names: BTreeSet<String>,
    /// The seats their terms name, slugged.
    seats: BTreeSet<String>,
    /// Whether any of their terms in the chamber has no end.
    open: bool,
    /// Their photo on Commons; the first by name when there are several.
    file: Option<String>,
}

/// Candidates by position, then by Wikidata item.
fn candidates(bindings: &[Value]) -> IndexMap<String, IndexMap<String, Candidate>> {
    let mut out: IndexMap<String, IndexMap<String, Candidate>> = IndexMap::new();
    for b in bindings {
        let (Some(pos), Some(person)) = (binding_value(b, "pos"), binding_value(b, "person"))
        else {
            continue;
        };
        let pos = pos.rsplit('/').next().unwrap_or(pos);
        let qid = person.rsplit('/').next().unwrap_or(person);
        let c = out
            .entry(pos.to_string())
            .or_default()
            .entry(qid.to_string())
            .or_default();
        for key in ["personLabel", "alt"] {
            if let Some(name) = binding_value(b, key) {
                c.names.insert(slugify(&plain_name(name)));
            }
        }
        if let Some(seat) = binding_value(b, "seatLabel") {
            c.seats.insert(slugify(seat));
        }
        if binding_value(b, "end").is_none() {
            c.open = true;
        }
        if let Some(img) = binding_value(b, "img") {
            let file = commons_file(img);
            if c.file.as_ref().is_none_or(|f| file < *f) {
                c.file = Some(file);
            }
        }
    }
    out
}

/// Whether a seat as Wikidata names it is the one the list gives:
/// "Electoral district of Bundoora" is Bundoora, and "Northern Victoria
/// Region" is Northern Victoria. Both are slugged.
fn same_seat(wikidata: &str, listed: &str) -> bool {
    !listed.is_empty() && format!("-{wikidata}-").contains(&format!("-{listed}-"))
}

/// The item for a member as their parliament lists them. A name is enough
/// when it is held by someone still sitting or by someone who sat for the
/// same seat; a name shared by several is settled by seat, then by who is
/// still sitting. A first name Wikidata spells another way ("Bec" for
/// Rebecca) is matched by surname, but only to someone still sitting for
/// that very seat. Anything less certain finds nobody, so no one is shown
/// with someone else's photo.
fn find<'a>(
    people: &'a IndexMap<String, Candidate>,
    name: &str,
    seat: Option<&str>,
) -> Option<&'a str> {
    let name = slugify(&plain_name(name));
    let seat = seat.map(slugify).unwrap_or_default();
    let holds_seat = |c: &Candidate| c.seats.iter().any(|s| same_seat(s, &seat));
    let named: Vec<(&String, &Candidate)> = people
        .iter()
        .filter(|(_, c)| c.names.contains(&name))
        .collect();
    let picked: Vec<&String> = match named.as_slice() {
        [] => {
            let surname = name.rsplit('-').next().unwrap_or(&name);
            people
                .iter()
                .filter(|(_, c)| {
                    c.open
                        && holds_seat(c)
                        && c.names
                            .iter()
                            .any(|n| n.rsplit('-').next() == Some(surname))
                })
                .map(|(q, _)| q)
                .collect()
        }
        [(q, c)] if c.open || holds_seat(c) => vec![*q],
        [_] => Vec::new(),
        _ => {
            let by_seat: Vec<&String> = named
                .iter()
                .filter(|(_, c)| holds_seat(c))
                .map(|(q, _)| *q)
                .collect();
            if by_seat.len() == 1 {
                by_seat
            } else {
                named
                    .iter()
                    .filter(|(_, c)| c.open)
                    .map(|(q, _)| *q)
                    .collect()
            }
        }
    };
    match picked.as_slice() {
        [only] => Some(only.as_str()),
        _ => None,
    }
}

pub async fn sync_state_photos(
    store: &Store,
    reference: &IndexMap<String, StateReferenceEntry>,
    endpoints: &Endpoints,
) -> Result<()> {
    for code in STATES {
        let (Some(state), Some(entry)) = (StateCode::parse(code), reference.get(code)) else {
            continue;
        };
        let Some(roll) = store.get_json::<StateRoll>(&roll_key(state)).await? else {
            continue;
        };
        let positions: IndexMap<&str, &str> = entry
            .chambers
            .iter()
            .filter_map(|c| Some((c.slug.as_str(), c.wikidata.as_deref()?)))
            .collect();
        if positions.is_empty() {
            continue;
        }
        let query = members_query(&positions.values().copied().collect::<Vec<_>>());
        let mut opts = endpoints.opts(2000);
        opts.accept = Some("application/sparql-results+json".to_string());
        let data: Value = fetch_json(
            &format!(
                "{}?query={}",
                endpoints.wikidata_sparql,
                encode_uri_component(&query)
            ),
            &opts,
        )
        .await?;
        let bindings = data
            .pointer("/results/bindings")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let by_position = candidates(&bindings);

        let mut found: Vec<(&str, &str, &str, &str)> = Vec::new();
        let mut members = 0;
        for chamber in &roll.chambers {
            members += chamber.members.len();
            let Some(people) = positions
                .get(chamber.slug.as_str())
                .and_then(|q| by_position.get(*q))
            else {
                continue;
            };
            for m in &chamber.members {
                let Some(qid) = find(people, &m.name, m.seat.as_deref()) else {
                    continue;
                };
                if let Some(file) = &people[qid].file {
                    found.push((&chamber.slug, &m.name, qid, file));
                }
            }
        }

        let files: Vec<String> = found.iter().map(|f| f.3.to_string()).collect();
        let licences = fetch_commons_licences(&files, endpoints).await?;
        let mut photos: Vec<StatePhoto> = Vec::new();
        for (chamber, name, qid, file) in found {
            let Some(licence) = licences
                .get(file)
                .filter(|l| FREE_LICENCES.is_match(&l.licence))
            else {
                continue;
            };
            let mut photo = Photo {
                commons_file: file.to_string(),
                url: thumb_url(endpoints, file, 400),
                licence: licence.licence.clone(),
                attribution: licence.attribution.clone(),
                thumb: None,
                thumb_large: None,
            };
            mirror_photo(store, &mut photo, &format!("states/{qid}"), endpoints).await?;
            photos.push(StatePhoto {
                chamber: chamber.to_string(),
                name: name.to_string(),
                wikidata: qid.to_string(),
                photo,
            });
        }
        store.put_json(&photos_key(state), &photos).await?;
        println!(
            "state-photos: {} photos of {members} {state} members",
            photos.len()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::states::{RollChamber, RollMember};
    use crate::store::LocalStore;
    use crate::test_http::{Response, TestServer};
    use std::path::PathBuf;

    const VIC_ASSEMBLY: &str = "Q18534408";

    fn row(qid: &str, name: &str, seat: Option<&str>, ended: bool, file: Option<&str>) -> Value {
        let mut row = serde_json::json!({
            "pos": { "value": format!("http://www.wikidata.org/entity/{VIC_ASSEMBLY}") },
            "person": { "value": format!("http://www.wikidata.org/entity/{qid}") },
            "personLabel": { "value": name },
        });
        if let Some(seat) = seat {
            row["seatLabel"] = serde_json::json!({ "value": seat });
        }
        if ended {
            row["end"] = serde_json::json!({ "value": "2022-11-26T00:00:00Z" });
        }
        if let Some(file) = file {
            row["img"] = serde_json::json!({
                "value": format!("http://commons.wikimedia.org/wiki/Special:FilePath/{file}")
            });
        }
        row
    }

    fn people(rows: &[Value]) -> IndexMap<String, Candidate> {
        candidates(rows)
            .swap_remove(VIC_ASSEMBLY)
            .unwrap_or_default()
    }

    #[test]
    fn a_seat_matches_however_wikidata_words_it() {
        assert!(same_seat("electoral-district-of-bundoora", "bundoora"));
        assert!(same_seat("northern-victoria-region", "northern-victoria"));
        assert!(!same_seat("electoral-district-of-bundoora", "bunyip"));
        // A member elected state-wide has no seat to match.
        assert!(!same_seat("electoral-district-of-bundoora", ""));
    }

    #[test]
    fn a_sitting_member_is_found_by_name_however_the_apostrophe_is_typed() {
        let people = people(&[row(
            "Q1",
            "Barbara O'Shea",
            Some("Electoral district of South Brisbane"),
            false,
            Some("Barbara.jpg"),
        )]);
        assert_eq!(
            find(&people, "Barbara O\u{2019}Shea", Some("South Brisbane")),
            Some("Q1")
        );
        assert_eq!(
            find(&people, "The Hon. Barbara O'Shea MP", None),
            Some("Q1")
        );
    }

    #[test]
    fn namesakes_are_told_apart_by_seat_then_by_who_still_sits() {
        let people = people(&[
            row(
                "Q1",
                "John Smith",
                Some("Electoral district of Ripon"),
                true,
                None,
            ),
            row(
                "Q2",
                "John Smith",
                Some("Electoral district of Bendigo East"),
                false,
                None,
            ),
        ]);
        assert_eq!(find(&people, "John Smith", Some("Ripon")), Some("Q1"));
        assert_eq!(
            find(&people, "John Smith", Some("Bendigo East")),
            Some("Q2")
        );
        // Elected state-wide, so only a term still open tells them apart.
        assert_eq!(find(&people, "John Smith", None), Some("Q2"));
    }

    #[test]
    fn a_namesake_who_has_left_another_seat_is_not_taken_for_a_new_member() {
        let people = people(&[row(
            "Q1",
            "Tom Smith",
            Some("Electoral district of Ripon"),
            true,
            Some("Tom.jpg"),
        )]);
        assert_eq!(find(&people, "Tom Smith", Some("Bundaberg")), None);
        // Re-elected for the same seat before Wikidata opened the new term.
        assert_eq!(find(&people, "Tom Smith", Some("Ripon")), Some("Q1"));
    }

    #[test]
    fn a_first_name_spelt_another_way_is_matched_by_surname_and_seat() {
        let mut bec = row(
            "Q1",
            "Bec Thomas",
            Some("Electoral division of Elwick"),
            false,
            Some("Bec.jpg"),
        );
        let sitting = people(&[bec.clone()]);
        assert_eq!(find(&sitting, "Rebecca Thomas", Some("Elwick")), Some("Q1"));
        assert_eq!(find(&sitting, "Rebecca Thomas", Some("Hobart")), None);
        // Not by surname alone once their term there has ended.
        bec["end"] = serde_json::json!({ "value": "2024-05-04T00:00:00Z" });
        let left = people(&[bec]);
        assert_eq!(find(&left, "Rebecca Thomas", Some("Elwick")), None);
    }

    #[test]
    fn an_alias_counts_as_a_name_and_the_first_photo_by_name_is_kept() {
        let mut alias = row("Q1", "Edward Cocks", None, false, Some("Ed Cocks 2024.jpg"));
        alias["alt"] = serde_json::json!({ "value": "Ed Cocks" });
        let other = row(
            "Q1",
            "Edward Cocks",
            None,
            false,
            Some("Cocks portrait.jpg"),
        );
        let people = people(&[alias, other]);
        assert_eq!(find(&people, "Ed Cocks", None), Some("Q1"));
        assert_eq!(people["Q1"].file.as_deref(), Some("Cocks portrait.jpg"));
    }

    fn new_store(name: &str) -> Store {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/state-photos-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Store::Local(LocalStore::new(dir))
    }

    fn member(name: &str, seat: &str) -> RollMember {
        RollMember {
            name: name.to_string(),
            party: "Australian Labor Party".to_string(),
            seat: Some(seat.to_string()),
            since: None,
            offices: Vec::new(),
            profile: None,
        }
    }

    #[tokio::test]
    async fn freely_licensed_photos_are_mirrored_and_filed_by_member() {
        let server = TestServer::start(|req| {
            if req.path.starts_with("/sparql") {
                let rows = serde_json::json!([
                    row(
                        "Q1",
                        "Alex Paterson",
                        Some("Electoral district of Sampleford"),
                        false,
                        Some("Alex.jpg")
                    ),
                    row(
                        "Q2",
                        "Jordan Nguyen",
                        Some("Electoral district of Placeholder Bay"),
                        false,
                        Some("Jordan.jpg")
                    ),
                ]);
                return Response::json(
                    serde_json::json!({ "results": { "bindings": rows } }).to_string(),
                );
            }
            if req.path.starts_with("/commons-files") {
                return Response::bytes(vec![0xff, 0xd8, 0xff, 0xd9], "image/jpeg");
            }
            if req.path.starts_with("/commons") {
                return Response::json(
                    serde_json::json!({ "query": { "pages": {
                        "-1": { "title": "File:Alex.jpg", "imageinfo": [{ "extmetadata": {
                            "LicenseShortName": { "value": "CC BY-SA 4.0" },
                            "Artist": { "value": "<a href=\"x\">A Photographer</a>" }
                        } }] },
                        "-2": { "title": "File:Jordan.jpg", "imageinfo": [{ "extmetadata": {
                            "LicenseShortName": { "value": "Fair use" }
                        } }] }
                    } } })
                    .to_string(),
                );
            }
            Response::status(404, "unexpected path")
        });
        let store = new_store("sync");
        let roll = StateRoll {
            state: StateCode::VIC,
            listed_at: "2026-10-30".to_string(),
            chambers: vec![RollChamber {
                slug: "legislative-assembly".to_string(),
                source: "https://www.parliament.vic.gov.au/members/".to_string(),
                members: vec![
                    member("Alex Paterson", "Sampleford"),
                    member("Jordan Nguyen", "Placeholder Bay"),
                    member("Sam Unlisted", "Elsewhere"),
                ],
            }],
        };
        store
            .put_json(&roll_key(StateCode::VIC), &roll)
            .await
            .unwrap();

        let reference = serde_json::from_value(serde_json::json!({ "VIC": {
            "name": "Parliament of Victoria",
            "url": "https://www.parliament.vic.gov.au",
            "leaderTitle": "Premier",
            "chambers": [{
                "slug": "legislative-assembly", "name": "Legislative Assembly",
                "kind": "lower", "seats": 88, "wikidata": VIC_ASSEMBLY
            }]
        } }))
        .expect("reference fixture");

        sync_state_photos(&store, &reference, &Endpoints::at(&server.base))
            .await
            .expect("sync");

        let photos: Vec<StatePhoto> = store
            .get_json(&photos_key(StateCode::VIC))
            .await
            .unwrap()
            .expect("photos filed");
        assert_eq!(
            photos.len(),
            1,
            "a photo that is not freely licensed is left out"
        );
        let alex = &photos[0];
        assert_eq!(alex.chamber, "legislative-assembly");
        assert_eq!(alex.name, "Alex Paterson");
        assert_eq!(alex.wikidata, "Q1");
        assert_eq!(alex.photo.licence, "CC BY-SA 4.0");
        assert_eq!(alex.photo.attribution, "A Photographer");
        assert_eq!(alex.photo.thumb.as_deref(), Some("/img/states/Q1-96.jpg"));
        assert_eq!(
            alex.photo.thumb_large.as_deref(),
            Some("/img/states/Q1-320.jpg")
        );
        let mirrored = store.list("derived/img/states/").await.unwrap();
        assert!(mirrored.contains(&"derived/img/states/Q1-96.jpg".to_string()));
        assert!(mirrored.contains(&"derived/img/states/Q1-320.jpg".to_string()));
        // No roll, no lookup: the other parliaments file nothing.
        assert!(store
            .get_json::<Vec<StatePhoto>>(&photos_key(StateCode::QLD))
            .await
            .unwrap()
            .is_none());
    }
}
