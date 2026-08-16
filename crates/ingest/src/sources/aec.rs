use crate::http::{fetch_text, FetchOpts};
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

pub async fn sync_aec(store: &Store, event_id: &str) -> Result<()> {
    let rows = if BY_ELECTIONS.contains(&event_id) {
        fetch_by_election(store, event_id).await?
    } else {
        fetch_general(store, event_id).await?
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

async fn fetch_general(store: &Store, event_id: &str) -> Result<EventRows> {
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
                "https://results.aec.gov.au/{event_id}/Website/Downloads/{file}-{event_id}.csv"
            ),
            file,
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

async fn fetch_by_election(store: &Store, event_id: &str) -> Result<EventRows> {
    let base = format!("https://results.aec.gov.au/{event_id}/Website/Downloads");
    let candidates = fetch_csv(
        store,
        event_id,
        &format!("{base}/HouseCandidatesDownload-{event_id}.csv"),
        "HouseCandidatesDownload",
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
    )
    .await?;
    let tcp_pp = fetch_csv(
        store,
        event_id,
        &format!("{base}/HouseTcpByCandidateByPollingPlaceDownload-{event_id}.csv"),
        "HouseTcpByCandidateByPollingPlaceDownload",
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

async fn fetch_csv(store: &Store, event_id: &str, url: &str, file: &str) -> Result<Vec<AecRow>> {
    let csv = fetch_text(url, &FetchOpts::min_interval(1500)).await?;
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
