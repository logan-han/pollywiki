//! Entity schemas: the single source of truth for everything the ingest
//! writes and the site reads. Field order here defines JSON key order.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::OnceLock;
use unicode_normalization::UnicodeNormalization;

pub const HOUSES: [House; 2] = [House::Representatives, House::Senate];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum House {
    Representatives,
    Senate,
}

impl House {
    pub fn as_str(self) -> &'static str {
        match self {
            House::Representatives => "representatives",
            House::Senate => "senate",
        }
    }
}

impl fmt::Display for House {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

pub const STATES: [&str; 8] = ["NSW", "VIC", "QLD", "WA", "SA", "TAS", "ACT", "NT"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StateCode {
    NSW,
    VIC,
    QLD,
    WA,
    SA,
    TAS,
    ACT,
    NT,
}

impl StateCode {
    pub fn as_str(self) -> &'static str {
        match self {
            StateCode::NSW => "NSW",
            StateCode::VIC => "VIC",
            StateCode::QLD => "QLD",
            StateCode::WA => "WA",
            StateCode::SA => "SA",
            StateCode::TAS => "TAS",
            StateCode::ACT => "ACT",
            StateCode::NT => "NT",
        }
    }

    pub fn parse(value: &str) -> Option<StateCode> {
        match value {
            "NSW" => Some(StateCode::NSW),
            "VIC" => Some(StateCode::VIC),
            "QLD" => Some(StateCode::QLD),
            "WA" => Some(StateCode::WA),
            "SA" => Some(StateCode::SA),
            "TAS" => Some(StateCode::TAS),
            "ACT" => Some(StateCode::ACT),
            "NT" => Some(StateCode::NT),
            _ => None,
        }
    }
}

impl fmt::Display for StateCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An f64 that serialises the way JavaScript's JSON.stringify does:
/// whole values print without a fractional part (60, not 60.0).
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(transparent)]
pub struct JsNum(pub f64);

impl Serialize for JsNum {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let v = self.0;
        if v.is_finite() && v.fract() == 0.0 && v.abs() < 9_007_199_254_740_992.0 {
            serializer.serialize_i64(v as i64)
        } else {
            serializer.serialize_f64(v)
        }
    }
}

impl From<f64> for JsNum {
    fn from(v: f64) -> Self {
        JsNum(v)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Photo {
    pub commons_file: String,
    pub url: String,
    pub licence: String,
    pub attribution: String,
    /// Site-relative mirrored thumbnails, set once the image sync has run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumb: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumb_large: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PositionKind {
    Ministry,
    Shadow,
    Position,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PositionRecord {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ministry: Option<String>,
    pub kind: PositionKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Background {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub born: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub birthplace: Option<String>,
    #[serde(default)]
    pub occupations: Vec<String>,
    #[serde(default)]
    pub qualifications: Vec<String>,
    #[serde(default)]
    pub honours: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_start: Option<String>,
    #[serde(default)]
    pub parliaments: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElectionContest {
    pub event: String,
    pub event_name: String,
    pub electorate_slug: String,
    pub electorate_name: String,
    pub party: String,
    pub votes: i64,
    pub pct: JsNum,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub swing: Option<JsNum>,
    pub elected: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonIds {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wikidata: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tvfy: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aph: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aec_candidate: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonLinks {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wikipedia: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiText {
    pub text: String,
    pub model: String,
    pub generated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonStats {
    pub divisions_eligible: i64,
    pub divisions_voted: i64,
    pub against_group_majority: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    pub slug: String,
    pub name: String,
    pub house: House,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<StateCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub electorate: Option<String>,
    pub group: String,
    pub group_slug: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    #[serde(default)]
    pub ids: PersonIds,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub photo: Option<Photo>,
    #[serde(default)]
    pub links: PersonLinks,
    /// Machine-written descriptive note about the voting record. Never evaluative.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai_note: Option<AiText>,
    /// Career and biographical facts from the Parliamentary Handbook.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<Background>,
    /// Dated ministry, shadow ministry and parliamentary positions (Handbook).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub positions: Option<Vec<PositionRecord>>,
    /// Contests this person stood in, from AEC results (House events).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elections: Option<Vec<ElectionContest>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<PersonStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartySeats {
    pub representatives: i64,
    pub senate: i64,
}

impl PartySeats {
    pub fn get(&self, house: House) -> i64 {
        match house {
            House::Representatives => self.representatives,
            House::Senate => self.senate,
        }
    }

    pub fn get_mut(&mut self, house: House) -> &mut i64 {
        match house {
            House::Representatives => &mut self.representatives,
            House::Senate => &mut self.senate,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartyFacts {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub founded: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub website: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wikipedia: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Party {
    pub slug: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub colour: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seats: Option<PartySeats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub facts: Option<PartyFacts>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElectorateProfileFacts {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name_derivation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub area: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gazetted: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_contested: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub demographic: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Electorate {
    pub slug: String,
    pub name: String,
    pub state: StateCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member_slug: Option<String>,
    /// Facts from the AEC's official electorate profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<ElectorateProfileFacts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enrolment: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Vote {
    Aye,
    No,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoteCast {
    pub person_slug: String,
    pub vote: Vote,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub teller: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub against_group_majority: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SummaryKind {
    Summary,
    Transcript,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DivisionResult {
    Passed,
    Rejected,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DivisionLinks {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hansard: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tvfy: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Division {
    pub id: String,
    pub house: House,
    pub date: String,
    pub number: i64,
    pub name: String,
    /// Plain-English context written by They Vote For You volunteers (markdown, ODbL).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Whether summary reads as written context or as a Hansard transcript excerpt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_kind: Option<SummaryKind>,
    /// Machine-written context, only generated when summary is a transcript.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai_summary: Option<AiText>,
    pub result: DivisionResult,
    pub ayes: i64,
    pub noes: i64,
    #[serde(default)]
    pub bill_ids: Vec<String>,
    #[serde(default)]
    pub links: DivisionLinks,
    #[serde(default)]
    pub votes: Vec<VoteCast>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineStep {
    pub date: String,
    pub event: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BillLinks {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aph: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub em: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parlinfo: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BillRaiser {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bill {
    pub id: String,
    pub title: String,
    pub parliament: i64,
    pub chamber: House,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub bill_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sponsor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub portfolio: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Machine-written plain-English explanation of what the bill is about.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai_summary: Option<AiText>,
    pub status: String,
    #[serde(default)]
    pub timeline: Vec<TimelineStep>,
    #[serde(default)]
    pub links: BillLinks,
    /// List-response freshness marker driving incremental detail fetches.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_updated: Option<String>,
    /// Who raised the bill: sponsors for private bills, movers otherwise.
    #[serde(default)]
    pub sponsors: Vec<BillRaiser>,
    #[serde(default)]
    pub movers: Vec<BillRaiser>,
    #[serde(default)]
    pub division_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateResult {
    pub name: String,
    pub party: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub party_code: Option<String>,
    pub votes: i64,
    pub pct: JsNum,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub swing: Option<JsNum>,
    pub elected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElectorateResult {
    pub event_id: String,
    pub event_name: String,
    pub electorate_slug: String,
    pub electorate_name: String,
    pub state: StateCode,
    pub first_prefs: Vec<CandidateResult>,
    #[serde(default)]
    pub tcp: Vec<CandidateResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceStatus {
    pub last_sync: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    pub generated_at: String,
    #[serde(default)]
    pub sample: bool,
    #[serde(default)]
    pub sources: indexmap::IndexMap<String, SourceStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuickSearchEntry {
    pub t: String,
    pub slug: String,
    pub name: String,
    pub sub: String,
}

/// Kebab-case slug: lowercase, ASCII, hyphen separated. Stable across syncs.
pub fn slugify(input: &str) -> String {
    let stripped: String = input
        .nfkd()
        .filter(|c| !('\u{0300}'..='\u{036F}').contains(c))
        .collect();
    let lowered = stripped.to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    for c in lowered.chars() {
        if c == '\'' || c == '\u{2019}' {
            continue;
        }
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// String comparison matching JavaScript's default localeCompare (ICU en).
pub fn js_compare(a: &str, b: &str) -> std::cmp::Ordering {
    use icu::collator::{options::CollatorOptions, Collator, CollatorBorrowed};
    use icu::locale::locale;
    static COLLATOR: OnceLock<CollatorBorrowed<'static>> = OnceLock::new();
    let collator = COLLATOR.get_or_init(|| {
        Collator::try_new(locale!("en").into(), CollatorOptions::default())
            .expect("en collation data is compiled in")
    });
    collator.compare(a, b)
}

pub const BUNDLE_PEOPLE: &str = "people.jsonl";
pub const BUNDLE_PARTIES: &str = "parties.jsonl";
pub const BUNDLE_ELECTORATES: &str = "electorates.jsonl";
pub const BUNDLE_DIVISIONS: &str = "divisions.jsonl";
pub const BUNDLE_BILLS: &str = "bills.jsonl";
pub const BUNDLE_ELECTIONS: &str = "elections.jsonl";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_handles_punctuation_and_diacritics() {
        assert_eq!(slugify("Anthony Albanese"), "anthony-albanese");
        assert_eq!(
            slugify("Pauline Hanson's One Nation"),
            "pauline-hansons-one-nation"
        );
        assert_eq!(
            slugify("Liberal\u{2013}National Coalition"),
            "liberal-national-coalition"
        );
        assert_eq!(slugify("Zo\u{eb} Daniel"), "zoe-daniel");
        assert_eq!(slugify("O'Brien"), "obrien");
    }

    #[test]
    fn js_num_serialises_like_javascript() {
        assert_eq!(serde_json::to_string(&JsNum(60.0)).unwrap(), "60");
        assert_eq!(serde_json::to_string(&JsNum(1.25)).unwrap(), "1.25");
        assert_eq!(serde_json::to_string(&JsNum(-0.0)).unwrap(), "0");
    }

    #[test]
    fn js_compare_orders_like_locale_compare() {
        use std::cmp::Ordering;
        assert_eq!(js_compare("Aged Care", "ANZAC Day"), Ordering::Less);
        assert_eq!(js_compare("a", "b"), Ordering::Less);
    }
}
