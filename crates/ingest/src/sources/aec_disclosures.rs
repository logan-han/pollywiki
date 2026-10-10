//! Financial disclosure returns lodged with the AEC, from the Transparency
//! Register's bulk downloads: parties' and members' annual returns, the
//! donations the party returns itemise, and candidates' election returns.

use crate::endpoints::Endpoints;
use crate::http::fetch_bytes;
use crate::store::Store;
use anyhow::{anyhow, bail, Result};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::io::Read;

pub const KEY: &str = "canonical/disclosures/aec.json";
/// Financial years of annual returns kept, newest last.
const YEARS: i64 = 4;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Disclosures {
    pub party_returns: Vec<PartyReturnRow>,
    /// Gifts the party returns list as donations received.
    pub party_donations: Vec<DonationRow>,
    pub member_returns: Vec<MemberReturnRow>,
    pub candidate_returns: Vec<CandidateReturnRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartyReturnRow {
    pub year: String,
    pub name: String,
    /// The AEC's grouping of a party's branches, e.g. "Liberal".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub receipts: i64,
    pub payments: i64,
    pub debts: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DonationRow {
    pub year: String,
    pub recipient: String,
    pub donor: String,
    pub value: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberReturnRow {
    pub year: String,
    /// As lodged, titles and all: "Dr Monique Ryan MP".
    pub name: String,
    pub donations: i64,
    pub donors: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateReturnRow {
    /// The AEC's event name, e.g. "2025 Federal Election".
    pub event: String,
    /// "SURNAME, Given Names".
    pub name: String,
    pub party: String,
    pub electorate: String,
    pub state: String,
    pub nil: bool,
    pub gifts: i64,
    pub donors: i64,
    pub expenditure: i64,
}

pub async fn sync_aec_disclosures(store: &Store, endpoints: &Endpoints) -> Result<()> {
    let annual = fetch_bytes(
        &format!("{}/Download/AllAnnualData", endpoints.aec_transparency),
        &endpoints.opts(1500),
    )
    .await?;
    let elections = fetch_bytes(
        &format!("{}/Download/AllElectionsData", endpoints.aec_transparency),
        &endpoints.opts(1500),
    )
    .await?;
    let disclosures = Disclosures {
        party_returns: party_returns(&unzip(&annual, "Party Returns.csv")?)?,
        party_donations: party_donations(&unzip(&annual, "Detailed Receipts.csv")?)?,
        member_returns: member_returns(&unzip(&annual, "MemberOfParliamentReturns.csv")?)?,
        candidate_returns: candidate_returns(&unzip(
            &elections,
            "Senate Groups and Candidate Return Summary.csv",
        )?)?,
    };
    if disclosures.party_returns.is_empty() {
        bail!("aec-disclosures: no party returns in the download");
    }
    println!(
        "aec-disclosures: {} party returns, {} donations, {} member returns, {} candidate returns",
        disclosures.party_returns.len(),
        disclosures.party_donations.len(),
        disclosures.member_returns.len(),
        disclosures.candidate_returns.len()
    );
    store.put_json(KEY, &disclosures).await
}

/// One CSV from the register's zip, by name.
fn unzip(archive: &[u8], name: &str) -> Result<String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive))
        .map_err(|e| anyhow!("aec-disclosures: not a zip ({e})"))?;
    let mut file = zip
        .by_name(name)
        .map_err(|_| anyhow!("aec-disclosures: {name} missing from the download"))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes)
        .trim_start_matches('\u{feff}')
        .to_string())
}

type Row = IndexMap<String, String>;

fn rows(csv: &str) -> Result<Vec<Row>> {
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .from_reader(csv.as_bytes());
    let headers = reader.headers()?.clone();
    let mut out = Vec::new();
    for record in reader.records() {
        let record = record?;
        out.push(
            headers
                .iter()
                .enumerate()
                .map(|(i, h)| {
                    (
                        h.to_string(),
                        record.get(i).unwrap_or("").trim().to_string(),
                    )
                })
                .collect(),
        );
    }
    Ok(out)
}

fn text(row: &Row, key: &str) -> String {
    row.get(key).cloned().unwrap_or_default()
}

/// Whole dollars, as the register writes them.
fn dollars(row: &Row, key: &str) -> i64 {
    row.get(key)
        .and_then(|v| v.replace(',', "").parse::<f64>().ok())
        .map(|v| v.round() as i64)
        .unwrap_or(0)
}

/// "2024-25" and the older "1998-1999" both open on their first year.
fn start_year(financial_year: &str) -> Option<i64> {
    financial_year.get(..4)?.parse().ok()
}

/// The newest financial years in a file, `YEARS` of them.
fn recent(rows: &[Row]) -> impl Fn(&str) -> bool {
    let newest = rows
        .iter()
        .filter_map(|r| start_year(&text(r, "Financial Year")))
        .max()
        .unwrap_or(0);
    move |year: &str| start_year(year).is_some_and(|y| y > newest - YEARS)
}

fn party_returns(csv: &str) -> Result<Vec<PartyReturnRow>> {
    let rows = rows(csv)?;
    let keep = recent(&rows);
    Ok(rows
        .iter()
        .filter(|r| keep(&text(r, "Financial Year")))
        .map(|r| PartyReturnRow {
            year: text(r, "Financial Year"),
            name: text(r, "Name"),
            group: Some(text(r, "Party Group")).filter(|g| !g.is_empty()),
            receipts: dollars(r, "Total Receipts"),
            payments: dollars(r, "Total Payments"),
            debts: dollars(r, "Total Debts"),
        })
        .collect())
}

fn party_donations(csv: &str) -> Result<Vec<DonationRow>> {
    let rows = rows(csv)?;
    let keep = recent(&rows);
    Ok(rows
        .iter()
        .filter(|r| {
            text(r, "Return Type") == "Political Party Return"
                && text(r, "Receipt Type") == "Donation Received"
                && keep(&text(r, "Financial Year"))
        })
        .map(|r| DonationRow {
            year: text(r, "Financial Year"),
            recipient: text(r, "Recipient Name"),
            donor: text(r, "Received From"),
            value: dollars(r, "Value"),
        })
        .collect())
}

fn member_returns(csv: &str) -> Result<Vec<MemberReturnRow>> {
    Ok(rows(csv)?
        .iter()
        .map(|r| MemberReturnRow {
            year: text(r, "Financial Year"),
            name: text(r, "Name"),
            donations: dollars(r, "Total Donations Received"),
            donors: dollars(r, "Number of Donors"),
        })
        .collect())
}

/// Candidates' returns, one per candidate and event: an amended return
/// replaces the one it amends.
fn candidate_returns(csv: &str) -> Result<Vec<CandidateReturnRow>> {
    let mut latest: IndexMap<(String, String, String), (i64, CandidateReturnRow)> = IndexMap::new();
    for r in rows(csv)?
        .iter()
        .filter(|r| text(r, "Return Type (Candidate/Senate Group)") == "Candidate")
    {
        let row = CandidateReturnRow {
            event: text(r, "Event"),
            name: text(r, "Name"),
            party: text(r, "Party Name"),
            electorate: text(r, "Electorate Name"),
            state: text(r, "Electorate State"),
            nil: text(r, "Nil Return") == "Y",
            gifts: dollars(r, "Total Gift Value"),
            donors: dollars(r, "Number Of Donors"),
            expenditure: dollars(r, "Total Electoral Expenditure"),
        };
        let amendment = dollars(r, "Amendment No");
        let key = (row.event.clone(), row.name.clone(), row.electorate.clone());
        match latest.get(&key) {
            Some((held, _)) if *held >= amendment => {}
            _ => {
                latest.insert(key, (amendment, row));
            }
        }
    }
    Ok(latest.into_values().map(|(_, row)| row).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::LocalStore;
    use crate::test_http::{Response, TestServer};
    use std::io::Write;
    use std::path::PathBuf;

    fn new_store(name: &str) -> Store {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/aec-disclosures-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Store::Local(LocalStore::new(dir))
    }

    fn zip_of(files: &[(&str, &str)]) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut out);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (name, body) in files {
                zip.start_file(*name, options).expect("start");
                zip.write_all(body.as_bytes()).expect("write");
            }
            zip.finish().expect("finish");
        }
        out.into_inner()
    }

    fn annual() -> Vec<u8> {
        zip_of(&[
            (
                "Party Returns.csv",
                "\u{feff}\"Financial Year\",\"Name\",\"Party Group\",\"Total Receipts\",\"Total Payments\",\"Total Debts\",\"Total Discretionary Benefits\"\n\
                 \"2024-25\",\"Example Party (Victorian Branch)\",\"Example Party\",\"1500\",\"1200\",\"10\",\"0\"\n\
                 \"2024-25\",\"Small Party\",\"\",\"300\",\"200\",\"0\",\"0\"\n\
                 \"2020-21\",\"Example Party (Victorian Branch)\",\"Example Party\",\"900\",\"800\",\"0\",\"0\"\n\
                 \"1998-1999\",\"Old Party\",\"\",\"1\",\"1\",\"0\",\"0\"",
            ),
            (
                "Detailed Receipts.csv",
                "\"Financial Year\",\"Return Type\",\"Recipient Name\",\"Received From\",\"Receipt Type\",\"Value\"\n\
                 \"2024-25\",\"Political Party Return\",\"Example Party (Victorian Branch)\",\"Acme Pty Ltd\",\"Donation Received\",\"25000\"\n\
                 \"2024-25\",\"Political Party Return\",\"Example Party (Victorian Branch)\",\"Bank\",\"Other Receipt\",\"90000\"\n\
                 \"2024-25\",\"Associated Entity Return\",\"Club\",\"Acme Pty Ltd\",\"Donation Received\",\"5000\"\n\
                 \"2019-20\",\"Political Party Return\",\"Example Party (Victorian Branch)\",\"Old Donor\",\"Donation Received\",\"1\"",
            ),
            (
                "MemberOfParliamentReturns.csv",
                "\"Financial Year\",\"Return Type\",\"Name\",\"Total Donations Received\",\"Number of Donors\"\n\
                 \"2024-25\",\"Member of House of Representatives Return\",\"Dr Alex Paterson MP\",\"4484\",\"6\"",
            ),
        ])
    }

    fn elections() -> Vec<u8> {
        zip_of(&[(
            "Senate Groups and Candidate Return Summary.csv",
            "\"Event\",\"Return Type (Candidate/Senate Group)\",\"Name\",\"Party ID\",\"Party Name\",\"Electorate Name\",\"Electorate State\",\"Nil Return\",\"Amendment No\",\"Total Gift Value\",\"Number Of Donors\",\"Total Electoral Expenditure\",\"Discretionary Benefits Received\"\n\
             \"2025 Federal Election\",\"Candidate\",\"PATERSON, Alex\",\"1\",\"Example Party\",\"Sampleford\",\"VIC\",\"N\",\"0\",\"1000\",\"2\",\"5000\",\"0\"\n\
             \"2025 Federal Election\",\"Candidate\",\"PATERSON, Alex\",\"1\",\"Example Party\",\"Sampleford\",\"VIC\",\"N\",\"1\",\"1200\",\"3\",\"5100\",\"0\"\n\
             \"2025 Federal Election\",\"Candidate\",\"DOE, Casey\",\"2\",\"Independent\",\"Sampleford\",\"VIC\",\"Y\",\"0\",\"0\",\"0\",\"0\",\"0\"\n\
             \"2025 Federal Election\",\"Senate Group\",\"Example Group\",\"1\",\"Example Party\",\"\",\"VIC\",\"N\",\"0\",\"1\",\"1\",\"1\",\"0\"",
        )])
    }

    #[tokio::test]
    async fn the_register_downloads_reduce_to_recent_returns_and_party_donations() {
        let server = TestServer::start(|req| match req.path.as_str() {
            "/aec-transparency/Download/AllAnnualData" => {
                Response::bytes(annual(), "application/zip")
            }
            "/aec-transparency/Download/AllElectionsData" => {
                Response::bytes(elections(), "application/zip")
            }
            _ => Response::status(404, "unexpected path"),
        });
        let store = new_store("register");
        sync_aec_disclosures(&store, &Endpoints::at(&server.base))
            .await
            .expect("sync");
        let d: Disclosures = store.get_json(KEY).await.unwrap().expect("stored");

        // Four financial years back from the newest; the 1990s are gone.
        let years: Vec<&str> = d.party_returns.iter().map(|r| r.year.as_str()).collect();
        assert_eq!(years, ["2024-25", "2024-25"]);
        assert_eq!(d.party_returns[0].group.as_deref(), Some("Example Party"));
        assert!(
            d.party_returns[1].group.is_none(),
            "an ungrouped party has no group"
        );
        assert_eq!(d.party_returns[0].receipts, 1500);

        // Only donations on party returns, and only recent ones.
        assert_eq!(d.party_donations.len(), 1);
        assert_eq!(d.party_donations[0].donor, "Acme Pty Ltd");
        assert_eq!(d.party_donations[0].value, 25000);

        assert_eq!(d.member_returns[0].name, "Dr Alex Paterson MP");
        assert_eq!(
            (d.member_returns[0].donations, d.member_returns[0].donors),
            (4484, 6)
        );

        // The amended return stands in for the first; groups are not candidates.
        assert_eq!(d.candidate_returns.len(), 2);
        let alex = &d.candidate_returns[0];
        assert_eq!((alex.gifts, alex.donors, alex.expenditure), (1200, 3, 5100));
        assert!(d.candidate_returns[1].nil);
    }

    #[tokio::test]
    async fn a_download_that_is_not_a_zip_fails_the_sync() {
        let server = TestServer::start(|_| Response::html("<html>maintenance</html>"));
        let store = new_store("not-zip");
        let err = sync_aec_disclosures(&store, &Endpoints::at(&server.base))
            .await
            .expect_err("unreadable download");
        assert!(err.to_string().contains("not a zip"), "got {err}");
        assert!(store.get_raw(KEY).await.unwrap().is_none());
    }

    #[test]
    fn a_zip_without_the_expected_file_names_what_is_missing() {
        let err = unzip(&zip_of(&[("Other.csv", "a")]), "Party Returns.csv").expect_err("missing");
        assert!(
            err.to_string().contains("Party Returns.csv missing"),
            "got {err}"
        );
    }
}
