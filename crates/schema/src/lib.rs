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

/// One committee membership from the Handbook's records of service.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitteeService {
    pub name: String,
    /// The Handbook's committee type, e.g. "Joint Standing" or "Senate Select".
    pub kind: String,
    /// Chair, Deputy Chair, Substitute member and the like; plain membership has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// When the role ran on dates of its own within the membership.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role_to: Option<String>,
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
    /// Senate contests only. The count is state-wide, and votes and pct are
    /// the whole group's first preferences, not the candidate's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub senate: Option<SenateSeat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SenateSeat {
    pub state: StateCode,
    pub vacancies: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elected_order: Option<i64>,
}

/// One state's Senate count at one event, from the AEC's first-preference
/// and senators-elected files.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SenateResult {
    pub event_id: String,
    pub event_name: String,
    pub state: StateCode,
    pub vacancies: i64,
    pub formal_votes: i64,
    /// Ranked by first preferences. Ungrouped candidates stand alone.
    pub groups: Vec<SenateGroup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SenateGroup {
    /// The ballot-paper letter, or "UG" for an ungrouped candidate.
    pub ticket: String,
    pub party: String,
    /// Above-the-line votes plus every group candidate's own first preferences.
    pub votes: i64,
    pub pct: JsNum,
    pub candidates: Vec<SenateCandidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SenateCandidate {
    pub name: String,
    /// First given name and surname, where the ballot also carries middle
    /// names ("Kim John Carr" is "Kim Carr").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short_name: Option<String>,
    pub party: String,
    /// Below-the-line first preferences for this candidate alone.
    pub votes: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elected_order: Option<i64>,
}

/// A division's map: its outline over the divisions around it, already
/// projected into the view box.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Boundary {
    /// The ABS boundary set drawn, e.g. "ASGS 2025".
    pub set: String,
    pub view_box: String,
    /// SVG path data for the division, rings filled even-odd.
    pub path: String,
    #[serde(default)]
    pub neighbours: Vec<BoundaryNeighbour>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundaryNeighbour {
    pub slug: String,
    pub name: String,
    pub path: String,
    /// Where the name fits, in view units; none when it has no room.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<(i64, i64)>,
}

/// Who won a seat at one event, for an electorate's run of past results.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeatResult {
    pub event: String,
    pub event_name: String,
    pub member: String,
    pub party: String,
    /// The winner's share of the two-candidate-preferred count, where the
    /// AEC published one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tcp_pct: Option<JsNum>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub person_slug: Option<String>,
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

/// A parliamentarian's own disclosure returns to the AEC.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonFunding {
    /// Candidate returns, one per election stood at, newest first.
    #[serde(default)]
    pub elections: Vec<CandidateFunding>,
    /// Annual returns as a member, newest first.
    #[serde(default)]
    pub annual: Vec<MemberFunding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateFunding {
    /// The AEC's event name, e.g. "2025 Federal Election".
    pub event: String,
    pub electorate: String,
    pub nil: bool,
    pub gifts: i64,
    pub donors: i64,
    pub expenditure: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberFunding {
    pub year: String,
    pub donations: i64,
    pub donors: i64,
}

/// One quarter of a parliamentarian's expenses as IPEA reports them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpenseQuarter {
    /// IPEA's reporting period id, e.g. "2026Q02".
    pub period: String,
    /// IPEA's label, e.g. "Apr-Jun 2026".
    pub label: String,
    pub total: JsNum,
    /// By IPEA's high-level category, largest first.
    pub categories: Vec<ExpenseLine>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpenseLine {
    pub category: String,
    pub amount: JsNum,
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
    /// End of the seat this record describes. Set only once the term is over:
    /// the register keeps the page, marked former, because the divisions the
    /// person voted in are permanent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
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
    /// Committee memberships, current ones first (Handbook).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub committees: Option<Vec<CommitteeService>>,
    /// Contests this person stood in, from AEC results (House events).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elections: Option<Vec<ElectionContest>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<PersonStats>,
    /// The latest quarters of parliamentary expenses, newest first (IPEA).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expenses: Option<Vec<ExpenseQuarter>>,
    /// Disclosure returns lodged with the AEC.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub funding: Option<PersonFunding>,
}

impl Person {
    /// Whether this seat has ended. Former members keep their page but are
    /// left out of anything describing the parliament as it stands.
    pub fn is_former(&self) -> bool {
        self.until.is_some()
    }

    /// Whether the person held the seat on an ISO date, used to decide which
    /// divisions they could have voted in.
    pub fn served_on(&self, date: &str) -> bool {
        self.since.as_deref().is_none_or(|since| date >= since)
            && self.until.as_deref().is_none_or(|until| date <= until)
    }
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

/// A party's disclosure returns to the AEC, as lodged.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartyFunding {
    /// Annual returns of the party and each of its branches, newest year first.
    pub returns: Vec<AnnualReturn>,
    /// The financial year the donations below were disclosed in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub donations_year: Option<String>,
    /// Donations itemised in that year's returns, summed by donor, largest first.
    #[serde(default)]
    pub donations: Vec<DonorTotal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnualReturn {
    pub year: String,
    pub name: String,
    pub receipts: i64,
    pub payments: i64,
    pub debts: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DonorTotal {
    pub donor: String,
    pub value: i64,
    /// Itemised gifts making up the value.
    pub gifts: i64,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub funding: Option<PartyFunding>,
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
    /// Date the division was proclaimed, from the Parliamentary Handbook.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub established: Option<String>,
    /// Every contest for the seat under this name, newest first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<Vec<SeatResult>>,
    /// The division's map, from the ABS boundary set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boundary: Option<Boundary>,
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
    /// The member's name as the division record gave it. Kept so a vote still
    /// reads correctly when no person entry matches the slug (a new member
    /// Wikidata has not recorded yet).
    #[serde(default)]
    pub name: String,
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

/// The Act a bill became, from the Federal Register of Legislation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Act {
    /// The Register's id, e.g. "C2025A00015".
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<i64>,
    /// Date of assent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assent: Option<String>,
    /// The Register's own status word, e.g. "InForce" or "Repealed".
    pub status: String,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub act: Option<Act>,
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

impl CandidateResult {
    /// The party the AEC files give their informal pseudo-candidate, and the
    /// one the ingest writes on that row whatever the file called it.
    pub const INFORMAL: &'static str = "Informal";

    /// Whether this row is the informal ballots rather than a candidate. The
    /// AEC lists them among the candidates in its first-preference files;
    /// every consumer asks here, so none of them ranks the row, counts it as
    /// a formal vote or matches it to a person.
    pub fn is_informal(&self) -> bool {
        self.party == Self::INFORMAL
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElectorateResult {
    pub event_id: String,
    pub event_name: String,
    pub electorate_slug: String,
    pub electorate_name: String,
    pub state: StateCode,
    /// Every candidate's first preferences, then the informal row when the
    /// AEC file carries one. A candidate's pct is of formal votes; the
    /// informal row's is of all ballots cast, the AEC's informality rate.
    pub first_prefs: Vec<CandidateResult>,
    #[serde(default)]
    pub tcp: Vec<CandidateResult>,
}

impl ElectorateResult {
    /// Formal first-preference votes: the base every candidate's share is
    /// taken over, and the base of the swings the AEC publishes beside them.
    pub fn formal_votes(&self) -> i64 {
        self.first_prefs
            .iter()
            .filter(|c| !c.is_informal())
            .map(|c| c.votes)
            .sum()
    }

    /// The informal ballots, when the AEC file reported them.
    pub fn informal(&self) -> Option<&CandidateResult> {
        self.first_prefs.iter().find(|c| c.is_informal())
    }
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

/// Readable name for a slug whose entity is no longer in the bundles, e.g. an
/// electorate abolished at a redistribution. Not a true inverse of `slugify`:
/// it only restores word boundaries and capitals.
pub fn title_from_slug(slug: Option<&str>) -> String {
    let Some(slug) = slug else {
        return String::new();
    };
    let spaced = slug.replace('-', " ");
    let mut out = String::with_capacity(spaced.len());
    let mut at_boundary = true;
    for c in spaced.chars() {
        if at_boundary && c.is_ascii_lowercase() {
            out.push(c.to_ascii_uppercase());
        } else {
            out.push(c);
        }
        at_boundary = !c.is_alphanumeric();
    }
    out
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
        // Beyond the safe integer range, and for anything not finite, the
        // float form is the only one that survives the round trip.
        assert_eq!(
            serde_json::to_string(&JsNum(9_007_199_254_740_994.0)).unwrap(),
            "9007199254740994.0"
        );
        // JSON has no infinity, and neither serde_json nor JSON.stringify
        // invents one.
        assert_eq!(
            serde_json::to_string(&JsNum(f64::INFINITY)).unwrap(),
            "null"
        );
        assert_eq!(JsNum::from(2.5), JsNum(2.5));
    }

    #[test]
    fn every_state_code_round_trips_and_nothing_else_parses() {
        for code in [
            StateCode::NSW,
            StateCode::VIC,
            StateCode::QLD,
            StateCode::WA,
            StateCode::SA,
            StateCode::TAS,
            StateCode::ACT,
            StateCode::NT,
        ] {
            assert_eq!(StateCode::parse(code.as_str()), Some(code));
            assert_eq!(code.to_string(), code.as_str(), "Display matches as_str");
        }
        // Codes the AEC does not issue, including the lower-cased form.
        assert_eq!(StateCode::parse("nsw"), None);
        assert_eq!(StateCode::parse("JBT"), None);
    }

    #[test]
    fn seat_counts_are_read_and_written_by_chamber() {
        let mut seats = PartySeats {
            representatives: 3,
            senate: 1,
        };
        assert_eq!(seats.get(House::Representatives), 3);
        assert_eq!(seats.get(House::Senate), 1);
        *seats.get_mut(House::Senate) += 2;
        assert_eq!(seats.get(House::Senate), 3);
    }

    #[test]
    fn title_from_slug_restores_words_and_capitals() {
        assert_eq!(title_from_slug(Some("higgins")), "Higgins");
        assert_eq!(title_from_slug(Some("north-sydney")), "North Sydney");
        assert_eq!(title_from_slug(None), "");
    }

    #[test]
    fn served_on_bounds_a_term_at_both_ends() {
        let person: Person = serde_json::from_str(
            r#"{"slug":"casey-obrien","name":"Casey O'Brien","house":"representatives",
                "group":"Example Party","groupSlug":"example-party",
                "since":"2022-05-21","until":"2026-03-14","ids":{},"links":{}}"#,
        )
        .expect("fixture");
        assert!(person.is_former());
        assert!(!person.served_on("2022-05-20"));
        assert!(person.served_on("2022-05-21"), "the first day counts");
        assert!(person.served_on("2026-03-14"), "so does the last");
        assert!(!person.served_on("2026-03-15"));

        // An open-ended record has no upper bound, and no start means no lower.
        let sitting = Person {
            until: None,
            ..person.clone()
        };
        assert!(!sitting.is_former());
        assert!(sitting.served_on("2030-01-01"));
        let undated = Person {
            since: None,
            ..sitting
        };
        assert!(undated.served_on("1901-01-01"));
    }

    #[test]
    fn informal_ballots_are_told_apart_from_candidates_and_formal_votes() {
        let result: ElectorateResult = serde_json::from_str(
            r#"{"eventId":"31496","eventName":"2025 federal election",
                "electorateSlug":"sampleford","electorateName":"Sampleford","state":"VIC",
                "firstPrefs":[
                  {"name":"Alex Paterson","party":"Example Party","votes":60000,"pct":60,"elected":true},
                  {"name":"Casey Doe","party":"Independent","votes":40000,"pct":40,"elected":false},
                  {"name":"Informal","party":"Informal","votes":5000,"pct":4.76,"swing":0.4,"elected":false}
                ]}"#,
        )
        .expect("fixture");
        assert!(!result.first_prefs[0].is_informal());
        assert!(!result.first_prefs[1].is_informal());
        assert!(result.first_prefs[2].is_informal());
        assert_eq!(result.formal_votes(), 100_000, "informal is not formal");
        assert_eq!(result.informal().map(|c| c.votes), Some(5000));
        assert!(result.tcp.is_empty(), "the TCP table is optional");

        // Bundles written before the ingest named the row say "Informal
        // Informal"; it is the party that marks it, so they still agree.
        let older = CandidateResult {
            name: "Informal Informal".to_string(),
            ..result.first_prefs[2].clone()
        };
        assert!(older.is_informal());

        // With no informal row, every vote listed is formal.
        let formal_only = ElectorateResult {
            first_prefs: result.first_prefs[..2].to_vec(),
            ..result.clone()
        };
        assert_eq!(formal_only.formal_votes(), 100_000);
        assert!(formal_only.informal().is_none());
    }

    #[test]
    fn js_compare_orders_like_locale_compare() {
        use std::cmp::Ordering;
        assert_eq!(js_compare("Aged Care", "ANZAC Day"), Ordering::Less);
        assert_eq!(js_compare("a", "b"), Ordering::Less);
    }
}
