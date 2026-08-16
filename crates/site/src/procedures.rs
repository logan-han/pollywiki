//! Plain-English explanations of common motion types, matched against the
//! official division name. Static text, written once, no AI involved.
//! Order matters: first match wins.

use regex::Regex;
use std::sync::LazyLock;

pub struct Procedure {
    pattern: LazyLock<Regex>,
    pub label: &'static str,
    pub text: &'static str,
}

macro_rules! procedure {
    ($pattern:expr, $label:expr, $text:expr) => {
        Procedure {
            pattern: LazyLock::new(|| Regex::new($pattern).unwrap()),
            label: $label,
            text: $text,
        }
    };
}

static PROCEDURES: [Procedure; 15] = [
    procedure!(
        r"(?i)rearrangement",
        "Rearrangement",
        "A rearrangement motion changes the order in which the chamber deals with the day's business, usually to bring a particular item on for debate immediately. It decides scheduling only, not the substance of any bill."
    ),
    procedure!(
        r"(?i)suspend(sion of)? standing orders|standing orders be suspended",
        "Suspension of standing orders",
        "Standing orders are the chamber's rules of procedure. A suspension motion asks to set them aside temporarily, usually so a member can move a motion the rules would not otherwise allow at that time."
    ),
    procedure!(
        r"(?i)be now put",
        "Closure",
        "A closure (gag) motion ends debate immediately so the chamber votes on the question straight away."
    ),
    procedure!(
        r"(?i)no longer be heard|be no longer heard",
        "Member no longer heard",
        "This motion stops the member who is speaking from continuing their speech."
    ),
    procedure!(
        r"(?i)debate be adjourned|adjournment",
        "Adjournment",
        "Adjourning a debate postpones it to a later time. Voting no keeps the debate going now."
    ),
    procedure!(
        r"(?i)consideration in detail",
        "Consideration in detail",
        "Consideration in detail examines a bill part by part in the House. Divisions at this stage decide specific amendments to the bill's text."
    ),
    procedure!(
        r"(?i)in committee|committee of the whole",
        "Committee of the whole",
        "The Senate examines a bill in detail as a committee of the whole. Divisions at this stage decide specific amendments to the bill's text."
    ),
    procedure!(
        r"(?i)first reading",
        "First reading",
        "The first reading is the formal introduction of a bill. A division at this stage decides whether the bill is introduced at all."
    ),
    procedure!(
        r"(?i)second reading",
        "Second reading",
        "The second reading vote decides whether the chamber agrees with the bill in principle. A second reading amendment attaches commentary or conditions to that agreement without changing the bill's text."
    ),
    procedure!(
        r"(?i)third reading",
        "Third reading",
        "The third reading is the chamber's final vote on the bill in the form it has reached. Passing it sends the bill to the other chamber, or on for assent."
    ),
    procedure!(
        r"(?i)production of documents|order for the production",
        "Order for the production of documents",
        "An order for the production of documents requires the government to table specified documents. It is one of the Senate's main accountability tools."
    ),
    procedure!(
        r"(?i)disallow",
        "Disallowance",
        "A disallowance motion strikes down delegated legislation (a regulation or other instrument made under an act). If agreed to, the instrument ceases to have effect."
    ),
    procedure!(
        r"(?i)censure",
        "Censure",
        "A censure motion formally records the chamber's criticism of a minister or member. It has no direct legal effect."
    ),
    procedure!(
        r"(?i)federation chamber",
        "Federation Chamber referral",
        "This refers a bill to the Federation Chamber, the House's second debating venue, for its detail stages."
    ),
    procedure!(
        r"(?i)senate message|message from the senate|senate amendments|house of representatives message",
        "Message between the chambers",
        "The two chambers exchange messages when they disagree over amendments to a bill. This vote decides whether to accept the other chamber's position or insist on its own."
    ),
];

pub fn procedure_for(division_name: &str) -> Option<&'static Procedure> {
    PROCEDURES
        .iter()
        .find(|p| p.pattern.is_match(division_name))
}
