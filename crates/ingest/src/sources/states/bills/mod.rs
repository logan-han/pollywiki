//! Bills in each state and territory parliament's current term, from the
//! official list that parliament or its legislation register publishes. Each
//! list is worded its own way, so each has a reader of its own, and they all
//! produce the same roll: every bill with where it stands, how far it has
//! come and the dated steps that got it there.

use super::StateReferenceEntry;
use crate::endpoints::Endpoints;
use crate::store::Store;
use anyhow::{anyhow, bail, Result};
use pollywiki_schema::{StateCode, TimelineStep};
use serde::{Deserialize, Serialize};

mod act;
mod nt;
mod qld;
mod sa;
mod tas;
mod vic;

pub const PREFIX: &str = "canonical/state-bills/";

/// The sync record name for one parliament's bills, e.g. "vic-bills".
pub fn source_name(state: StateCode) -> String {
    format!("{}-bills", state.as_str().to_lowercase())
}

/// Reads one parliament's bills and files them. Ok(false) for a parliament
/// with no reader yet, which leaves no sync record. A list that reads as
/// empty is refused, so a changed page cannot wipe out the last good one.
pub async fn sync_state_bills(
    store: &Store,
    state: StateCode,
    reference: Option<&StateReferenceEntry>,
    endpoints: &Endpoints,
) -> Result<bool> {
    let reference =
        reference.ok_or_else(|| anyhow!("no entry for {state} in state-parliaments.json"))?;
    let since = reference
        .bills_since
        .as_deref()
        .ok_or_else(|| anyhow!("no billsSince for {state} in state-parliaments.json"))?;
    let roll = match state {
        StateCode::VIC => vic::read(store, endpoints, since).await?,
        StateCode::TAS => tas::read(store, endpoints, since).await?,
        StateCode::ACT => act::read(store, endpoints, since).await?,
        StateCode::NT => nt::read(store, endpoints, since).await?,
        StateCode::QLD => qld::read(store, endpoints, since).await?,
        StateCode::SA => sa::read(store, endpoints, since).await?,
        _ => return Ok(false),
    };
    if roll.bills.is_empty() {
        bail!("{}: the list read as empty", source_name(state));
    }
    store.put_json(&bills_key(state), &roll).await?;
    println!("{}: {} bills", source_name(state), roll.bills.len());
    Ok(true)
}

pub fn bills_key(state: StateCode) -> String {
    format!("{PREFIX}{}.json", state.as_str().to_lowercase())
}

/// One parliament's bills as last read.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BillRoll {
    pub state: StateCode,
    pub listed_at: String,
    /// Who publishes the list: the parliament, or its legislation register.
    pub publisher: String,
    /// The list's own page.
    pub source: String,
    pub bills: Vec<RollBill>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RollBill {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<String>,
    /// The chamber it began in, as the list names it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sponsor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub status: String,
    /// 1 introduced, 2 passed the chamber it began in, 3 passed both,
    /// 4 assented.
    pub stage: u8,
    #[serde(default)]
    pub stopped: bool,
    #[serde(default)]
    pub timeline: Vec<TimelineStep>,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanatory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub act: Option<String>,
}

/// Every roll in the store, by state.
pub async fn load_all(store: &Store) -> anyhow::Result<Vec<BillRoll>> {
    let mut out = Vec::new();
    for key in store.list(PREFIX).await? {
        if let Some(roll) = store.get_json::<BillRoll>(&key).await? {
            out.push(roll);
        }
    }
    Ok(out)
}
