//! State and territory parliaments, from the member lists each one publishes.
//! Every parliament words its list its own way, so each has a reader of its
//! own, and they all produce the same roll: who sits in which chamber, for
//! which party and which seat, with any offices the list gives them. Party
//! names stay as the parliament wrote them; derive matches them to the
//! reference data.

use crate::endpoints::Endpoints;
use crate::store::Store;
use anyhow::{anyhow, bail, Result};
use indexmap::IndexMap;
use pollywiki_schema::{ChamberKind, StateCode};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

mod act;
mod drop;
mod nsw;
mod qld;
mod sa;
mod tas;
mod vic;

pub const PREFIX: &str = "canonical/states/";

/// A state or territory parliament's fixed facts: its name, its chambers and
/// their sizes in law.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateReferenceEntry {
    pub name: String,
    pub url: String,
    pub leader_title: String,
    pub chambers: Vec<StateReferenceChamber>,
    /// Why this parliament's lists are not read, when they cannot be: its
    /// sync is skipped and its place on the site says so.
    #[serde(default)]
    pub unavailable: Option<String>,
    /// Where the lists are downloaded by hand instead, for a parliament
    /// whose site turns automated readers away. Read whenever a download has
    /// been filed; until then the parliament stays unavailable.
    #[serde(default)]
    pub drop: Option<DropConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DropConfig {
    /// How the lists arrive, in the words the parliament's page shows.
    pub note: String,
    /// The official page the files are downloaded from.
    pub source: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateReferenceChamber {
    pub slug: String,
    pub name: String,
    pub kind: ChamberKind,
    pub seats: i64,
    pub seat_label: Option<String>,
}

/// Each parliament's fixed facts by state code, from
/// data/reference/state-parliaments.json; empty, and said so, without it.
pub fn reference() -> IndexMap<String, StateReferenceEntry> {
    let path = crate::reference_path("state-parliaments.json");
    match std::fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|err| {
            eprintln!("states: data/reference/state-parliaments.json unreadable: {err}");
            IndexMap::new()
        }),
        Err(_) => {
            eprintln!("states: data/reference/state-parliaments.json not found");
            IndexMap::new()
        }
    }
}

/// One parliament's members as last read.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateRoll {
    pub state: StateCode,
    pub listed_at: String,
    /// The lower house first, as the reference data orders them.
    pub chambers: Vec<RollChamber>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RollChamber {
    /// The chamber's slug in data/reference/state-parliaments.json.
    pub slug: String,
    /// The page or feed the members were read from.
    pub source: String,
    pub members: Vec<RollMember>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RollMember {
    pub name: String,
    pub party: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seat: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    #[serde(default)]
    pub offices: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

pub fn roll_key(state: StateCode) -> String {
    format!("{PREFIX}{}.json", state.as_str().to_lowercase())
}

/// A member's name without the styles some lists write around it: "The
/// Honourable Eric Abetz MP" is Eric Abetz. Titles go too, as the other
/// parliaments' lists and the federal register leave them out.
pub fn plain_name(styled: &str) -> String {
    const LEADING: [&str; 9] = [
        "The Honourable ",
        "The Hon. ",
        "The Hon ",
        "Hon. ",
        "Hon ",
        "Dr ",
        "Mr ",
        "Mrs ",
        "Ms ",
    ];
    const TRAILING: [&str; 10] = [
        " MP", " MLC", " MLA", " AC", " AO", " AM", " OAM", " CSC", " PSM", " APM",
    ];
    let mut name = styled.split_whitespace().collect::<Vec<_>>().join(" ");
    while let Some(rest) = LEADING.iter().find_map(|p| name.strip_prefix(p)) {
        name = rest.to_string();
    }
    while let Some(rest) = TRAILING.iter().find_map(|p| name.strip_suffix(p)) {
        name = rest.to_string();
    }
    name
}

/// Offices a list runs together in one cell, as Queensland's and New South
/// Wales's do:
/// "Premier and Minister for Veterans", "Deputy Premier, Minister for State
/// Development, Infrastructure and Planning and Minister for Industrial
/// Relations". It is cut wherever ", " or " and " is followed by the start of
/// another office, so a single portfolio with commas of its own stays whole.
pub fn split_offices(portfolio: &str) -> Vec<String> {
    static BREAK: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?:, | and )(?:Minister|Shadow|Assistant|Leader|Deputy|Manager|Parliamentary|Government Whip|Opposition Whip|Chair|Speaker|Treasurer|Attorney-General|Premier)\b",
        )
        .unwrap()
    });
    let mut out = Vec::new();
    let mut start = 0;
    for found in BREAK.find_iter(portfolio) {
        out.push(portfolio[start..found.start()].trim().to_string());
        // The office begins after the separator, not at it.
        let separator = if found.as_str().starts_with(", ") {
            2
        } else {
            5
        };
        start = found.start() + separator;
    }
    out.push(portfolio[start..].trim().to_string());
    out.retain(|o| !o.is_empty());
    out
}

/// The name a parliament's sync is recorded under, and can be run by.
pub fn source_name(state: StateCode) -> String {
    format!("{}-parliament", state.as_str().to_lowercase())
}

/// Reads one parliament's member lists and stores them as its roll. A
/// reading that does not look like its chambers leaves the last good roll
/// where it is.
pub async fn sync_state(
    store: &Store,
    state: StateCode,
    reference: Option<&StateReferenceEntry>,
    endpoints: &Endpoints,
) -> Result<()> {
    let reference =
        reference.ok_or_else(|| anyhow!("no entry for {state} in state-parliaments.json"))?;
    let filed = match &reference.drop {
        Some(_) => {
            drop::newest(&crate::reference_path("state-drops").join(state.as_str().to_lowercase()))?
        }
        None => None,
    };
    let roll = match (&reference.drop, filed) {
        // Downloaded by hand: listed the day it was downloaded.
        (Some(config), Some(filed)) => {
            let age = drop::age_days(&filed.date, chrono::Utc::now().date_naive());
            if age.is_some_and(|days| days > drop::STALE_AFTER_DAYS) {
                eprintln!(
                    "{}: the list downloaded on {} is {} days old; file a fresh one",
                    source_name(state),
                    filed.date,
                    age.unwrap_or_default()
                );
            }
            StateRoll {
                state,
                listed_at: format!("{}T00:00:00.000Z", filed.date),
                chambers: drop::read(state, &filed, &config.source)?,
            }
        }
        _ => {
            if let Some(reason) = &reference.unavailable {
                println!("{}: skipped ({reason})", source_name(state));
                return Ok(());
            }
            StateRoll {
                state,
                listed_at: crate::now_iso(),
                chambers: read(store, state, endpoints).await?,
            }
        }
    };
    check_roll(&roll, reference)?;
    store.put_json(&roll_key(state), &roll).await?;
    println!(
        "{}: {}",
        source_name(state),
        roll.chambers
            .iter()
            .map(|c| format!("{} {}", c.members.len(), c.slug))
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(())
}

async fn read(store: &Store, state: StateCode, endpoints: &Endpoints) -> Result<Vec<RollChamber>> {
    match state {
        StateCode::VIC => vic::read(store, endpoints).await,
        StateCode::QLD => qld::read(store, endpoints).await,
        StateCode::SA => sa::read(store, endpoints).await,
        StateCode::TAS => tas::read(store, endpoints).await,
        StateCode::ACT => act::read(store, endpoints).await,
        _ => bail!("no reader for {state} yet"),
    }
}

/// A reading far short of its chamber, or longer than it, is a list that
/// changed shape rather than a parliament that did, so it is refused. A few
/// vacancies pass; a chamber three-quarters empty does not.
pub fn check_roll(roll: &StateRoll, reference: &StateReferenceEntry) -> Result<()> {
    for chamber in &reference.chambers {
        let Some(listed) = roll.chambers.iter().find(|c| c.slug == chamber.slug) else {
            bail!("no member list for the {}", chamber.name);
        };
        let listed_count = listed.members.len() as i64;
        if listed_count * 4 < chamber.seats * 3 || listed_count > chamber.seats {
            bail!(
                "{listed_count} members listed for the {}'s {} seats",
                chamber.name,
                chamber.seats
            );
        }
        if let Some(m) = listed
            .members
            .iter()
            .find(|m| m.name.trim().is_empty() || m.party.trim().is_empty())
        {
            bail!("a {} member with no name or party: {m:?}", chamber.name);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference_entry() -> StateReferenceEntry {
        serde_json::from_str(
            r#"{"name":"Parliament of Example","url":"https://example.org","leaderTitle":"Premier",
                "chambers":[{"slug":"legislative-assembly","name":"Legislative Assembly",
                             "kind":"lower","seats":4}]}"#,
        )
        .expect("reference fixture")
    }

    fn roll_of(members: usize) -> StateRoll {
        StateRoll {
            state: StateCode::NSW,
            listed_at: "2026-10-30T01:00:00.000Z".to_string(),
            chambers: vec![RollChamber {
                slug: "legislative-assembly".to_string(),
                source: "https://example.org/members".to_string(),
                members: (0..members)
                    .map(|n| RollMember {
                        name: format!("Member {n}"),
                        party: "Example Party".to_string(),
                        seat: Some(format!("Seat {n}")),
                        since: None,
                        offices: Vec::new(),
                        profile: None,
                    })
                    .collect(),
            }],
        }
    }

    #[test]
    fn a_reading_far_short_of_its_chamber_or_over_it_is_refused() {
        let reference = reference_entry();
        assert!(check_roll(&roll_of(4), &reference).is_ok());
        assert!(
            check_roll(&roll_of(3), &reference).is_ok(),
            "a vacancy passes"
        );
        assert_eq!(
            check_roll(&roll_of(2), &reference).unwrap_err().to_string(),
            "2 members listed for the Legislative Assembly's 4 seats"
        );
        assert!(
            check_roll(&roll_of(5), &reference).is_err(),
            "more members than seats is a list read twice"
        );

        let no_chamber = StateRoll {
            chambers: Vec::new(),
            ..roll_of(4)
        };
        assert_eq!(
            check_roll(&no_chamber, &reference).unwrap_err().to_string(),
            "no member list for the Legislative Assembly"
        );

        let mut partyless = roll_of(4);
        partyless.chambers[0].members[1].party = " ".to_string();
        assert!(check_roll(&partyless, &reference)
            .unwrap_err()
            .to_string()
            .contains("no name or party"));
    }

    #[tokio::test]
    async fn a_parliament_that_cannot_be_read_is_skipped_not_failed() {
        let mut reference = reference_entry();
        reference.unavailable = Some("Its website turns away automated readers.".to_string());
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/states-tests/unavailable");
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::Local(crate::store::LocalStore::new(dir));
        sync_state(
            &store,
            StateCode::WA,
            Some(&reference),
            &Endpoints::at("http://127.0.0.1:1"),
        )
        .await
        .expect("a skip is not a failure");
        assert!(store
            .get_json::<StateRoll>(&roll_key(StateCode::WA))
            .await
            .unwrap()
            .is_none());

        let err = sync_state(&store, StateCode::WA, None, &Endpoints::default())
            .await
            .expect_err("no reference entry");
        assert!(err.to_string().contains("state-parliaments.json"), "{err}");
    }

    #[test]
    fn styles_and_titles_come_off_a_name() {
        assert_eq!(plain_name("The Honourable Eric Abetz"), "Eric Abetz");
        assert_eq!(
            plain_name("  The Hon.  Jeremy Rockliff MP "),
            "Jeremy Rockliff"
        );
        assert_eq!(plain_name("Dr Rosalie Woodruff MP"), "Rosalie Woodruff");
        assert_eq!(plain_name("Hon Craig Farrell MLC"), "Craig Farrell");
        assert_eq!(plain_name("Jeremy Hanson CSC MLA"), "Jeremy Hanson");
        // A name that merely starts like a title keeps it.
        assert_eq!(plain_name("Drew Honeysett"), "Drew Honeysett");
    }

    #[test]
    fn a_portfolio_splits_into_its_offices_and_no_further() {
        assert_eq!(
            split_offices("Premier and Minister for Veterans"),
            ["Premier", "Minister for Veterans"]
        );
        assert_eq!(
            split_offices("Deputy Premier, Minister for State Development, Infrastructure and Planning and Minister for Industrial Relations"),
            [
                "Deputy Premier",
                "Minister for State Development, Infrastructure and Planning",
                "Minister for Industrial Relations"
            ]
        );
        assert_eq!(
            split_offices("Minister for Finance, Trade, Employment and Training"),
            ["Minister for Finance, Trade, Employment and Training"],
            "one portfolio with commas of its own"
        );
        assert_eq!(
            split_offices("Assistant Minister to the Premier on Matters of State and New Citizens, Leader of the House"),
            [
                "Assistant Minister to the Premier on Matters of State and New Citizens",
                "Leader of the House"
            ]
        );
        assert!(split_offices("").is_empty());
    }

    #[test]
    fn each_parliament_syncs_and_stores_under_its_own_name() {
        assert_eq!(source_name(StateCode::NSW), "nsw-parliament");
        assert_eq!(source_name(StateCode::ACT), "act-parliament");
        assert_eq!(roll_key(StateCode::TAS), "canonical/states/tas.json");
    }
}
