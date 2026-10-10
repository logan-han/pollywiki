//! Parliamentarians' expenses as the Independent Parliamentary Expenses
//! Authority publishes them each quarter on data.gov.au. Each quarter's
//! transaction file (12 MB or so) is summed per parliamentarian and IPEA
//! category, and kept, so a quarter is only downloaded again when IPEA
//! republishes it.

use crate::endpoints::Endpoints;
use crate::http::{fetch_json, fetch_text};
use crate::store::Store;
use anyhow::{anyhow, bail, Result};
use indexmap::IndexMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

/// Quarters kept on each member's page.
pub const QUARTERS: usize = 4;
pub const PREFIX: &str = "canonical/ipea/";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IpeaQuarter {
    /// IPEA's reporting period id, e.g. "2026Q02".
    pub period: String,
    /// IPEA's label, e.g. "Apr-Jun 2026".
    pub label: String,
    pub source_url: String,
    pub last_modified: String,
    pub parliamentarians: Vec<IpeaParliamentarian>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IpeaParliamentarian {
    pub office_code: String,
    pub name: String,
    pub first_name: String,
    pub surname: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub electorate: Option<String>,
    pub state: String,
    /// Cents per IPEA high-level category, largest first.
    pub categories: Vec<(String, i64)>,
}

#[derive(Deserialize)]
struct Search {
    result: SearchResult,
}

#[derive(Deserialize)]
struct SearchResult {
    #[serde(default)]
    results: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
    #[serde(default)]
    resources: Vec<Resource>,
}

#[derive(Deserialize)]
struct Resource {
    #[serde(default)]
    name: String,
    #[serde(default)]
    url: String,
    last_modified: Option<String>,
    metadata_modified: Option<String>,
}

static PERIOD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(\d{4})q0?([1-4])_dataextract(?:_transactional)?\.csv$").unwrap()
});

pub async fn sync_ipea(store: &Store, endpoints: &Endpoints) -> Result<()> {
    let search: Search = fetch_json(
        &format!(
            "{}/package_search?fq=organization:ipea&rows=200",
            endpoints.data_gov
        ),
        &endpoints.opts(1000),
    )
    .await?;
    // The newest quarters' transaction files, one per quarter.
    let mut files: IndexMap<String, (String, String)> = IndexMap::new();
    for resource in search
        .result
        .results
        .iter()
        .flat_map(|p| &p.resources)
        .filter(|r| r.name.to_lowercase().contains("expenses"))
    {
        let Some(caps) = PERIOD.captures(&resource.url) else {
            continue;
        };
        let period = format!("{}Q0{}", &caps[1], &caps[2]);
        let modified = resource
            .last_modified
            .clone()
            .or_else(|| resource.metadata_modified.clone())
            .unwrap_or_default();
        files.insert(period, (resource.url.clone(), modified));
    }
    files.sort_keys();
    let latest: Vec<(String, (String, String))> = files.into_iter().rev().take(QUARTERS).collect();
    if latest.is_empty() {
        bail!("ipea: no quarterly expenses files on data.gov.au");
    }

    let mut fetched = 0;
    for (period, (url, modified)) in &latest {
        let key = format!("{PREFIX}{period}.json");
        if let Some(existing) = store.get_json::<IpeaQuarter>(&key).await? {
            if &existing.source_url == url && &existing.last_modified == modified {
                continue;
            }
        }
        let csv = fetch_text(url, &endpoints.opts(1000)).await?;
        let (label, parliamentarians) = summarise_quarter(&csv)?;
        store
            .put_json(
                &key,
                &IpeaQuarter {
                    period: period.clone(),
                    label,
                    source_url: url.clone(),
                    last_modified: modified.clone(),
                    parliamentarians,
                },
            )
            .await?;
        fetched += 1;
    }
    // Quarters that have rolled off the end leave the store with them.
    let keep: Vec<String> = latest
        .iter()
        .map(|(period, _)| format!("{PREFIX}{period}.json"))
        .collect();
    for key in store.list(PREFIX).await? {
        if !keep.contains(&key) {
            store.delete(&key).await?;
        }
    }
    println!(
        "ipea: {fetched} of {} quarters downloaded, the rest unchanged",
        latest.len()
    );
    Ok(())
}

/// Each parliamentarian's spending in one quarter's transaction file, summed
/// per IPEA high-level category in whole cents.
pub fn summarise_quarter(csv: &str) -> Result<(String, Vec<IpeaParliamentarian>)> {
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .from_reader(csv.trim_start_matches('\u{feff}').as_bytes());
    let headers = reader.headers()?.clone();
    let column = |name: &str| {
        headers
            .iter()
            .position(|h| h == name)
            .ok_or_else(|| anyhow!("ipea: no {name} column"))
    };
    let (period, code, state, electorate, full, surname, first, category, amount) = (
        column("ReportingPeriod")?,
        column("OfficeCode")?,
        column("StateOrTerritory")?,
        column("Electorate")?,
        column("FullNameWithTitle")?,
        column("Surname")?,
        column("FirstName")?,
        column("HighLevelCategory")?,
        column("Amount")?,
    );
    let mut label = String::new();
    let mut people: IndexMap<String, (IpeaParliamentarian, IndexMap<String, i64>)> =
        IndexMap::new();
    for record in reader.records() {
        let record = record?;
        let get = |i: usize| record.get(i).unwrap_or("").trim().to_string();
        let office_code = get(code);
        if office_code.is_empty() {
            continue;
        }
        if label.is_empty() {
            label = get(period);
        }
        let cents = cents(&get(amount))?;
        let (_, categories) = people.entry(office_code.clone()).or_insert_with(|| {
            (
                IpeaParliamentarian {
                    office_code,
                    name: get(full),
                    first_name: get(first),
                    surname: get(surname),
                    electorate: Some(get(electorate)).filter(|e| !e.is_empty()),
                    state: get(state),
                    categories: Vec::new(),
                },
                IndexMap::new(),
            )
        });
        let name = match get(category) {
            c if c.is_empty() => "Other".to_string(),
            c => c,
        };
        *categories.entry(name).or_insert(0) += cents;
    }
    if people.is_empty() {
        bail!("ipea: the quarter's file has no transactions");
    }
    let parliamentarians = people
        .into_values()
        .map(|(mut p, categories)| {
            let mut lines: Vec<(String, i64)> = categories.into_iter().collect();
            lines.sort_by_key(|(_, cents)| std::cmp::Reverse(*cents));
            p.categories = lines;
            p
        })
        .collect();
    Ok((label, parliamentarians))
}

/// A dollar amount as IPEA writes it ("1234.5", "-10.00") in whole cents.
fn cents(amount: &str) -> Result<i64> {
    if amount.is_empty() {
        return Ok(0);
    }
    let value: f64 = amount
        .replace(',', "")
        .parse()
        .map_err(|_| anyhow!("ipea: unreadable amount {amount:?}"))?;
    Ok((value * 100.0).round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::LocalStore;
    use crate::test_http::{Response, TestServer};
    use std::path::PathBuf;

    fn new_store(name: &str) -> Store {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/ipea-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Store::Local(LocalStore::new(dir))
    }

    const HEADER: &str = "UniqueId,ReportingPeriodId,ReportingPeriod,OfficeCode,StateOrTerritory,Electorate,FullNameWithTitle,Party,Surname,FirstName,Role,HighLevelCategory,Amount";

    fn quarter(label: &str) -> String {
        [
            format!("\u{feff}{HEADER}"),
            format!("1,X,{label},PATA,VIC,SAMPLEFORD,Alex PATERSON MP,Example,PATERSON,Alex,Parliamentarian,Office Administration,100.10"),
            format!("2,X,{label},PATA,VIC,SAMPLEFORD,Alex PATERSON MP,Example,PATERSON,Alex,Parliamentarian,Office Administration,-0.10"),
            format!("3,X,{label},PATA,VIC,SAMPLEFORD,Alex PATERSON MP,Example,PATERSON,Alex,Parliamentarian,Travel Allowance,\"1,250.00\""),
            format!("4,X,{label},ROSM,TAS,,Senator Morgan ROSSI,Example,ROSSI,Morgan,Parliamentarian,,12.5"),
            format!("5,X,{label},,TAS,,,,,,,Office Administration,99"),
        ]
        .join("\n")
    }

    #[test]
    fn a_quarter_sums_each_member_by_category_in_cents() {
        let (label, people) = summarise_quarter(&quarter("Apr-Jun 2026")).expect("parses");
        assert_eq!(label, "Apr-Jun 2026");
        assert_eq!(people.len(), 2, "a row with no office code is nobody's");
        let alex = &people[0];
        assert_eq!(alex.office_code, "PATA");
        assert_eq!(alex.electorate.as_deref(), Some("SAMPLEFORD"));
        assert_eq!(
            alex.categories,
            vec![
                ("Travel Allowance".to_string(), 125_000),
                ("Office Administration".to_string(), 10_000)
            ],
            "largest first, credits netted"
        );
        assert_eq!(people[1].categories, vec![("Other".to_string(), 1250)]);
        assert!(people[1].electorate.is_none());
        assert!(summarise_quarter(HEADER).is_err(), "no transactions");
        assert!(summarise_quarter("A,B\n1,2").is_err(), "not an IPEA file");
    }

    #[tokio::test]
    async fn the_newest_quarters_are_fetched_once_and_old_ones_dropped() {
        let server = TestServer::start(|req| {
            if req.path.contains("/package_search") {
                let base = req
                    .header("host")
                    .map(|h| format!("http://{h}"))
                    .unwrap_or_default();
                let package = |q: &str, modified: &str| {
                    serde_json::json!({ "resources": [
                        { "name": format!("{q} expenses"), "url": format!("{base}/files/{q}_dataextract.csv"),
                          "last_modified": modified },
                        { "name": format!("{q} repayments"), "url": format!("{base}/files/{q}_dataextract_repayments.csv") }
                    ] })
                };
                return Response::json(
                    serde_json::json!({ "result": { "results": [
                        package("2025q02", "a"), package("2025q03", "a"), package("2025q04", "a"),
                        package("2026q01", "a"), package("2026q02", "a")
                    ] } })
                    .to_string(),
                );
            }
            if req.path.contains("_dataextract.csv") {
                return Response::text(quarter("Sample quarter"));
            }
            Response::status(404, "unexpected path")
        });
        let store = new_store("quarters");
        // A quarter that has rolled off the window is removed.
        store
            .put_json("canonical/ipea/2025Q01.json", &serde_json::json!({}))
            .await
            .unwrap();
        let endpoints = Endpoints::at(&server.base);
        sync_ipea(&store, &endpoints).await.expect("sync");
        assert_eq!(server.hits(), 1 + QUARTERS, "the search, then four files");
        let keys = store.list(PREFIX).await.unwrap();
        assert_eq!(keys.len(), QUARTERS);
        assert!(keys.iter().any(|k| k.ends_with("2026Q02.json")));
        assert!(!keys
            .iter()
            .any(|k| k.ends_with("2025Q02.json") || k.ends_with("2025Q01.json")));

        // Unchanged quarters are not downloaded again.
        sync_ipea(&store, &endpoints).await.expect("again");
        assert_eq!(server.hits(), 2 + QUARTERS);
    }

    #[tokio::test]
    async fn no_quarterly_files_is_a_failed_sync() {
        let server = TestServer::start(|_| Response::json(r#"{"result":{"results":[]}}"#));
        let store = new_store("none");
        let err = sync_ipea(&store, &Endpoints::at(&server.base))
            .await
            .expect_err("nothing to read");
        assert!(err.to_string().contains("no quarterly"), "got {err}");
    }

    #[test]
    fn amounts_read_to_the_cent() {
        assert_eq!(cents("10.00").unwrap(), 1000);
        assert_eq!(cents("-0.10").unwrap(), -10);
        assert_eq!(cents("44.555").unwrap(), 4456);
        assert_eq!(cents("").unwrap(), 0);
        assert!(cents("n/a").is_err());
    }
}
