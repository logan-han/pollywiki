use crate::endpoints::Endpoints;
use crate::http::fetch_text;
use crate::store::Store;
use anyhow::{anyhow, Result};
use indexmap::IndexMap;
use pollywiki_schema::{slugify, CandidateResult, Electorate, ElectorateResult, JsNum, StateCode};
use regex::Regex;
use std::sync::LazyLock;

fn event_name(event_id: &str) -> Option<&'static str> {
    match event_id {
        "31633" => Some("2026 Farrer by-election"),
        "31496" => Some("2025 federal election"),
        "29807" => Some("2024 Cook by-election"),
        "29778" => Some("2024 Dunkley by-election"),
        "29422" => Some("2023 Fadden by-election"),
        "28791" => Some("2023 Aston by-election"),
        "27966" => Some("2022 federal election"),
        "24310" => Some("2019 federal election"),
        _ => None,
    }
}

// General elections publish per-candidate totals directly; by-elections only
// publish polling-place level files, which are aggregated per candidate here.
const BY_ELECTIONS: [&str; 7] = [
    "31633", "29807", "29778", "29422", "28791", "25881", "25820",
];

pub type AecRow = IndexMap<String, String>;

struct EventRows {
    first_prefs: Vec<AecRow>,
    tcp: Vec<AecRow>,
}

pub async fn sync_aec(store: &Store, event_id: &str, endpoints: &Endpoints) -> Result<()> {
    let rows = if BY_ELECTIONS.contains(&event_id) {
        fetch_by_election(store, event_id, endpoints).await?
    } else {
        fetch_general(store, event_id, endpoints).await?
    };

    let event_name = event_name(event_id)
        .map(str::to_string)
        .unwrap_or_else(|| format!("AEC event {event_id}"));
    // Electorate entities come only from the most recent general election;
    // historical events contribute results without redefining the seat map.
    let defines_electorates = event_id == "31496";
    let mut by_electorate: IndexMap<String, (String, String)> = IndexMap::new();
    for row in &rows.first_prefs {
        let name = row.get("DivisionNm").cloned().unwrap_or_default();
        let state = row.get("StateAb").cloned().unwrap_or_default();
        by_electorate.insert(name.clone(), (name, state));
    }

    for (name, state) in by_electorate.values() {
        let electorate_slug = slugify(name);
        let state_code =
            StateCode::parse(state).ok_or_else(|| anyhow!("invalid state code: {state:?}"))?;

        if defines_electorates {
            let electorate = Electorate {
                slug: electorate_slug.clone(),
                name: name.clone(),
                state: state_code,
                member_slug: None,
                profile: None,
                enrolment: None,
            };
            store
                .put_json(
                    &format!("canonical/electorates/{electorate_slug}.json"),
                    &electorate,
                )
                .await?;
        }

        let result = ElectorateResult {
            event_id: event_id.to_string(),
            event_name: event_name.clone(),
            electorate_slug: electorate_slug.clone(),
            electorate_name: name.clone(),
            state: state_code,
            first_prefs: to_candidates(&rows.first_prefs, name),
            tcp: to_candidates(&rows.tcp, name),
        };
        store
            .put_json(
                &format!("canonical/elections/{event_id}/{electorate_slug}.json"),
                &result,
            )
            .await?;
    }
    Ok(())
}

async fn fetch_general(store: &Store, event_id: &str, endpoints: &Endpoints) -> Result<EventRows> {
    let files = [
        ("HouseFirstPrefsByCandidateByVoteTypeDownload", true),
        ("HouseTcpByCandidateByVoteTypeDownload", false),
    ];
    let mut first_prefs = Vec::new();
    let mut tcp = Vec::new();
    for (file, is_first_prefs) in files {
        let rows = fetch_csv(
            store,
            event_id,
            &format!(
                "{}/{event_id}/Website/Downloads/{file}-{event_id}.csv",
                endpoints.aec_results
            ),
            file,
            endpoints,
        )
        .await?;
        if is_first_prefs {
            first_prefs = rows;
        } else {
            tcp = rows;
        }
    }
    Ok(EventRows { first_prefs, tcp })
}

async fn fetch_by_election(
    store: &Store,
    event_id: &str,
    endpoints: &Endpoints,
) -> Result<EventRows> {
    let base = format!("{}/{event_id}/Website/Downloads", endpoints.aec_results);
    let candidates = fetch_csv(
        store,
        event_id,
        &format!("{base}/HouseCandidatesDownload-{event_id}.csv"),
        "HouseCandidatesDownload",
        endpoints,
    )
    .await?;
    let state = candidates
        .first()
        .and_then(|r| r.get("StateAb"))
        .cloned()
        .unwrap_or_default();
    let first_prefs_pp = fetch_csv(
        store,
        event_id,
        &format!("{base}/HouseStateFirstPrefsByPollingPlaceDownload-{event_id}-{state}.csv"),
        "HouseStateFirstPrefsByPollingPlaceDownload",
        endpoints,
    )
    .await?;
    let tcp_pp = fetch_csv(
        store,
        event_id,
        &format!("{base}/HouseTcpByCandidateByPollingPlaceDownload-{event_id}.csv"),
        "HouseTcpByCandidateByPollingPlaceDownload",
        endpoints,
    )
    .await?;
    Ok(EventRows {
        first_prefs: aggregate_polling_places(&first_prefs_pp),
        tcp: aggregate_polling_places(&tcp_pp),
    })
}

/// Sums polling-place rows into per-candidate totals shaped like the
/// by-vote-type files, so downstream parsing is identical. Per-booth swings
/// do not aggregate meaningfully, so swing is left blank.
fn aggregate_polling_places(rows: &[AecRow]) -> Vec<AecRow> {
    let mut by_candidate: IndexMap<String, AecRow> = IndexMap::new();
    for row in rows {
        let id = row.get("CandidateID").cloned().unwrap_or_default();
        let votes = js_number(row.get("OrdinaryVotes").map(String::as_str).unwrap_or("0"));
        if let Some(existing) = by_candidate.get_mut(&id) {
            let total = js_number(existing.get("TotalVotes").map(String::as_str).unwrap_or(""));
            existing.insert("TotalVotes".to_string(), js_number_to_string(total + votes));
        } else {
            let mut fresh = row.clone();
            fresh.insert("TotalVotes".to_string(), js_number_to_string(votes));
            fresh.insert("Swing".to_string(), String::new());
            by_candidate.insert(id, fresh);
        }
    }
    by_candidate.into_values().collect()
}

async fn fetch_csv(
    store: &Store,
    event_id: &str,
    url: &str,
    file: &str,
    endpoints: &Endpoints,
) -> Result<Vec<AecRow>> {
    let csv = fetch_text(url, &endpoints.opts(1500)).await?;
    store
        .put_raw(&format!("raw/aec/{event_id}/{file}.csv"), csv.as_bytes())
        .await?;
    // AEC files carry a metadata title line above the real header.
    let body = match csv.find('\n') {
        Some(at) => &csv[at + 1..],
        None => "",
    };
    parse_csv(body)
}

pub fn parse_csv(body: &str) -> Result<Vec<AecRow>> {
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .from_reader(body.as_bytes());
    let headers = reader.headers()?.clone();
    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record?;
        if record.iter().all(str::is_empty) {
            continue;
        }
        let mut row = AecRow::new();
        for (i, header) in headers.iter().enumerate() {
            row.insert(header.to_string(), record.get(i).unwrap_or("").to_string());
        }
        rows.push(row);
    }
    Ok(rows)
}

/// Number(...) semantics for the strings the AEC files contain.
fn js_number(value: &str) -> f64 {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return 0.0;
    }
    trimmed.parse::<f64>().unwrap_or(f64::NAN)
}

fn js_number_to_string(value: f64) -> String {
    if value.fract() == 0.0 && value.is_finite() {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

pub fn to_candidates(rows: &[AecRow], electorate_name: &str) -> Vec<CandidateResult> {
    let mine: Vec<&AecRow> = rows
        .iter()
        .filter(|r| r.get("DivisionNm").map(String::as_str) == Some(electorate_name))
        .collect();
    let total: f64 = mine
        .iter()
        .map(|r| js_number(r.get("TotalVotes").map(String::as_str).unwrap_or("0")))
        .sum();
    let mut candidates: Vec<CandidateResult> = mine
        .iter()
        .map(|r| {
            let get = |key: &str| r.get(key).map(String::as_str).unwrap_or("");
            let votes = js_number(if r.contains_key("TotalVotes") {
                get("TotalVotes")
            } else {
                "0"
            });
            let party = get("PartyNm");
            let swing = r.get("Swing").map(String::as_str);
            CandidateResult {
                name: title_case(format!("{} {}", get("GivenNm"), get("Surname")).trim()),
                party: if party.is_empty() {
                    "Independent".to_string()
                } else {
                    party.to_string()
                },
                party_code: match get("PartyAb") {
                    "" => None,
                    code => Some(code.to_string()),
                },
                votes: votes as i64,
                pct: JsNum(if total > 0.0 {
                    round2(votes / total * 100.0)
                } else {
                    0.0
                }),
                swing: match swing {
                    None | Some("") => None,
                    Some(s) => Some(JsNum(js_number(s))),
                },
                elected: get("Elected") == "Y",
            }
        })
        .collect();
    candidates.sort_by_key(|c| std::cmp::Reverse(c.votes));
    candidates
}

pub fn title_case(name: &str) -> String {
    static FIRST_LETTERS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(^|[\s\-'])([a-z])").unwrap());
    static MC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bMc([a-z])").unwrap());
    let lowered = name.to_lowercase();
    let cased = FIRST_LETTERS.replace_all(&lowered, |caps: &regex::Captures| {
        format!("{}{}", &caps[1], caps[2].to_uppercase())
    });
    MC.replace_all(&cased, |caps: &regex::Captures| {
        format!("Mc{}", caps[1].to_uppercase())
    })
    .into_owned()
}

fn round2(n: f64) -> f64 {
    // Math.round rounds half towards +Infinity; inputs here are >= 0.
    (n * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::LocalStore;
    use crate::test_http::{Response, TestServer};
    use std::path::PathBuf;

    fn new_store(name: &str) -> Store {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/aec-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Store::Local(LocalStore::new(dir))
    }

    /// AEC downloads carry a metadata title line above the real header.
    fn csv(header: &str, rows: &[&str]) -> String {
        let mut out = String::from("Information for this file is current as at some date\n");
        out.push_str(header);
        for row in rows {
            out.push('\n');
            out.push_str(row);
        }
        out
    }

    fn row(pairs: &[(&str, &str)]) -> AecRow {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn known_events_are_named_and_unknown_ones_are_not_invented() {
        assert_eq!(event_name("31496"), Some("2025 federal election"));
        assert_eq!(event_name("29807"), Some("2024 Cook by-election"));
        assert_eq!(event_name("99999"), None);
        // Every by-election id in the aggregation list is also a named event.
        for id in BY_ELECTIONS {
            if let Some(name) = event_name(id) {
                assert!(name.contains("by-election"), "{id} is named {name}");
            }
        }
    }

    #[test]
    fn csv_parsing_skips_blank_rows_and_tolerates_short_ones() {
        let rows =
            parse_csv("DivisionNm,CandidateID,OrdinaryVotes\nWentworth,1,100\n\nWentworth,2\n")
                .expect("csv parses");
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].get("OrdinaryVotes").map(String::as_str),
            Some("100")
        );
        // A short record fills the missing column rather than failing the file.
        assert_eq!(rows[1].get("OrdinaryVotes").map(String::as_str), Some(""));
    }

    #[test]
    fn by_election_polling_places_sum_per_candidate() {
        let rows = vec![
            row(&[
                ("CandidateID", "1"),
                ("OrdinaryVotes", "100"),
                ("Surname", "One"),
            ]),
            row(&[
                ("CandidateID", "2"),
                ("OrdinaryVotes", "40"),
                ("Surname", "Two"),
            ]),
            row(&[
                ("CandidateID", "1"),
                ("OrdinaryVotes", "55"),
                ("Surname", "One"),
            ]),
            row(&[
                ("CandidateID", "1"),
                ("OrdinaryVotes", ""),
                ("Surname", "One"),
            ]),
        ];
        let totals = aggregate_polling_places(&rows);
        assert_eq!(totals.len(), 2, "one row per candidate");
        assert_eq!(totals[0].get("TotalVotes").map(String::as_str), Some("155"));
        assert_eq!(totals[1].get("TotalVotes").map(String::as_str), Some("40"));
        // By-elections publish no swing, so the column is blanked, not guessed.
        assert_eq!(totals[0].get("Swing").map(String::as_str), Some(""));
    }

    #[test]
    fn number_parsing_follows_javascript_for_the_strings_aec_files_use() {
        assert_eq!(js_number("1234"), 1234.0);
        assert_eq!(js_number("  12.5 "), 12.5);
        assert_eq!(js_number(""), 0.0);
        assert!(js_number("not a number").is_nan());
        assert_eq!(js_number_to_string(155.0), "155");
        assert_eq!(js_number_to_string(12.5), "12.5");
    }

    #[test]
    fn percentages_round_to_two_places() {
        assert_eq!(round2(33.333333), 33.33);
        assert_eq!(round2(33.335), 33.34);
        assert_eq!(round2(50.0), 50.0);
    }

    #[tokio::test]
    async fn a_general_election_defines_the_seats_and_stores_both_result_tables() {
        let server = TestServer::start(|req| {
            if req
                .path
                .contains("HouseFirstPrefsByCandidateByVoteTypeDownload")
            {
                return Response::text(csv(
                    "StateAb,DivisionNm,CandidateID,Surname,GivenNm,PartyNm,PartyAb,TotalVotes,Swing",
                    &[
                        "VIC,Sampleford,101,PATERSON,ALEXANDRA,Example Party,EX,45000,2.5",
                        "VIC,Sampleford,102,NGUYEN,JORDAN,Placeholder Alliance,PA,30000,-1.5",
                    ],
                ));
            }
            if req.path.contains("HouseTcpByCandidateByVoteTypeDownload") {
                return Response::text(csv(
                    "StateAb,DivisionNm,CandidateID,Surname,GivenNm,PartyNm,PartyAb,TotalVotes,Swing",
                    &["VIC,Sampleford,101,PATERSON,ALEXANDRA,Example Party,EX,52000,1.8"],
                ));
            }
            Response::status(404, "unexpected file")
        });
        let store = new_store("general");

        sync_aec(&store, "31496", &Endpoints::at(&server.base))
            .await
            .expect("sync");

        // The current general election is the one that defines the seat map.
        let electorate: Electorate = store
            .get_json("canonical/electorates/sampleford.json")
            .await
            .unwrap()
            .expect("electorate defined");
        assert_eq!(electorate.name, "Sampleford");
        assert_eq!(electorate.state.as_str(), "VIC");

        let result: pollywiki_schema::ElectorateResult = store
            .get_json("canonical/elections/31496/sampleford.json")
            .await
            .unwrap()
            .expect("result stored");
        assert_eq!(result.event_name, "2025 federal election");
        assert_eq!(result.first_prefs.len(), 2);
        assert_eq!(result.first_prefs[0].name, "Alexandra Paterson");
        assert_eq!(result.first_prefs[0].votes, 45000);
        assert_eq!(result.tcp.len(), 1);
        assert_eq!(result.tcp[0].votes, 52000);

        // The raw downloads are kept alongside the parsed records.
        assert!(store
            .get_raw("raw/aec/31496/HouseFirstPrefsByCandidateByVoteTypeDownload.csv")
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn a_historical_event_adds_results_without_redefining_the_seat_map() {
        let server = TestServer::start(|_| {
            Response::text(csv(
                "StateAb,DivisionNm,CandidateID,Surname,GivenNm,PartyNm,PartyAb,TotalVotes,Swing",
                &["VIC,Sampleford,101,PATERSON,ALEXANDRA,Example Party,EX,40000,0"],
            ))
        });
        let store = new_store("historical");

        sync_aec(&store, "27966", &Endpoints::at(&server.base))
            .await
            .expect("sync");

        assert!(
            store
                .get_json::<Electorate>("canonical/electorates/sampleford.json")
                .await
                .unwrap()
                .is_none(),
            "only the current general election defines seats"
        );
        assert!(store
            .get_json::<pollywiki_schema::ElectorateResult>(
                "canonical/elections/27966/sampleford.json"
            )
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn a_by_election_is_aggregated_from_its_polling_place_files() {
        let server = TestServer::start(|req| {
            if req.path.contains("HouseCandidatesDownload") {
                return Response::text(csv(
                    "StateAb,DivisionNm,CandidateID,Surname,GivenNm",
                    &["NSW,Farrer,201,FARLEY,DAVID"],
                ));
            }
            if req
                .path
                .contains("HouseStateFirstPrefsByPollingPlaceDownload")
            {
                // By-elections publish per-booth rows only; the same candidate
                // appears once per polling place and must be summed.
                assert!(
                    req.path.contains("-NSW.csv"),
                    "state comes from the candidate file"
                );
                return Response::text(csv(
                    "StateAb,DivisionNm,CandidateID,Surname,GivenNm,PartyNm,PartyAb,OrdinaryVotes,Swing",
                    &[
                        "NSW,Farrer,201,FARLEY,DAVID,Example Party,EX,1200,3.1",
                        "NSW,Farrer,201,FARLEY,DAVID,Example Party,EX,800,2.0",
                        "NSW,Farrer,202,OTHER,SAM,Placeholder Alliance,PA,500,-1.0",
                    ],
                ));
            }
            if req
                .path
                .contains("HouseTcpByCandidateByPollingPlaceDownload")
            {
                return Response::text(csv(
                    "StateAb,DivisionNm,CandidateID,Surname,GivenNm,PartyNm,PartyAb,OrdinaryVotes,Swing",
                    &["NSW,Farrer,201,FARLEY,DAVID,Example Party,EX,1500,1.0"],
                ));
            }
            Response::status(404, "unexpected file")
        });
        let store = new_store("byelection");

        sync_aec(&store, "31633", &Endpoints::at(&server.base))
            .await
            .expect("sync");

        let result: pollywiki_schema::ElectorateResult = store
            .get_json("canonical/elections/31633/farrer.json")
            .await
            .unwrap()
            .expect("result stored");
        assert_eq!(result.event_name, "2026 Farrer by-election");
        let farley = result
            .first_prefs
            .iter()
            .find(|c| c.name == "David Farley")
            .expect("candidate aggregated");
        assert_eq!(farley.votes, 2000, "booth rows are summed");
        // Per-booth swings do not aggregate, so the field is left empty.
        assert!(farley.swing.is_none(), "swing is not summed across booths");
    }

    #[tokio::test]
    async fn an_unknown_state_code_stops_the_sync_rather_than_writing_a_bad_seat() {
        let server = TestServer::start(|_| {
            Response::text(csv(
                "StateAb,DivisionNm,CandidateID,Surname,GivenNm,PartyNm,PartyAb,TotalVotes,Swing",
                &["ZZ,Nowhere,101,SOMEONE,SAM,Example Party,EX,10,0"],
            ))
        });
        let store = new_store("badstate");
        let err = sync_aec(&store, "31496", &Endpoints::at(&server.base))
            .await
            .expect_err("an unknown state is fatal");
        assert!(err.to_string().contains("invalid state code"), "got {err}");
    }

    #[tokio::test]
    async fn a_missing_download_fails_the_sync() {
        let server = TestServer::start(|_| Response::status(404, "no such event"));
        let store = new_store("missing");
        let err = sync_aec(&store, "31496", &Endpoints::at(&server.base))
            .await
            .expect_err("a missing file is a failed sync");
        assert!(err.to_string().contains("404"), "got {err}");
    }
}
