//! Victoria. The Parliament's member search API lists each house's current
//! members with the seat each sits for, their party, and any ministries and
//! positions, one search per house.

use super::{RollChamber, RollMember};
use crate::endpoints::Endpoints;
use crate::http::fetch_json;
use crate::store::Store;
use anyhow::{bail, Result};
use serde::Deserialize;
use serde_json::Value;

/// The search caps a page at 100; neither house is that large, but a page
/// is asked for until the hits match the total either way.
const PAGE_SIZE: usize = 100;

#[derive(Deserialize)]
struct Search {
    result: SearchResult,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchResult {
    total_matching: usize,
    #[serde(default)]
    hits: Vec<Hit>,
}

#[derive(Deserialize)]
struct Hit {
    title: String,
    url: Option<String>,
    #[serde(default)]
    memberships: Vec<Membership>,
}

#[derive(Deserialize)]
struct Membership {
    title: String,
    #[serde(default)]
    details: Vec<String>,
}

impl Hit {
    fn details<'a>(&'a self, title: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.memberships
            .iter()
            .filter(move |m| m.title == title)
            .flat_map(|m| m.details.iter())
            .map(|d| d.trim())
            .filter(|d| !d.is_empty())
    }
}

pub async fn read(store: &Store, endpoints: &Endpoints) -> Result<Vec<RollChamber>> {
    let mut chambers = Vec::new();
    // The API's own codes for the two houses.
    for (house, slug) in [
        ("10", "legislative-assembly"),
        ("20", "legislative-council"),
    ] {
        let source = format!(
            "{}/api/search/members?member-status=current&member-house={house}&pageSize={PAGE_SIZE}&sortType=2",
            endpoints.vic_parliament
        );
        let mut hits: Vec<Hit> = Vec::new();
        for page in 1.. {
            let raw: Value =
                fetch_json(&format!("{source}&page={page}"), &endpoints.opts(1500)).await?;
            store
                .put_json(&format!("raw/states/vic/{slug}-{page}.json"), &raw)
                .await?;
            let search: Search = serde_json::from_value(raw)?;
            let found = search.result.hits.len();
            hits.extend(search.result.hits);
            if found == 0 || hits.len() >= search.result.total_matching {
                if hits.len() != search.result.total_matching {
                    bail!(
                        "{} of {} {slug} members came back",
                        hits.len(),
                        search.result.total_matching
                    );
                }
                break;
            }
        }
        chambers.push(RollChamber {
            slug: slug.to_string(),
            source: source.clone(),
            members: hits
                .iter()
                .map(|hit| RollMember {
                    name: hit.title.trim().to_string(),
                    party: hit.details("Party").next().unwrap_or("").to_string(),
                    seat: hit.details("Member for").next().map(str::to_string),
                    since: None,
                    offices: hit
                        .details("Ministry")
                        .chain(hit.details("Position"))
                        .map(str::to_string)
                        .collect(),
                    profile: hit
                        .url
                        .as_deref()
                        .map(|path| format!("https://www.parliament.vic.gov.au{path}")),
                })
                .collect(),
        });
    }
    Ok(chambers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::LocalStore;
    use crate::test_http::{Response, TestServer};

    fn hit(name: &str, seat: &str, party: &str, extra: &str) -> String {
        format!(
            r#"{{"id":"x","title":"{name}","house":"10","url":"/members/{slug}/",
                "memberships":[{{"title":"Member for","details":["{seat}"]}},
                               {{"title":"House","details":["Legislative Assembly"]}},
                               {{"title":"Party","details":["{party}"]}}{extra}]}}"#,
            slug = name.to_lowercase().replace(' ', "-")
        )
    }

    #[tokio::test]
    async fn each_house_is_searched_and_paged_to_its_total() {
        let server = TestServer::start(|req| {
            if !req.path.starts_with("/vic/api/search/members") {
                return Response::status(404, "unexpected path");
            }
            assert_eq!(req.query("member-status").as_deref(), Some("current"));
            let body = match (
                req.query("member-house").as_deref(),
                req.query("page").as_deref(),
            ) {
                (Some("10"), Some("1")) => format!(
                    r#"{{"result":{{"totalMatching":3,"hits":[{},{}]}},"error":null}}"#,
                    hit(
                        "Riley Okafor",
                        "Harbourside",
                        "Australian Labor Party",
                        r#",{"title":"Ministry","details":["Premier"]}"#
                    ),
                    hit("Priya Raman", "Exampleton North", "Liberal Party", "")
                ),
                (Some("10"), Some("2")) => format!(
                    r#"{{"result":{{"totalMatching":3,"hits":[{}]}},"error":null}}"#,
                    hit(
                        "Erin Blackwood",
                        "Placeholder Ranges",
                        "Independent",
                        r#",{"title":"Position","details":["Speaker of the Legislative Assembly"]}"#
                    )
                ),
                (Some("20"), Some("1")) => format!(
                    r#"{{"result":{{"totalMatching":1,"hits":[{}]}},"error":null}}"#,
                    hit("Mei Tanaka", "Northern Victoria", "Australian Greens", "")
                ),
                _ => r#"{"result":{"totalMatching":0,"hits":[]},"error":null}"#.to_string(),
            };
            Response::json(body)
        });
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/states-tests/vic");
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::Local(LocalStore::new(dir));

        let chambers = read(&store, &Endpoints::at(&server.base))
            .await
            .expect("read");
        assert_eq!(chambers[0].slug, "legislative-assembly");
        let names: Vec<&str> = chambers[0]
            .members
            .iter()
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(names, ["Riley Okafor", "Priya Raman", "Erin Blackwood"]);
        let premier = &chambers[0].members[0];
        assert_eq!(premier.party, "Australian Labor Party");
        assert_eq!(premier.seat.as_deref(), Some("Harbourside"));
        assert_eq!(premier.offices, ["Premier"]);
        assert_eq!(
            premier.profile.as_deref(),
            Some("https://www.parliament.vic.gov.au/members/riley-okafor/")
        );
        assert_eq!(
            chambers[0].members[2].offices,
            ["Speaker of the Legislative Assembly"]
        );
        assert!(chambers[0].source.contains("member-house=10"));
        assert!(
            !chambers[0].source.contains("page="),
            "the source names the search, not a page"
        );

        assert_eq!(chambers[1].slug, "legislative-council");
        assert_eq!(
            chambers[1].members[0].seat.as_deref(),
            Some("Northern Victoria"),
            "a councillor sits for a region"
        );
    }

    #[tokio::test]
    async fn a_search_that_stops_short_of_its_total_is_an_error() {
        let server = TestServer::start(|req| {
            Response::json(match req.query("page").as_deref() {
                Some("1") => format!(
                    r#"{{"result":{{"totalMatching":5,"hits":[{}]}},"error":null}}"#,
                    hit("Riley Okafor", "Harbourside", "Australian Labor Party", "")
                ),
                _ => r#"{"result":{"totalMatching":5,"hits":[]},"error":null}"#.to_string(),
            })
        });
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/states-tests/vic-short");
        let _ = std::fs::remove_dir_all(&dir);
        let err = read(
            &Store::Local(LocalStore::new(dir)),
            &Endpoints::at(&server.base),
        )
        .await
        .expect_err("a short list is refused");
        assert_eq!(
            err.to_string(),
            "1 of 5 legislative-assembly members came back"
        );
    }
}
