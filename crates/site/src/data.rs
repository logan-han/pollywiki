//! Sole data access point for every page. Reads the JSONL bundles produced by
//! the ingest derive step (BUNDLES_DIR, or the committed sample data) once at
//! build time and exposes typed lookups. Pages template; they never compute.

use anyhow::{Context, Result};
use indexmap::IndexMap;
use pollywiki_schema::{
    js_compare, Bill, Division, Electorate, ElectorateResult, Meta, Party, Person, Vote,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct SiteData {
    pub people: Vec<Person>,
    pub parties: Vec<Party>,
    pub electorates: Vec<Electorate>,
    pub divisions: Vec<Division>,
    pub bills: Vec<Bill>,
    pub elections: Vec<ElectorateResult>,
    pub meta: Meta,
    pub bundles_dir: PathBuf,
    /// Canonical origin without a trailing slash, for absolute URLs in
    /// structured data and feeds.
    pub site_url: String,
    people_by_slug: HashMap<String, usize>,
    parties_by_slug: HashMap<String, usize>,
    electorates_by_slug: HashMap<String, usize>,
    bills_by_id: HashMap<String, usize>,
    divisions_by_id: HashMap<String, usize>,
    elections_by_electorate: HashMap<String, usize>,
    bill_links: Vec<TitleLink>,
}

struct TitleLink {
    lower: Vec<char>,
    href: String,
}

fn read_jsonl<T: serde::de::DeserializeOwned>(dir: &Path, file: &str) -> Result<Vec<T>> {
    let path = dir.join(file);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(&path)?;
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).with_context(|| format!("parsing {file}")))
        .collect()
}

impl SiteData {
    pub fn load(bundles_dir: &Path, site_url: &str) -> Result<SiteData> {
        let people: Vec<Person> = read_jsonl(bundles_dir, "people.jsonl")?;
        let mut parties: Vec<Party> = read_jsonl(bundles_dir, "parties.jsonl")?;
        parties.sort_by(|a, b| {
            seat_total(b)
                .cmp(&seat_total(a))
                .then_with(|| js_compare(&a.name, &b.name))
        });
        let electorates: Vec<Electorate> = read_jsonl(bundles_dir, "electorates.jsonl")?;
        let divisions: Vec<Division> = read_jsonl(bundles_dir, "divisions.jsonl")?;
        let bills: Vec<Bill> = read_jsonl(bundles_dir, "bills.jsonl")?;
        let elections: Vec<ElectorateResult> = read_jsonl(bundles_dir, "elections.jsonl")?;

        let meta_path = bundles_dir.join("meta.json");
        let meta: Meta = if meta_path.exists() {
            serde_json::from_str(&std::fs::read_to_string(&meta_path)?)?
        } else {
            Meta {
                generated_at: chrono::Utc::now()
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                sample: true,
                sources: IndexMap::new(),
            }
        };

        let mut people_by_slug = HashMap::new();
        for (i, p) in people.iter().enumerate() {
            people_by_slug.insert(p.slug.clone(), i);
        }
        let mut parties_by_slug = HashMap::new();
        for (i, p) in parties.iter().enumerate() {
            parties_by_slug.insert(p.slug.clone(), i);
        }
        let mut electorates_by_slug = HashMap::new();
        for (i, e) in electorates.iter().enumerate() {
            electorates_by_slug.insert(e.slug.clone(), i);
        }
        let mut bills_by_id = HashMap::new();
        for (i, b) in bills.iter().enumerate() {
            bills_by_id.insert(b.id.clone(), i);
        }
        let mut divisions_by_id = HashMap::new();
        for (i, d) in divisions.iter().enumerate() {
            divisions_by_id.insert(d.id.clone(), i);
        }
        let mut elections_by_electorate = HashMap::new();
        for (i, e) in elections.iter().enumerate() {
            elections_by_electorate.insert(e.electorate_slug.clone(), i);
        }

        let bill_links = build_title_links(&bills);

        Ok(SiteData {
            people,
            parties,
            electorates,
            divisions,
            bills,
            elections,
            meta,
            bundles_dir: bundles_dir.to_path_buf(),
            site_url: site_url.trim_end_matches('/').to_string(),
            people_by_slug,
            parties_by_slug,
            electorates_by_slug,
            bills_by_id,
            divisions_by_id,
            elections_by_electorate,
            bill_links,
        })
    }

    pub fn person_by_slug(&self, slug: &str) -> Option<&Person> {
        self.people_by_slug.get(slug).map(|&i| &self.people[i])
    }

    pub fn party_by_slug(&self, slug: &str) -> Option<&Party> {
        self.parties_by_slug.get(slug).map(|&i| &self.parties[i])
    }

    pub fn electorate_by_slug(&self, slug: &str) -> Option<&Electorate> {
        self.electorates_by_slug
            .get(slug)
            .map(|&i| &self.electorates[i])
    }

    pub fn bill_by_id(&self, id: &str) -> Option<&Bill> {
        self.bills_by_id.get(id).map(|&i| &self.bills[i])
    }

    pub fn division_by_id(&self, id: &str) -> Option<&Division> {
        self.divisions_by_id.get(id).map(|&i| &self.divisions[i])
    }

    pub fn election_for_electorate(&self, slug: &str) -> Option<&ElectorateResult> {
        self.elections_by_electorate
            .get(slug)
            .map(|&i| &self.elections[i])
    }

    pub fn members_of_party(&self, slug: &str) -> Vec<&Person> {
        self.people
            .iter()
            .filter(|p| p.group_slug == slug)
            .collect()
    }

    /// Other divisions in the same chamber on the same sitting day, in order.
    pub fn same_sitting_day(&self, division: &Division) -> Vec<&Division> {
        let mut out: Vec<&Division> = self
            .divisions
            .iter()
            .filter(|d| d.house == division.house && d.date == division.date && d.id != division.id)
            .collect();
        out.sort_by_key(|d| d.number);
        out
    }

    pub fn votes_for_person(&self, slug: &str) -> Vec<PersonVote<'_>> {
        let mut out = Vec::new();
        for division in &self.divisions {
            if let Some(vote) = division.votes.iter().find(|v| v.person_slug == slug) {
                out.push(PersonVote {
                    division,
                    vote: vote.vote,
                    against_group_majority: vote.against_group_majority == Some(true),
                });
            }
        }
        out
    }

    /// Per-party aye/no counts for one division.
    pub fn group_breakdown(&self, division: &Division) -> Vec<GroupBreakdownRow<'_>> {
        let mut rows: IndexMap<String, GroupBreakdownRow> = IndexMap::new();
        for vote in &division.votes {
            let person = self.person_by_slug(&vote.person_slug);
            let group = person
                .map(|p| p.group.clone())
                .unwrap_or_else(|| "Unknown".to_string());
            let group_slug = person
                .map(|p| p.group_slug.clone())
                .unwrap_or_else(|| "unknown".to_string());
            let row = rows
                .entry(group_slug.clone())
                .or_insert_with(|| GroupBreakdownRow {
                    party: self.party_by_slug(&group_slug),
                    group,
                    aye: 0,
                    no: 0,
                });
            match vote.vote {
                Vote::Aye => row.aye += 1,
                Vote::No => row.no += 1,
            }
        }
        let mut out: Vec<GroupBreakdownRow> = rows.into_values().collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.aye + r.no));
        out
    }

    /// Escapes plain text and wraps any known bill title in a link to its bill
    /// page. Longest titles claim their span first; matching is case-insensitive
    /// because generated text sometimes re-cases acronyms.
    pub fn link_bill_titles(&self, text: &str) -> String {
        let chars: Vec<char> = text.chars().collect();
        let lower: Vec<char> = chars.iter().map(|c| js_lower_char(*c)).collect();
        struct Span<'a> {
            start: usize,
            end: usize,
            href: &'a str,
        }
        let mut spans: Vec<Span> = Vec::new();
        for link in &self.bill_links {
            let mut idx = 0;
            while let Some(at) = find_chars(&lower, &link.lower, idx) {
                let end = at + link.lower.len();
                if !spans.iter().any(|s| at < s.end && end > s.start) {
                    spans.push(Span {
                        start: at,
                        end,
                        href: &link.href,
                    });
                }
                idx = end;
            }
        }
        spans.sort_by_key(|s| s.start);
        let mut out = String::new();
        let mut pos = 0;
        let slice = |from: usize, to: usize| chars[from..to].iter().collect::<String>();
        for span in &spans {
            out.push_str(&escape_html(&slice(pos, span.start)));
            out.push_str(&format!("<a href=\"{}\">", span.href));
            out.push_str(&escape_html(&slice(span.start, span.end)));
            out.push_str("</a>");
            pos = span.end;
        }
        out.push_str(&escape_html(&slice(pos, chars.len())));
        out
    }
}

/// Bill families: several acts amended under one recurring name, e.g.
/// "Treasury Laws Amendment (…) Bill 2026" × 12. Generated notes refer to
/// these collectively ("Treasury Laws Amendment bills"), which links to the
/// bills index filtered to the family rather than one arbitrary bill.
fn build_title_links(bills: &[Bill]) -> Vec<TitleLink> {
    let mut by_length: Vec<TitleLink> = bills
        .iter()
        .map(|b| TitleLink {
            lower: b.title.chars().map(js_lower_char).collect(),
            href: format!("/bills/{}/", b.id),
        })
        .collect();
    by_length.sort_by_key(|l| std::cmp::Reverse(l.lower.len()));

    let mut family_counts: IndexMap<String, usize> = IndexMap::new();
    for b in bills {
        let Some(at) = b.title.find(" (") else {
            continue;
        };
        let prefix = &b.title[..at];
        if prefix.split(' ').count() < 2 {
            continue;
        }
        *family_counts.entry(prefix.to_string()).or_insert(0) += 1;
    }
    let mut families: Vec<TitleLink> = family_counts
        .iter()
        .filter(|(_, &count)| count >= 2)
        .map(|(prefix, _)| TitleLink {
            lower: format!("{} bills", prefix.to_lowercase()).chars().collect(),
            href: format!("/bills/?q={}", encode_uri_component(prefix)),
        })
        .collect();
    families.sort_by_key(|l| std::cmp::Reverse(l.lower.len()));

    by_length.extend(families);
    by_length
}

fn js_lower_char(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(single), None) => single,
        _ => c,
    }
}

fn find_chars(haystack: &[char], needle: &[char], from: usize) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (from..=haystack.len() - needle.len()).find(|&i| haystack[i..i + needle.len()] == *needle)
}

pub struct PersonVote<'a> {
    pub division: &'a Division,
    pub vote: Vote,
    pub against_group_majority: bool,
}

pub struct GroupBreakdownRow<'a> {
    pub party: Option<&'a Party>,
    pub group: String,
    pub aye: i64,
    pub no: i64,
}

pub fn seat_total(party: &Party) -> i64 {
    party
        .seats
        .as_ref()
        .map(|s| s.representatives + s.senate)
        .unwrap_or(0)
}

/// URL path segment for a division: date-number under its house.
pub fn division_key(division: &Division) -> String {
    format!("{}-{}", division.date, division.number)
}

pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Headline size tier by title length, decided at build time so an 80-char
/// formal bill title never renders at display size.
pub fn title_tier(title: &str) -> &'static str {
    let len = title.encode_utf16().count();
    if len <= 45 {
        "title-l"
    } else if len <= 90 {
        "title-m"
    } else {
        "title-s"
    }
}

pub struct BillSummaryGroup {
    pub acts: String,
    pub items: Vec<String>,
}

/// Official bill summaries for multi-act bills follow a nested grammar:
/// "Amends the: Act A to: item; item; Act B to single item; Act C to: item".
/// Group items under the act they amend; single-act or short summaries
/// return None and render as prose or a flat list.
pub fn parse_bill_summary(summary: &str) -> Option<Vec<BillSummaryGroup>> {
    use regex::Regex;
    use std::sync::LazyLock;
    static AMENDS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^Amends the:?\s*").unwrap());
    static SPLIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r";\s+").unwrap());
    static LEADING_AND: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^and\s+").unwrap());
    static ACT_HEADING: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"^((?:[A-Z][A-Za-z0-9_'\u{2019}()\u{2013}\- ]*?(?:Act|Code|Regulations?)(?: \d{4})?)(?:,? and [A-Z][A-Za-z0-9_'\u{2019}()\u{2013}\- ]*?(?:Act|Code|Regulations?)(?: \d{4})?)*) to:?\s*(.*)$",
        )
        .unwrap()
    });

    let stripped = AMENDS.replace(summary, "");
    let parts: Vec<String> = SPLIT
        .split(&stripped)
        .map(|p| {
            let trimmed = p.trim();
            let no_and = LEADING_AND.replace(trimmed, "");
            no_and.strip_suffix('.').unwrap_or(&no_and).to_string()
        })
        .filter(|p| !p.is_empty())
        .collect();
    let mut groups: Vec<BillSummaryGroup> = Vec::new();
    for part in &parts {
        if let Some(caps) = ACT_HEADING.captures(part) {
            let items = match caps.get(2).map(|m| m.as_str()) {
                Some("") | None => Vec::new(),
                Some(rest) => vec![rest.to_string()],
            };
            groups.push(BillSummaryGroup {
                acts: caps[1].to_string(),
                items,
            });
        } else {
            // No heading to hang this item under, so the summary is not in
            // this grammar at all.
            groups.last_mut()?.items.push(part.clone());
        }
    }
    // Only worth grouping when there is more than one act group.
    if groups.len() >= 2 {
        Some(groups)
    } else {
        None
    }
}

pub enum Occupation {
    Parsed {
        role: String,
        org: String,
        period: String,
    },
    Raw(String),
}

/// Handbook occupation strings follow "Role at|for Organisation from X to Y."
/// Parse them into columns; anything that doesn't match renders verbatim.
pub fn parse_occupation(text: &str) -> Occupation {
    use regex::Regex;
    use std::sync::LazyLock;
    static PATTERN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(.+?) (?:at|for|with) (?:the )?(.+?)(?: from (.+?))?(?: to (.+))?$").unwrap()
    });
    let trimmed = text.trim();
    let trimmed = trimmed.strip_suffix('.').unwrap_or(trimmed);
    let Some(caps) = PATTERN.captures(trimmed) else {
        return Occupation::Raw(text.to_string());
    };
    let (Some(role), Some(org)) = (caps.get(1), caps.get(2)) else {
        return Occupation::Raw(text.to_string());
    };
    if org.as_str().is_empty() {
        return Occupation::Raw(text.to_string());
    }
    let from = caps.get(3).map(|m| m.as_str());
    let to = caps.get(4).map(|m| m.as_str());
    let period = match (from, to) {
        (Some(from), Some(to)) => format!("{} \u{2013} {}", dotted_date(from), dotted_date(to)),
        (Some(from), None) => format!("from {}", dotted_date(from)),
        _ => String::new(),
    };
    Occupation::Parsed {
        role: role.as_str().to_string(),
        org: org.as_str().to_string(),
        period,
    }
}

/// Handbook dates arrive as "29.8.2022"; render them in the site's style.
fn dotted_date(text: &str) -> String {
    use regex::Regex;
    use std::sync::LazyLock;
    static PATTERN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(\d{1,2})\.(\d{1,2})\.(\d{4})$").unwrap());
    let Some(caps) = PATTERN.captures(text.trim()) else {
        return text.to_string();
    };
    format_date(&format!("{}-{:0>2}-{:0>2}", &caps[3], &caps[2], &caps[1]))
}

pub struct Qualification {
    pub qual: String,
    pub institution: String,
}

/// "Diploma in Community Services, Victoria University" → columns.
pub fn parse_qualification(text: &str) -> Qualification {
    match text.rfind(", ") {
        None => Qualification {
            qual: text.to_string(),
            institution: String::new(),
        },
        Some(at) => Qualification {
            qual: text[..at].to_string(),
            institution: text[at + 2..].to_string(),
        },
    }
}

pub fn format_date(iso: &str) -> String {
    let mut parts = iso.split('-');
    let (y, m, d) = (
        parts.next().and_then(|v| v.parse::<u32>().ok()),
        parts.next().and_then(|v| v.parse::<u32>().ok()),
        parts.next().and_then(|v| v.parse::<u32>().ok()),
    );
    let (Some(y), Some(m), Some(d)) = (y, m, d) else {
        return iso.to_string();
    };
    if y == 0 || m == 0 || d == 0 {
        return iso.to_string();
    }
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    match MONTHS.get((m - 1) as usize) {
        Some(month) => format!("{d} {month} {y}"),
        None => iso.to_string(),
    }
}

pub fn state_name(code: &str) -> Option<&'static str> {
    match code {
        "NSW" => Some("New South Wales"),
        "VIC" => Some("Victoria"),
        "QLD" => Some("Queensland"),
        "WA" => Some("Western Australia"),
        "SA" => Some("South Australia"),
        "TAS" => Some("Tasmania"),
        "ACT" => Some("Australian Capital Territory"),
        "NT" => Some("Northern Territory"),
        _ => None,
    }
}

/// Everything except A-Z a-z 0-9 - _ . ! ~ * ' ( ), matching JavaScript.
pub fn encode_uri_component(input: &str) -> String {
    use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
    const SET: &AsciiSet = &NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'_')
        .remove(b'.')
        .remove(b'!')
        .remove(b'~')
        .remove(b'*')
        .remove(b'\'')
        .remove(b'(')
        .remove(b')');
    utf8_percent_encode(input, SET).to_string()
}

pub fn decode_uri_component(input: &str) -> String {
    percent_encoding::percent_decode_str(input)
        .decode_utf8_lossy()
        .into_owned()
}

/// Number.prototype.toFixed: round to `digits` decimals, ties away from zero
/// against the exact binary value.
pub fn to_fixed(value: f64, digits: usize) -> String {
    if !value.is_finite() {
        return value.to_string();
    }
    let negative = value < 0.0;
    let magnitude = value.abs();
    // 60 decimals is exact for every non-tie; dyadic ties terminate well before.
    let expanded = format!("{magnitude:.60}");
    let (int_part, frac_part) = expanded.split_once('.').unwrap_or((&expanded, ""));
    let mut digits_vec: Vec<u8> = int_part
        .bytes()
        .chain(frac_part.bytes())
        .map(|b| b - b'0')
        .collect();
    let int_len = int_part.len();
    let keep = int_len + digits;
    let round_up = digits_vec.get(keep).is_some_and(|&d| d >= 5);
    digits_vec.truncate(keep);
    if round_up {
        let mut i = digits_vec.len();
        loop {
            if i == 0 {
                digits_vec.insert(0, 1);
                break;
            }
            i -= 1;
            if digits_vec[i] == 9 {
                digits_vec[i] = 0;
            } else {
                digits_vec[i] += 1;
                break;
            }
        }
    }
    let int_len = digits_vec.len() - digits;
    let int_str: String = digits_vec[..int_len]
        .iter()
        .map(|d| (d + b'0') as char)
        .collect();
    let int_str = int_str.trim_start_matches('0');
    let int_str = if int_str.is_empty() { "0" } else { int_str };
    let frac_str: String = digits_vec[int_len..]
        .iter()
        .map(|d| (d + b'0') as char)
        .collect();
    let sign = if negative { "-" } else { "" };
    if digits == 0 {
        format!("{sign}{int_str}")
    } else {
        format!("{sign}{int_str}.{frac_str}")
    }
}

/// Number.prototype.toLocaleString('en-AU') for integers.
pub fn locale_int(value: i64) -> String {
    let digits = value.abs().to_string();
    let mut out = String::new();
    let len = digits.len();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if value < 0 {
        format!("-{out}")
    } else {
        out
    }
}

/// JavaScript's default number-to-string (shortest round-trip).
pub fn js_float(value: f64) -> String {
    if value == value.trunc() && value.is_finite() && value.abs() < 1e21 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_fixed_rounds_ties_away_from_zero_like_javascript() {
        assert_eq!(to_fixed(1.25, 1), "1.3");
        assert_eq!(to_fixed(3.25, 1), "3.3");
        assert_eq!(to_fixed(60.0, 2), "60.00");
        assert_eq!(to_fixed(34.5, 2), "34.50");
        assert_eq!(to_fixed(-0.04, 1), "-0.0");
        assert_eq!(to_fixed(0.999, 2), "1.00");
    }

    #[test]
    fn locale_int_groups_thousands() {
        assert_eq!(locale_int(0), "0");
        assert_eq!(locale_int(999), "999");
        assert_eq!(locale_int(1000), "1,000");
        assert_eq!(locale_int(1234567), "1,234,567");
    }

    #[test]
    fn js_float_prints_shortest_round_trip() {
        assert_eq!(js_float(50.0), "50");
        assert_eq!(js_float(94.0 / 150.0 * 100.0), "62.66666666666667");
        assert_eq!(js_float(40.0 / 123.0 * 100.0), "32.52032520325203");
    }

    #[test]
    fn title_tiers_by_utf16_length() {
        assert_eq!(title_tier("Short title"), "title-l");
        assert_eq!(title_tier(&"x".repeat(46)), "title-m");
        assert_eq!(title_tier(&"x".repeat(91)), "title-s");
    }

    #[test]
    fn bill_summaries_group_by_act_heading() {
        let grouped = parse_bill_summary(
            "Amends the: Corporations Act 2001 to: do one thing; do another; and Privacy Act 1988 to make a change.",
        )
        .unwrap();
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0].acts, "Corporations Act 2001");
        assert_eq!(grouped[0].items, vec!["do one thing", "do another"]);
        assert_eq!(grouped[1].acts, "Privacy Act 1988");
        assert!(parse_bill_summary("A plain sentence.").is_none());
    }

    #[test]
    fn occupations_parse_into_columns() {
        match parse_occupation("Solicitor at Smith and Co from 1.2.2001 to 29.8.2022.") {
            Occupation::Parsed { role, org, period } => {
                assert_eq!(role, "Solicitor");
                assert_eq!(org, "Smith and Co");
                assert_eq!(period, "1 Feb 2001 \u{2013} 29 Aug 2022");
            }
            Occupation::Raw(_) => panic!("expected parsed"),
        }
        assert!(matches!(
            parse_occupation("Freeform text"),
            Occupation::Raw(_)
        ));
    }

    #[test]
    fn format_date_renders_site_style() {
        assert_eq!(format_date("2025-05-03"), "3 May 2025");
        assert_eq!(format_date("not-a-date"), "not-a-date");
    }
}
