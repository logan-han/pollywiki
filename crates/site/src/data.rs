//! Sole data access point for every page. Reads the JSONL bundles produced by
//! the ingest derive step (BUNDLES_DIR, or the committed sample data) once at
//! build time and exposes typed lookups. Pages template; they never compute.

use anyhow::{Context, Result};
use indexmap::IndexMap;
pub use pollywiki_schema::title_from_slug;
use pollywiki_schema::{
    js_compare, Bill, Division, Electorate, ElectorateResult, House, Meta, Party, Person, Vote,
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

    /// Members who hold a seat right now. Former members keep their page, but
    /// stay out of anything describing the parliament as it stands.
    pub fn sitting(&self) -> impl Iterator<Item = &Person> {
        self.people.iter().filter(|p| !p.is_former())
    }

    pub fn former(&self) -> impl Iterator<Item = &Person> {
        self.people.iter().filter(|p| p.is_former())
    }

    pub fn members_of_party(&self, slug: &str) -> Vec<&Person> {
        self.sitting().filter(|p| p.group_slug == slug).collect()
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

/// One row of the occupations table. Only the role is always present: the
/// Handbook writes anything from a bare title ("Senior Manager") to a fully
/// dated placement, and the table shows only the columns a person's rows fill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occupation {
    pub role: String,
    pub org: String,
    pub period: String,
}

/// Handbook occupation strings are prose: "Role at|for|with|to|in|of (the)
/// Organisation from X to Y.", with dates as years, "Month YYYY" or
/// "d.m.yyyy", and the year sometimes hung off the organisation instead ("UK
/// Treasury, 2003"). The role keeps every word up to the first preposition
/// that introduces something organisation-shaped (see `organisation`), tried
/// in order of confidence: at/for/with/to first, then in, then of, then a bare
/// comma. So "Chief of Staff to Senator B Joyce" splits at "to", "Cash in
/// Transit Officer for Linfox Armaguard" at "for", and "Head of Partnerships"
/// not at all. Whatever cannot be split stays whole in the role column.
pub fn parse_occupation(text: &str) -> Occupation {
    use regex::Regex;
    use std::sync::LazyLock;
    const DATE: &str = r"(?:\d{1,2}\.\d{1,2}\.\d{4}|(?:January|February|March|April|May|June|July|August|September|October|November|December)(?:\s+\d{4})?|\d{4})";
    // "from X", "from X to Y" or a bare "to Y", only when what follows the
    // preposition is a date: "Adviser to Senator M Watt" is a placement.
    static PERIOD: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&format!(
            r"^(.*?),?\s+(?:from\s+({DATE})(?:\s+to\s+({DATE}))?|to\s+({DATE}))$"
        ))
        .unwrap()
    });
    // A year or year range left dangling on the end, e.g. "UK Treasury, 2003".
    static TRAILING_YEARS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"^(.*?[^\s,])\s*,?\s+((?:1[89]|20)\d{2})(?:\s*[-\u{2013}\u{2014}]\s*((?:1[89]|20)\d{2}))?$",
        )
        .unwrap()
    });

    let trimmed = text.trim();
    let trimmed = trimmed.strip_suffix('.').unwrap_or(trimmed).trim();
    let (body, mut period) = match PERIOD.captures(trimmed) {
        Some(caps) if caps.get(1).is_some_and(|m| !m.as_str().trim().is_empty()) => {
            let period = match (caps.get(2), caps.get(3), caps.get(4)) {
                (Some(from), Some(to), _) => format!(
                    "{} \u{2013} {}",
                    dotted_date(from.as_str()),
                    dotted_date(to.as_str())
                ),
                (Some(from), None, _) => format!("from {}", dotted_date(from.as_str())),
                (None, _, Some(to)) => format!("to {}", dotted_date(to.as_str())),
                _ => String::new(),
            };
            (caps[1].to_string(), period)
        }
        _ => (trimmed.to_string(), String::new()),
    };
    let mut body = tidy(&body);
    if period.is_empty() {
        let dangling = TRAILING_YEARS.captures(&body).map(|years| {
            let period = match years.get(3) {
                Some(end) => format!("{} \u{2013} {}", &years[2], end.as_str()),
                None => years[2].to_string(),
            };
            (tidy(&years[1]), period)
        });
        if let Some((rest, years)) = dangling {
            body = rest;
            period = years;
        }
    }
    let (role, org) = split_role(&body);
    Occupation { role, org, period }
}

/// Where the role ends and the organisation begins, or the whole text as the
/// role when no preposition introduces anything organisation-shaped.
fn split_role(body: &str) -> (String, String) {
    // In order of confidence. "at" and "with" name the employer; "for" and
    // "to" often name a function or a minister first ("General Manager for
    // Business Development at Perth Airport"); "in" and "of" sit inside titles
    // ("Cash in Transit Officer", "Chief of Staff") and only split when nothing
    // stronger does; a bare comma is the last resort.
    const TIERS: [&[&str]; 5] = [
        &[" at ", " with "],
        &[" for ", " to "],
        &[" in "],
        &[" of "],
        &[", "],
    ];
    let mut candidates: Vec<(usize, usize, &str)> = Vec::new();
    for (tier, preps) in TIERS.iter().enumerate() {
        for prep in preps.iter() {
            candidates.extend(body.match_indices(prep).map(|(i, _)| (tier, i, *prep)));
        }
    }
    candidates.sort_unstable();
    for (_, i, prep) in candidates {
        let role = tidy(&body[..i]);
        if role.is_empty() {
            continue;
        }
        let Some(org) = organisation(&body[i + prep.len()..], prep == " of ") else {
            continue;
        };
        // "Convener, Department of Juvenile Justice": the institution's head
        // noun was left on the role, so the comma is the real boundary.
        if let Some((head, tail)) = role.rsplit_once(", ") {
            let one_capitalised_word =
                !tail.contains(' ') && tail.chars().next().is_some_and(char::is_uppercase);
            if one_capitalised_word && !head.trim().is_empty() {
                let comma = i - role.len() + head.len();
                let institution = &body[comma + 2..];
                if let Some(org) = organisation(institution, false) {
                    return (tidy(head), org);
                }
            }
        }
        return (role, org);
    }
    (body.to_string(), String::new())
}

/// The text after a preposition, if it reads as an organisation rather than as
/// the rest of a title: it starts with "the", a capital, a digit or an acronym.
/// After "of", a lone capitalised word is not enough ("Head of Partnerships",
/// "Director of Nursing"); it needs more words or the marks of a proper name.
fn organisation(text: &str, after_of: bool) -> Option<String> {
    let text = text.trim();
    let (had_article, rest) = ["the ", "The ", "a ", "an "]
        .iter()
        .find_map(|article| text.strip_prefix(*article).map(|rest| (true, rest)))
        .unwrap_or((false, text));
    let rest = tidy(rest);
    let first = rest.chars().next()?;
    if !(first.is_uppercase() || first.is_ascii_digit()) {
        return None;
    }
    if after_of && !had_article {
        let marked = rest
            .chars()
            .any(|c| !c.is_alphabetic() && !c.is_whitespace())
            || rest.chars().skip(1).any(char::is_uppercase);
        if rest.split_whitespace().count() < 2 && !marked {
            return None;
        }
    }
    Some(rest)
}

/// Strip the whitespace and separator commas a split leaves on either side.
fn tidy(text: &str) -> String {
    text.trim().trim_end_matches(',').trim().to_string()
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

/// The Hansard title before the first semicolon, which names what was being
/// dealt with; the stage of the question follows it ("...; Second Reading").
pub fn division_matter(name: &str) -> &str {
    name.split_once(';')
        .map_or(name, |(matter, _)| matter)
        .trim()
}

/// The stage after the semicolon, if the name has one.
pub fn division_stage(name: &str) -> Option<&str> {
    name.split_once(';')
        .map(|(_, stage)| stage.trim())
        .filter(|stage| !stage.is_empty())
}

/// The divisions one chamber took on one matter in one sitting day, in the
/// order it took them. Most matters get a single division; a contested bill
/// can get ten, as each amendment is put and lost before the question itself.
pub struct DivisionSeries<'a> {
    pub house: House,
    pub date: &'a str,
    pub matter: &'a str,
    pub divisions: Vec<&'a Division>,
}

impl SiteData {
    /// The newest `limit` series, newest first. Divisions arrive newest first,
    /// so a series sits where its latest division does.
    pub fn latest_series(&self, limit: usize) -> Vec<DivisionSeries<'_>> {
        let mut groups: IndexMap<(House, &str, &str), Vec<&Division>> = IndexMap::new();
        for d in &self.divisions {
            groups
                .entry((d.house, d.date.as_str(), division_matter(&d.name)))
                .or_default()
                .push(d);
        }
        groups
            .into_iter()
            .take(limit)
            .map(|((house, date, matter), mut divisions)| {
                divisions.sort_by_key(|d| d.number);
                DivisionSeries {
                    house,
                    date,
                    matter,
                    divisions,
                }
            })
            .collect()
    }
}

/// Markdown reduced to its words: links keep their text, emphasis and quote
/// marks go, whitespace collapses. For one-line labels, not for rendering.
pub fn plain_text(markdown: &str) -> String {
    use regex::Regex;
    use std::sync::LazyLock;
    static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[([^\]]*)\]\([^)]*\)").unwrap());
    let unlinked = LINK.replace_all(markdown, "$1");
    let mut words: Vec<&str> = Vec::new();
    for line in unlinked.lines() {
        let line = line.trim_start_matches(['>', '#', ' ']);
        words.extend(line.split_whitespace());
    }
    words
        .join(" ")
        .replace(['*', '_', '`'], "")
        .trim()
        .to_string()
}

/// The first sentence of a note, for a one-line label. A stop only ends the
/// sentence when a capital follows it, so "Bill (No. 2) 2025" stays whole.
pub fn first_sentence(text: &str) -> &str {
    let text = text.trim();
    for (i, c) in text.char_indices() {
        if c != '.' {
            continue;
        }
        let mut rest = text[i + 1..].chars();
        if let (Some(' '), Some(next)) = (rest.next(), rest.next()) {
            if next.is_uppercase() {
                return &text[..=i];
            }
        }
    }
    text
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
        assert_eq!(
            parse_occupation("Solicitor at Smith and Co from 1.2.2001 to 29.8.2022."),
            Occupation {
                role: "Solicitor".into(),
                org: "Smith and Co".into(),
                period: "1 Feb 2001 \u{2013} 29 Aug 2022".into(),
            }
        );
        // No preposition and no date: the whole text is the role.
        assert_eq!(
            parse_occupation("Freeform text"),
            Occupation {
                role: "Freeform text".into(),
                org: String::new(),
                period: String::new(),
            }
        );
    }

    #[test]
    fn division_names_split_into_matter_and_stage() {
        let name = "Bills \u{2014} Example Bill 2026; Second Reading";
        assert_eq!(division_matter(name), "Bills \u{2014} Example Bill 2026");
        assert_eq!(division_stage(name), Some("Second Reading"));
        assert_eq!(
            division_matter("Motions \u{2014} Economy"),
            "Motions \u{2014} Economy"
        );
        assert_eq!(division_stage("Motions \u{2014} Economy"), None);
        assert_eq!(division_stage("Trailing;"), None);
    }

    #[test]
    fn plain_text_keeps_link_text_and_drops_markup() {
        assert_eq!(
            plain_text("> The majority voted for a [sample motion](https://example.com) to *demonstrate*.\n\n# Heading"),
            "The majority voted for a sample motion to demonstrate. Heading"
        );
    }

    #[test]
    fn first_sentence_stops_at_a_capital_not_an_abbreviation() {
        assert_eq!(
            first_sentence("The Senate put the Example Bill (No. 2) 2025. It carried."),
            "The Senate put the Example Bill (No. 2) 2025."
        );
        assert_eq!(first_sentence("  One sentence only "), "One sentence only");
        assert_eq!(first_sentence("Ends with a stop."), "Ends with a stop.");
    }

    #[test]
    fn format_date_renders_site_style() {
        assert_eq!(format_date("2025-05-03"), "3 May 2025");
        assert_eq!(format_date("not-a-date"), "not-a-date");
    }
}

#[cfg(test)]
mod occupation_tests {
    use super::*;

    fn parsed(text: &str) -> (String, String, String) {
        let Occupation { role, org, period } = parse_occupation(text);
        (role, org, period)
    }

    fn row(role: &str, org: &str, period: &str) -> (String, String, String) {
        (role.to_string(), org.to_string(), period.to_string())
    }

    /// The forms that appear on live Handbook profiles, including the ones that
    /// used to fall through to the verbatim row.
    #[test]
    fn occupations_handle_the_of_the_form_and_dangling_years() {
        assert_eq!(
            parsed("CEO of the Australian Business and Community Network from 2017 to 2021."),
            row(
                "CEO",
                "Australian Business and Community Network",
                "2017 \u{2013} 2021"
            )
        );
        assert_eq!(
            parsed("Managing Director of Carla Zampatti Pty. Ltd. from 2008 to 2016."),
            row(
                "Managing Director",
                "Carla Zampatti Pty. Ltd.",
                "2008 \u{2013} 2016"
            )
        );
        // A year the Handbook hung off the organisation instead of "from ... to".
        assert_eq!(
            parsed("Policy Analyst at UK Treasury, 2003."),
            row("Policy Analyst", "UK Treasury", "2003")
        );
        // The separator leaves a trailing comma on the organisation.
        assert_eq!(
            parsed("Change Leader at King's College Hospital, London, from 2005 to 2007."),
            row(
                "Change Leader",
                "King's College Hospital, London",
                "2005 \u{2013} 2007"
            )
        );
        // at/for/with wins over of, so a title containing "of" stays intact.
        assert_eq!(
            parsed("Member of the Board at Example Co from 2010 to 2012."),
            row("Member of the Board", "Example Co", "2010 \u{2013} 2012")
        );
    }

    /// A title is not a placement: "Head of Partnerships" was rendering as the
    /// role "Head" at the organisation "Partnerships".
    #[test]
    fn titles_stay_whole_when_nothing_organisation_shaped_follows() {
        assert_eq!(
            parsed("Head of Partnerships"),
            row("Head of Partnerships", "", "")
        );
        assert_eq!(
            parsed("Director of Nursing"),
            row("Director of Nursing", "", "")
        );
        assert_eq!(parsed("Senior Manager"), row("Senior Manager", "", ""));
        assert_eq!(parsed("Mother of four"), row("Mother of four", "", ""));
        assert_eq!(
            parsed("Sales manager of truck and bus parts"),
            row("Sales manager of truck and bus parts", "", "")
        );
        // A proper name after "of" does split: more than one word, an acronym,
        // or a mark no common noun carries.
        assert_eq!(
            parsed("Chair of Screen NSW"),
            row("Chair", "Screen NSW", "")
        );
        assert_eq!(
            parsed("Board Member of HESTA"),
            row("Board Member", "HESTA", "")
        );
        assert_eq!(
            parsed("Owner of Nurses@Work"),
            row("Owner", "Nurses@Work", "")
        );
        assert_eq!(
            parsed("Director of the Productivity Commission from 2019 to 2022."),
            row("Director", "Productivity Commission", "2019 \u{2013} 2022")
        );
    }

    #[test]
    fn a_dated_title_with_no_organisation_keeps_its_period() {
        assert_eq!(
            parsed("Journalist from 1991 to 2008."),
            row("Journalist", "", "1991 \u{2013} 2008")
        );
        assert_eq!(
            parsed("Associate Editor from January 2021."),
            row("Associate Editor", "", "from January 2021")
        );
        assert_eq!(
            parsed("Senior Project Officer for SA Health from December 2005 to December 2006."),
            row(
                "Senior Project Officer",
                "SA Health",
                "December 2005 \u{2013} December 2006"
            )
        );
        // An end with no start.
        assert_eq!(
            parsed("Non-Executive Director of the Cancer Council (WA) to 2016."),
            row("Non-Executive Director", "Cancer Council (WA)", "to 2016")
        );
        assert_eq!(
            parsed("Radio Presenter for 2KO, 1992."),
            row("Radio Presenter", "2KO", "1992")
        );
        assert_eq!(parsed("Solicitor, 1999."), row("Solicitor", "", "1999"));
    }

    /// The staffer forms: "to" introduces whoever the adviser worked for, and
    /// beats an "of" or "in" inside the title.
    #[test]
    fn staff_roles_split_at_to_not_at_of() {
        assert_eq!(
            parsed("Adviser to Senator S Mackay from 1996 to 1998."),
            row("Adviser", "Senator S Mackay", "1996 \u{2013} 1998")
        );
        assert_eq!(
            parsed("Chief of Staff to Senator B Joyce."),
            row("Chief of Staff", "Senator B Joyce", "")
        );
        assert_eq!(
            parsed("Chief of Staff in the Queensland Government"),
            row("Chief of Staff", "Queensland Government", "")
        );
        assert_eq!(
            parsed("Cash in Transit Officer for Linfox Armaguard"),
            row("Cash in Transit Officer", "Linfox Armaguard", "")
        );
        // "at" names the employer even when "in" comes first.
        assert_eq!(
            parsed("Lecturer in the School of Education at the Central Coast Campus, University of Newcastle."),
            row(
                "Lecturer in the School of Education",
                "Central Coast Campus, University of Newcastle",
                ""
            )
        );
        assert_eq!(
            parsed("General Manager for Business Development at Perth Airport, 2007."),
            row(
                "General Manager for Business Development",
                "Perth Airport",
                "2007"
            )
        );
        assert_eq!(
            parsed("Business Support Officer at Hydro Tasmania 1998"),
            row("Business Support Officer", "Hydro Tasmania", "1998")
        );
        assert_eq!(
            parsed("Project Manager for the Investor Group on Climate Change , 2014."),
            row(
                "Project Manager",
                "Investor Group on Climate Change",
                "2014"
            )
        );
        // Lower-case after the preposition is not an organisation.
        assert_eq!(
            parsed("Solicitor in private practice"),
            row("Solicitor in private practice", "", "")
        );
    }

    #[test]
    fn a_comma_separates_role_from_organisation_when_nothing_else_does() {
        assert_eq!(
            parsed("Director and Lecturer, Institute of Environmental Studies (UNSW)"),
            row(
                "Director and Lecturer",
                "Institute of Environmental Studies (UNSW)",
                ""
            )
        );
        assert_eq!(
            parsed("Legal Counsel, the Electrical Trades Union of Australia"),
            row("Legal Counsel", "Electrical Trades Union of Australia", "")
        );
        // The institution's head noun stays with the institution, not the role.
        assert_eq!(
            parsed("Assistant Secretary (Africa Branch), Department of Foreign Affairs and Trade from 2012 to 2013."),
            row(
                "Assistant Secretary (Africa Branch)",
                "Department of Foreign Affairs and Trade",
                "2012 \u{2013} 2013"
            )
        );
        assert_eq!(
            parsed("President, Board Chair and Board Member of Hills Community Aid & Information Service"),
            row(
                "President, Board Chair and Board Member",
                "Hills Community Aid & Information Service",
                ""
            )
        );
        // A list of trades is not a role and an organisation.
        assert_eq!(
            parsed("Organic market gardener, shepherd, fruit picker to 1999"),
            row(
                "Organic market gardener, shepherd, fruit picker",
                "",
                "to 1999"
            )
        );
    }
}
