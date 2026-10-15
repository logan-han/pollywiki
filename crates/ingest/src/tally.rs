//! The arithmetic behind a member's note. Every division the member voted in
//! is read for what was being decided (a bill, or a kind of motion) and at
//! which stage, their ayes and noes are added up per bill and per kind, and
//! the model is handed those sums and nothing else. A note can then only
//! phrase counts it did not compute, and every number it uses can be checked
//! against them.

use indexmap::IndexMap;
use pollywiki_schema::{Division, Vote};
use std::collections::BTreeSet;

/// One of a member's votes, with the division it was cast in.
pub struct VoteRow<'a> {
    pub division: &'a Division,
    pub vote: Vote,
    /// Against the member's own party grouping's majority.
    pub crossed: bool,
}

/// What a division decided, read from its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Motion {
    /// The bill, or the subject of a motion. None for a plain business motion.
    pub subject: Option<String>,
    /// The question put: a bill's stage, or the kind of motion.
    pub kind: String,
    /// Whether `subject` names a bill.
    pub bill: bool,
}

/// Division names come in two shapes. They Vote For You's edited titles read
/// "<bill> - <stage> - <what the amendment sought>", or for motions
/// "<category> - <subject> - <kind>". Unedited Hansard titles read
/// "<category> — <subject>; <stage>", where the category is Bills, Business,
/// Motions, Documents, Committees and so on.
pub fn parse_motion(name: &str) -> Motion {
    let name = name.trim();
    if let Some((category, rest)) = split_category(name) {
        return from_category(category, rest);
    }
    let parts: Vec<&str> = name.split(" - ").map(str::trim).collect();
    match parts.as_slice() {
        [bill, stage, ..] if looks_like_bill(bill) => Motion {
            subject: Some(bill.to_string()),
            kind: stage.to_string(),
            bill: true,
        },
        [_, subject, kind, ..] => Motion {
            subject: Some(subject.to_string()),
            kind: kind.to_string(),
            bill: false,
        },
        [category, rest] => from_category(category, rest),
        _ => Motion {
            subject: None,
            kind: name.to_string(),
            bill: false,
        },
    }
}

fn split_category(name: &str) -> Option<(&str, &str)> {
    [" — ", " – "]
        .iter()
        .find_map(|sep| name.split_once(sep))
        .map(|(category, rest)| (category.trim(), rest.trim()))
}

fn from_category(category: &str, rest: &str) -> Motion {
    let (subject, stage) = match rest.rsplit_once("; ") {
        Some((subject, stage)) => (subject.trim(), Some(stage.trim())),
        None => (rest, None),
    };
    match category {
        "Bills" => Motion {
            subject: Some(subject.to_string()),
            kind: stage.unwrap_or("Bill").to_string(),
            bill: true,
        },
        // "Business — Rearrangement": the motion is its own kind.
        "Business" => Motion {
            subject: None,
            kind: rest.to_string(),
            bill: false,
        },
        _ => match stage {
            Some(stage) => Motion {
                subject: Some(subject.to_string()),
                kind: stage.to_string(),
                bill: false,
            },
            // "Matters of Urgency — Fossil Fuel Industry": the category is the kind.
            None => Motion {
                subject: Some(rest.to_string()),
                kind: category.to_string(),
                bill: false,
            },
        },
    }
}

fn looks_like_bill(s: &str) -> bool {
    s != "Bills" && (s.contains(" Bill ") || s.ends_with(" Bill") || s.contains(" Bills "))
}

/// Ayes and noes in one group of divisions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Count {
    pub ayes: usize,
    pub noes: usize,
}

impl Count {
    fn add(&mut self, vote: Vote) {
        match vote {
            Vote::Aye => self.ayes += 1,
            Vote::No => self.noes += 1,
        }
    }

    pub fn total(&self) -> usize {
        self.ayes + self.noes
    }

    /// Both sides always, so a zero is visible: "all" is only ever true of a
    /// group whose other side reads 0.
    fn render(&self) -> String {
        let total = self.total();
        let noun = if total == 1 { "division" } else { "divisions" };
        format!("{total} {noun}: aye {}, no {}", self.ayes, self.noes)
    }
}

/// A bill's divisions, per stage in order of first appearance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BillTally {
    pub bill: String,
    pub stages: Vec<(String, Count)>,
}

impl BillTally {
    pub fn total(&self) -> usize {
        self.stages.iter().map(|(_, c)| c.total()).sum()
    }
}

/// Divisions on one kind of motion that is not a stage of a bill, with the
/// subjects they concerned and how the member voted on each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindTally {
    pub kind: String,
    pub count: Count,
    pub subjects: Vec<(String, Vote)>,
}

/// A member's record added up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tally {
    /// Most divisions first.
    pub bills: Vec<BillTally>,
    /// Most divisions first.
    pub kinds: Vec<KindTally>,
    /// The divisions where the member voted against their grouping, as
    /// "date | division | voted aye".
    pub crossed: Vec<String>,
}

fn word(vote: Vote) -> &'static str {
    match vote {
        Vote::Aye => "aye",
        Vote::No => "no",
    }
}

impl Tally {
    pub fn of(votes: &[VoteRow<'_>]) -> Tally {
        let mut bills: IndexMap<String, IndexMap<String, Count>> = IndexMap::new();
        let mut kinds: IndexMap<String, (Count, Vec<(String, Vote)>)> = IndexMap::new();
        let mut crossed = Vec::new();
        for row in votes {
            let motion = parse_motion(&row.division.name);
            match (motion.bill, motion.subject) {
                (true, Some(bill)) => bills
                    .entry(bill)
                    .or_default()
                    .entry(motion.kind)
                    .or_default()
                    .add(row.vote),
                (_, subject) => {
                    let entry = kinds.entry(motion.kind).or_default();
                    entry.0.add(row.vote);
                    if let Some(subject) = subject {
                        entry.1.push((subject, row.vote));
                    }
                }
            }
            if row.crossed {
                crossed.push(format!(
                    "{} | {} | voted {}",
                    row.division.date,
                    row.division.name,
                    word(row.vote)
                ));
            }
        }
        let mut bills: Vec<BillTally> = bills
            .into_iter()
            .map(|(bill, stages)| BillTally {
                bill,
                stages: stages.into_iter().collect(),
            })
            .collect();
        // Stable sorts keep first appearance as the order among equals.
        bills.sort_by_key(|b| std::cmp::Reverse(b.total()));
        let mut kinds: Vec<KindTally> = kinds
            .into_iter()
            .map(|(kind, (count, subjects))| KindTally {
                kind,
                count,
                subjects,
            })
            .collect();
        kinds.sort_by_key(|k| std::cmp::Reverse(k.count.total()));
        Tally {
            bills,
            kinds,
            crossed,
        }
    }

    /// The data block a note is written from. Bills and kinds of motion with
    /// few divisions are summed into one line rather than listed, which keeps
    /// the block short and the note on what recurs.
    pub fn render(&self) -> String {
        const LISTED_BILLS: usize = 12;
        const MIN_BILL_DIVISIONS: usize = 3;
        const MIN_KIND_DIVISIONS: usize = 2;
        const SUBJECTS_SHOWN: usize = 6;

        let mut out = String::from("Bills, most divisions first (stage: divisions, ayes, noes):\n");
        let mut other = Count::default();
        let mut other_bills = 0;
        for (i, bill) in self.bills.iter().enumerate() {
            if i < LISTED_BILLS && bill.total() >= MIN_BILL_DIVISIONS {
                let stages = bill
                    .stages
                    .iter()
                    .map(|(stage, count)| format!("{stage}: {}", count.render()))
                    .collect::<Vec<_>>()
                    .join("; ");
                out.push_str(&format!("- {}: {stages}\n", bill.bill));
            } else {
                other_bills += 1;
                for (_, count) in &bill.stages {
                    other.ayes += count.ayes;
                    other.noes += count.noes;
                }
            }
        }
        if other_bills > 0 {
            out.push_str(&format!(
                "- {other_bills} other bills with fewer divisions each, not listed: {}\n",
                other.render()
            ));
        }
        if self.bills.is_empty() {
            out.push_str("- none\n");
        }

        out.push_str("\nMotions that are not a stage of a bill (kind: divisions, ayes, noes):\n");
        let mut other = Count::default();
        let mut other_kinds = 0;
        for kind in &self.kinds {
            if kind.count.total() < MIN_KIND_DIVISIONS {
                other_kinds += 1;
                other.ayes += kind.count.ayes;
                other.noes += kind.count.noes;
                continue;
            }
            let mut line = format!("- {}: {}", kind.kind, kind.count.render());
            if !kind.subjects.is_empty() && kind.subjects.len() <= SUBJECTS_SHOWN {
                let subjects = kind
                    .subjects
                    .iter()
                    .map(|(subject, vote)| format!("{subject} {}", word(*vote)))
                    .collect::<Vec<_>>()
                    .join("; ");
                line.push_str(&format!(" ({subjects})"));
            }
            out.push_str(&line);
            out.push('\n');
        }
        if other_kinds > 0 {
            out.push_str(&format!(
                "- {other_kinds} other kinds of motion with one division each, not listed: {}\n",
                other.render()
            ));
        }
        if self.kinds.is_empty() {
            out.push_str("- none\n");
        }

        out.push_str("\nDivisions where they voted against their party grouping's majority:\n");
        if self.crossed.is_empty() {
            out.push_str("- none\n");
        }
        for line in &self.crossed {
            out.push_str(&format!("- {line}\n"));
        }
        out
    }
}

/// Every whole number written in the text, so a note's figures can be checked
/// against the data it was written from.
pub fn numbers_in(text: &str) -> BTreeSet<u64> {
    let mut out = BTreeSet::new();
    let mut digits = String::new();
    for c in text.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_digit() {
            digits.push(c);
        } else if !digits.is_empty() {
            if let Ok(n) = digits.parse::<u64>() {
                out.insert(n);
            }
            digits.clear();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn motion(subject: Option<&str>, kind: &str, bill: bool) -> Motion {
        Motion {
            subject: subject.map(str::to_string),
            kind: kind.to_string(),
            bill,
        }
    }

    #[test]
    fn hansard_titles_split_into_category_subject_and_stage() {
        assert_eq!(
            parse_motion("Bills — Treasury Laws Amendment (Tax Reform No. 1) Bill 2026; Consideration in Detail"),
            motion(Some("Treasury Laws Amendment (Tax Reform No. 1) Bill 2026"), "Consideration in Detail", true)
        );
        assert_eq!(
            parse_motion("Motions — Prime Minister; Attempted Censure"),
            motion(Some("Prime Minister"), "Attempted Censure", false)
        );
        assert_eq!(
            parse_motion("Documents — Census; Order for the Production of Documents"),
            motion(
                Some("Census"),
                "Order for the Production of Documents",
                false
            )
        );
        assert_eq!(
            parse_motion("Business — Suspension of Standing and Sessional Orders"),
            motion(None, "Suspension of Standing and Sessional Orders", false)
        );
        assert_eq!(
            parse_motion("Matters of Urgency — Fossil Fuel Industry"),
            motion(Some("Fossil Fuel Industry"), "Matters of Urgency", false)
        );
        // A bill title with no stage after it.
        assert_eq!(
            parse_motion("Bills — Example Bill 2026"),
            motion(Some("Example Bill 2026"), "Bill", true)
        );
    }

    #[test]
    fn edited_titles_split_on_hyphens_with_the_bill_or_category_first() {
        assert_eq!(
            parse_motion("Fair Work Amendment (Protecting Penalty and Overtime Rates) Bill 2025 - Second Reading - Increase flexibility"),
            motion(Some("Fair Work Amendment (Protecting Penalty and Overtime Rates) Bill 2025"), "Second Reading", true)
        );
        assert_eq!(
            parse_motion("Universities Accord (Australian Tertiary Education Commission) Bill 2025 and another - Second Reading - Antisemitism"),
            motion(Some("Universities Accord (Australian Tertiary Education Commission) Bill 2025 and another"), "Second Reading", true)
        );
        assert_eq!(
            parse_motion("Motions - Telecommunications - Suspend the usual procedural rules"),
            motion(
                Some("Telecommunications"),
                "Suspend the usual procedural rules",
                false
            )
        );
        assert_eq!(
            parse_motion("Bills - Example; Second Reading"),
            motion(Some("Example"), "Second Reading", true)
        );
        assert_eq!(
            parse_motion("Second reading"),
            motion(None, "Second reading", false)
        );
    }

    fn division(name: &str) -> Division {
        serde_json::from_str(&format!(
            r#"{{"id":"representatives/2026-08-12/1","house":"representatives","date":"2026-08-12",
                 "number":1,"name":"{name}","result":"passed","ayes":80,"noes":40}}"#
        ))
        .expect("division fixture")
    }

    #[test]
    fn votes_are_added_up_per_bill_stage_and_kind_and_rendered_with_both_sides() {
        let divisions = [
            division("Bills — Example Bill 2026; Second Reading"),
            division("Bills — Example Bill 2026; Second Reading"),
            division("Bills — Example Bill 2026; Consideration in Detail"),
            division("Example Bill 2026 - Third Reading - Pass the bill"),
            division("Bills — Minor Bill 2026; Second Reading"),
            division("Motions — Prime Minister; Attempted Censure"),
            division("Motions — Minister for Defence Industry; Attempted Censure"),
            division("Business — Rearrangement"),
        ];
        let votes: Vec<VoteRow> = divisions
            .iter()
            .zip([
                Vote::Aye,
                Vote::No,
                Vote::No,
                Vote::Aye,
                Vote::No,
                Vote::Aye,
                Vote::No,
                Vote::Aye,
            ])
            .enumerate()
            .map(|(i, (division, vote))| VoteRow {
                division,
                vote,
                crossed: i == 6,
            })
            .collect();

        let tally = Tally::of(&votes);
        assert_eq!(tally.bills[0].bill, "Example Bill 2026");
        assert_eq!(
            tally.bills[0].stages,
            vec![
                ("Second Reading".to_string(), Count { ayes: 1, noes: 1 }),
                (
                    "Consideration in Detail".to_string(),
                    Count { ayes: 0, noes: 1 }
                ),
                ("Third Reading".to_string(), Count { ayes: 1, noes: 0 }),
            ]
        );
        assert_eq!(tally.kinds[0].kind, "Attempted Censure");
        assert_eq!(tally.kinds[0].count, Count { ayes: 1, noes: 1 });
        assert_eq!(
            tally.crossed,
            vec!["2026-08-12 | Motions — Minister for Defence Industry; Attempted Censure | voted no"]
        );

        let text = tally.render();
        assert!(text.contains(
            "- Example Bill 2026: Second Reading: 2 divisions: aye 1, no 1; Consideration in Detail: 1 division: aye 0, no 1; Third Reading: 1 division: aye 1, no 0"
        ), "{text}");
        // Minor Bill has one division, so it is summed, not listed.
        assert!(!text.contains("Minor Bill"), "{text}");
        assert!(
            text.contains(
                "- 1 other bills with fewer divisions each, not listed: 1 division: aye 0, no 1"
            ),
            "{text}"
        );
        assert!(text.contains(
            "- Attempted Censure: 2 divisions: aye 1, no 1 (Prime Minister aye; Minister for Defence Industry no)"
        ), "{text}");
        assert!(text.contains("- 1 other kinds of motion with one division each, not listed: 1 division: aye 1, no 0"), "{text}");
        assert!(text.contains("- 2026-08-12 | Motions — Minister for Defence Industry; Attempted Censure | voted no"), "{text}");
    }

    #[test]
    fn an_empty_record_renders_none_everywhere() {
        let text = Tally::of(&[]).render();
        assert_eq!(text.matches("- none").count(), 3, "{text}");
    }

    #[test]
    fn numbers_are_every_digit_run() {
        let numbers = numbers_in("aye in 11 of 14 divisions on the Bill (No. 1) 2026, 2025-07-28");
        assert_eq!(
            numbers.into_iter().collect::<Vec<_>>(),
            vec![1, 7, 11, 14, 28, 2025, 2026]
        );
        assert!(numbers_in("no digits").is_empty());
    }
}
