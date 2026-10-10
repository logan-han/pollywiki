//! Acts on the Federal Register of Legislation, keyed by the bill each came
//! from. An Act records its originating bill's ParlInfo id, the id the bills
//! sync already keys on, so the join needs no name matching.

use crate::endpoints::Endpoints;
use crate::http::fetch_json;
use crate::store::Store;
use anyhow::{bail, Result};
use indexmap::IndexMap;
use pollywiki_schema::Act;
use regex::Regex;
use serde::Deserialize;
use std::sync::LazyLock;

pub const ACTS_KEY: &str = "canonical/legislation/acts.json";
const PAGE: usize = 100;

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    value: Vec<Title>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Title {
    id: String,
    name: String,
    year: Option<i64>,
    number: Option<i64>,
    making_date: Option<String>,
    status: Option<String>,
    originating_bill_uri: Option<String>,
}

static BILL_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)billhome(?:%2F|/)([rs]\d+)").unwrap());

/// Every Act made from `first_year` to now that names its bill.
pub async fn sync_legislation(store: &Store, first_year: i32, endpoints: &Endpoints) -> Result<()> {
    let this_year = chrono::Datelike::year(&chrono::Utc::now());
    let mut acts: IndexMap<String, Act> = IndexMap::new();
    for year in first_year..=this_year {
        let mut skip = 0;
        loop {
            let url = format!(
                "{}/Titles?$filter=collection eq 'Act' and year eq {year}\
                 &$select=id,name,number,year,makingDate,status,originatingBillUri\
                 &$orderby=number&$top={PAGE}&$skip={skip}",
                endpoints.legislation
            );
            let page: Page = fetch_json(&url, &endpoints.opts(1000)).await?;
            let count = page.value.len();
            for title in page.value {
                if let Some((bill_id, act)) = to_act(title) {
                    acts.insert(bill_id, act);
                }
            }
            if count < PAGE {
                break;
            }
            skip += PAGE;
        }
    }
    if acts.is_empty() {
        bail!("legislation: no Acts naming their bill since {first_year}");
    }
    println!("legislation: {} Acts joined to their bills", acts.len());
    store.put_json(ACTS_KEY, &acts).await
}

fn to_act(title: Title) -> Option<(String, Act)> {
    let bill_id = BILL_ID
        .captures(title.originating_bill_uri.as_deref()?)?
        .get(1)?
        .as_str()
        .to_lowercase();
    Some((
        bill_id,
        Act {
            id: title.id,
            name: title.name,
            year: title.year,
            number: title.number,
            assent: title
                .making_date
                .map(|d| d.chars().take(10).collect())
                .filter(|d: &String| d.len() == 10),
            status: title.status.unwrap_or_default(),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::LocalStore;
    use crate::test_http::{Response, TestServer};
    use std::path::PathBuf;

    fn new_store(name: &str) -> Store {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/legislation-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Store::Local(LocalStore::new(dir))
    }

    fn title(number: i64, uri: Option<&str>) -> serde_json::Value {
        serde_json::json!({
            "id": format!("C2025A{number:05}"),
            "name": format!("Example Act {number} 2025"),
            "year": 2025,
            "number": number,
            "makingDate": "2025-02-20T00:00:00",
            "status": "InForce",
            "originatingBillUri": uri,
        })
    }

    #[tokio::test]
    async fn acts_are_paged_by_year_and_keyed_by_the_bill_they_came_from() {
        let this_year = chrono::Datelike::year(&chrono::Utc::now());
        let server = TestServer::start(move |req| {
            assert!(req.path.starts_with("/legislation/Titles?"));
            assert!(req.path.contains("orderby=number"), "pages must be stable");
            let skip: usize = req.query("$skip").and_then(|s| s.parse().ok()).unwrap_or(0);
            if !req.path.contains("year%20eq%202025") && !req.path.contains("year eq 2025") {
                return Response::json(r#"{"value":[]}"#);
            }
            // A full first page, then a short second one.
            let titles: Vec<serde_json::Value> = if skip == 0 {
                (1..=100)
                    .map(|n| {
                        let uri = format!(
                            "https://parlinfo.aph.gov.au/parlInfo/search/display/display.w3p;query=Id%3A\"legislation%2Fbillhome%2Fr7{n:03}\""
                        );
                        title(n, Some(&uri))
                    })
                    .collect()
            } else {
                vec![
                    title(101, Some("https://parlinfo.aph.gov.au/billhome/R7999")),
                    title(102, None),
                    title(103, Some("https://example.org/no-bill-here")),
                    // A bill introduced in the Senate carries an s id.
                    title(104, Some("query=Id%3A\"legislation%2Fbillhome%2Fs1484\"")),
                ]
            };
            Response::json(serde_json::json!({ "value": titles }).to_string())
        });
        let store = new_store("acts");
        sync_legislation(&store, 2025, &Endpoints::at(&server.base))
            .await
            .expect("sync");
        // Two pages for 2025, one for each later year.
        assert_eq!(server.hits(), 2 + (this_year - 2025) as usize);

        let acts: IndexMap<String, Act> = store.get_json(ACTS_KEY).await.unwrap().expect("stored");
        assert_eq!(acts.len(), 102, "titles naming no bill are left out");
        let act = &acts["r7015"];
        assert_eq!(act.id, "C2025A00015");
        assert_eq!(act.number, Some(15));
        assert_eq!(act.assent.as_deref(), Some("2025-02-20"));
        assert_eq!(act.status, "InForce");
        assert_eq!(
            acts["r7999"].id, "C2025A00101",
            "an unencoded uri, upper case id"
        );
    }

    #[tokio::test]
    async fn a_register_with_nothing_to_join_fails_the_sync() {
        let server = TestServer::start(|_| Response::json(r#"{"value":[]}"#));
        let store = new_store("empty");
        let err = sync_legislation(&store, 2025, &Endpoints::at(&server.base))
            .await
            .expect_err("nothing joined is a failed sync");
        assert!(err.to_string().contains("no Acts"), "got {err}");
        assert!(store.get_raw(ACTS_KEY).await.unwrap().is_none());
    }
}
