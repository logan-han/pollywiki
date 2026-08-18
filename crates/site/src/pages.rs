//! Every page template. Text flows are written pre-collapsed, so these
//! strings carry the exact bytes the pages render; the original constraint of
//! matching an earlier build byte-for-byte no longer applies.

use crate::components::{
    avatar, bill_dots, chamber_chip, group_chip, ledger_month, ledger_row, person_card,
    result_chip, result_word, seat_bar, vote_bar, BILL_DOTS_LEGEND,
};
use crate::data::{
    self, division_key, format_date, locale_int, parse_bill_summary, parse_occupation,
    parse_qualification, state_name, title_tier, to_fixed, Occupation, SiteData,
};
use crate::html::{esc, esc_attr};
use crate::layout::Page;
use crate::markdown;
use crate::procedures::procedure_for;
use pollywiki_schema::{Bill, Division, Electorate, House, Party, Person, SummaryKind, Vote};
use regex::Regex;
use std::sync::LazyLock;

// One filter script per index page, inlined where the page needs it.
const PEOPLE_FILTER_JS: &str = include_str!("../assets/js/people-filter.js");
const DIVISION_FILTER_JS: &str = include_str!("../assets/js/division-filter.js");
const BILL_FILTER_JS: &str = include_str!("../assets/js/bill-filter.js");
const ELECTORATE_FILTER_JS: &str = include_str!("../assets/js/electorate-filter.js");

/// Live count plus an explicit empty state, shared by the four index pages.
/// The count element stays in the DOM so its aria-live region is stable; it
/// reads empty while nothing is filtered.
fn filter_feedback(empty_message: &str) -> String {
    format!(
        "<p class=\"note filter-count\" id=\"filter-count\" aria-live=\"polite\"></p><div class=\"filter-empty\" id=\"filter-empty\" hidden><p>{}</p><button type=\"button\" id=\"filter-clear\">Clear filters</button></div>",
        esc(empty_message)
    )
}

/// JSON-LD for a <script> block. Angle brackets are escaped so no title can
/// close the element early.
fn jsonld_script(items: Vec<serde_json::Value>) -> String {
    let value = if items.len() == 1 {
        items.into_iter().next().unwrap_or(serde_json::Value::Null)
    } else {
        serde_json::Value::Array(items)
    };
    value
        .to_string()
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
}

fn breadcrumb(site_url: &str, trail: &[(&str, &str)]) -> serde_json::Value {
    serde_json::json!({
        "@context": "https://schema.org",
        "@type": "BreadcrumbList",
        "itemListElement": trail
            .iter()
            .enumerate()
            .map(|(i, (name, path))| serde_json::json!({
                "@type": "ListItem",
                "position": i + 1,
                "name": name,
                "item": format!("{site_url}{path}"),
            }))
            .collect::<Vec<_>>(),
    })
}

fn chamber_word(house: House) -> &'static str {
    match house {
        House::Senate => "Senate",
        House::Representatives => "House",
    }
}

fn full_chamber(house: House) -> &'static str {
    match house {
        House::Senate => "Senate",
        House::Representatives => "House of Representatives",
    }
}

/// Pill bucket for the bills index: still before a chamber, already law, or
/// finished some other way (negatived, discharged, not proceeding). APH reports
/// law as either "Act" or "Assent"; both belong in the same bucket.
fn bill_status_key(bill: &Bill) -> &'static str {
    if bill.status.starts_with("Before") {
        "open"
    } else if bill.status == "Act" || bill.status == "Assent" {
        "act"
    } else {
        "other"
    }
}

/// The ingest stores each step as "description (Chamber)", which reads right on
/// the bill page but is far too long for a homepage row. The chamber is already
/// carried by the progress dots, so the row shows the description alone and
/// keeps the full string in a title attribute.
fn event_description(event: &str) -> &str {
    for suffix in [" (House of Representatives)", " (Senate)"] {
        if let Some(head) = event.strip_suffix(suffix) {
            return head;
        }
    }
    event
}

/// The most recent recorded step, by date. Bills with no timeline return None.
pub fn latest_step(bill: &Bill) -> Option<&pollywiki_schema::TimelineStep> {
    bill.timeline.iter().max_by(|a, b| a.date.cmp(&b.date))
}

fn bill_row(bill: &Bill, with_filter_text: bool) -> String {
    let li = if with_filter_text {
        format!(
            "<li data-status=\"{}\" data-text=\"{}\">",
            bill_status_key(bill),
            esc_attr(
                &format!("{} {}", bill.title, bill.portfolio.as_deref().unwrap_or(""))
                    .to_lowercase()
            )
        )
    } else {
        "<li>".to_string()
    };
    format!(
        "{li}<span><a href=\"/bills/{id}/\">{title}</a></span><span class=\"chamber\">{chamber}</span><span>{dots}</span><span class=\"{status_class}\">{status}</span></li>",
        id = bill.id,
        title = esc(&bill.title),
        chamber = chamber_word(bill.chamber),
        dots = bill_dots(bill),
        status_class = if bill.status.starts_with("Before") {
            "status open"
        } else {
            "status"
        },
        status = esc(&bill.status),
    )
}

/// Homepage row: title, progress dots, and what last happened to the bill.
/// The date drops its year to keep the mono line on one row; the full date
/// stays machine-readable in the <time> element and on the bill page.
fn bill_activity_row(bill: &Bill) -> String {
    let event =
        match latest_step(bill) {
            Some(step) => format!(
            "<span class=\"event\" title=\"{}\">{} \u{b7} <time datetime=\"{}\">{}</time></span>",
            esc_attr(&format!("{} \u{b7} {}", step.event, format_date(&step.date))),
            esc(event_description(&step.event)),
            esc_attr(&step.date),
            esc(&short_date(&step.date)),
        ),
            None => format!("<span class=\"event\">{}</span>", esc(&bill.status)),
        };
    format!(
        "<li><span><a href=\"/bills/{id}/\">{title}</a></span><span>{dots}</span>{event}</li>",
        id = bill.id,
        title = esc(&bill.title),
        dots = bill_dots(bill),
    )
}

/// "2025-08-01" -> "1 Aug". Falls back to the full rendering if unparseable.
fn short_date(iso: &str) -> String {
    let full = format_date(iso);
    match full.rsplit_once(' ') {
        Some((head, year)) if year.len() == 4 && year.chars().all(|c| c.is_ascii_digit()) => {
            head.to_string()
        }
        _ => full,
    }
}

pub fn home(data: &SiteData) -> Page {
    let mut body = String::new();
    body.push_str("<div class=\"masthead\"><h1>What federal parliament actually did.</h1><p class=\"lede\">Decisions that change people's lives should stay on the record. pollywiki keeps the record of Australia's federal parliament: who sits in it, how they voted, and what became law, straight from official sources, with no commentary.</p></div>");
    body.push_str("<h2>Composition of the 48th Parliament</h2>");
    body.push_str(&format!(
        "<p class=\"note\">{} sitting parliamentarians, grouped by parliamentary group.</p>",
        data.people.len()
    ));
    body.push_str(&seat_bar(data, House::Representatives));
    body.push_str(&seat_bar(data, House::Senate));

    body.push_str("<div class=\"section-head\"><h2>Latest divisions</h2><a href=\"/divisions/\">All divisions →</a></div>");
    let latest: Vec<&Division> = data.divisions.iter().take(8).collect();
    if !latest.is_empty() {
        body.push_str("<ul class=\"ledger\">");
        for d in latest {
            body.push_str(&ledger_row(d));
        }
        body.push_str("</ul>");
    } else {
        body.push_str("<p class=\"note\">No division records loaded yet. Votes appear here once the They Vote For You sync runs.</p>");
    }

    body.push_str("<div class=\"section-head\"><h2>Latest bill activity</h2><a href=\"/bills/\">All bills →</a></div>");
    // Newest recorded step first. The sort is stable, so bills with no
    // timeline keep the bundle's alphabetical order at the bottom.
    let mut by_activity: Vec<&Bill> = data.bills.iter().collect();
    by_activity.sort_by(|a, b| {
        latest_step(b)
            .map(|s| s.date.as_str())
            .cmp(&latest_step(a).map(|s| s.date.as_str()))
    });
    let recent: Vec<&Bill> = by_activity.into_iter().take(8).collect();
    if !recent.is_empty() {
        body.push_str("<ul class=\"bill-list activity\">");
        for b in recent {
            body.push_str(&bill_activity_row(b));
        }
        body.push_str("</ul>");
        body.push_str(BILL_DOTS_LEGEND);
    } else {
        body.push_str("<p class=\"note\">No bill records loaded yet.</p>");
    }

    let mut page = Page::new("pollywiki", None, "/", body);
    page.jsonld = Some(jsonld_script(vec![serde_json::json!({
        "@context": "https://schema.org",
        "@type": "WebSite",
        "name": "pollywiki",
        "url": format!("{}/", data.site_url),
        "description": crate::layout::DEFAULT_DESCRIPTION,
        "potentialAction": {
            "@type": "SearchAction",
            "target": {
                "@type": "EntryPoint",
                "urlTemplate": format!("{}/search/?q={{search_term_string}}", data.site_url),
            },
            "query-input": "required name=search_term_string",
        },
    })]));
    page
}

pub fn people_index(data: &SiteData) -> Page {
    let mut sorted: Vec<&Person> = data.people.iter().collect();
    sorted.sort_by(|a, b| pollywiki_schema::js_compare(&a.name, &b.name));

    let mut body = String::new();
    body.push_str("<div class=\"masthead\"><h1>People</h1><p class=\"lede\">");
    body.push_str(&format!(
        "{} sitting parliamentarians in the 48th Parliament.",
        data.people.len()
    ));
    body.push_str("</p></div>");

    body.push_str("<div class=\"filter-bar\"><span class=\"segmented\" id=\"house-filter\" role=\"group\" aria-label=\"Filter by chamber\"><button aria-pressed=\"true\" data-house=\"\">Both chambers</button><button aria-pressed=\"false\" data-house=\"representatives\">House</button><button aria-pressed=\"false\" data-house=\"senate\">Senate</button></span><span class=\"pills\" id=\"group-filter\" role=\"group\" aria-label=\"Filter by party\"><button aria-pressed=\"true\" data-group=\"\">All parties</button>");
    for p in &data.parties {
        body.push_str(&format!(
            "<button aria-pressed=\"false\" data-group=\"{}\" title=\"{}\">{}</button>",
            p.slug,
            esc_attr(&p.name),
            esc(p.code.as_deref().unwrap_or(&p.name)),
        ));
    }
    body.push_str("</span><input type=\"search\" id=\"people-filter\" placeholder=\"Filter by name, electorate or state\" aria-label=\"Filter people\"></div>");
    body.push_str(&filter_feedback("No one matches these filters."));

    body.push_str("<div class=\"person-grid\" id=\"person-grid\">");
    for person in sorted {
        body.push_str(&format!(
            "<div class=\"person-cell\" data-name=\"{}\" data-house=\"{}\" data-group=\"{}\">{}</div>",
            esc_attr(
                &format!(
                    "{} {} {}",
                    person.name,
                    person.electorate.as_deref().unwrap_or(""),
                    person.state.map(|s| s.as_str()).unwrap_or("")
                )
                .to_lowercase()
            ),
            person.house,
            person.group_slug,
            person_card(data, person),
        ));
    }
    body.push_str("</div>");

    let mut page = Page::new(
        "People",
        Some(
            "Every sitting member of the Australian House of Representatives and Senate."
                .to_string(),
        ),
        "/people/",
        body,
    );
    page.page_script = Some(PEOPLE_FILTER_JS);
    page
}

pub fn person_page(data: &SiteData, person: &Person) -> Page {
    let electorate = person
        .electorate
        .as_deref()
        .and_then(|slug| data.electorate_by_slug(slug));
    let seat = match person.house {
        House::Senate => format!(
            "Senator for {}",
            person
                .state
                .map(|s| state_name(s.as_str()).unwrap_or(s.as_str()))
                .unwrap_or("")
        ),
        House::Representatives => format!(
            "Member for {}",
            electorate.map(|e| e.name.as_str()).unwrap_or("")
        ),
    };
    let votes = data.votes_for_person(&person.slug);
    let stats = person.stats.as_ref();
    let service_start = person
        .background
        .as_ref()
        .and_then(|b| b.service_start.as_deref())
        .or(person.since.as_deref());
    let service_years = service_start.and_then(|start| {
        let start_ms = parse_js_date_millis(start)?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let years = (now_ms - start_ms) as f64 / 31_557_600_000.0;
        let fixed = to_fixed(years, 1);
        Some(fixed.strip_suffix(".0").unwrap_or(&fixed).to_string())
    });

    struct Raised<'b> {
        bill: &'b Bill,
        role: &'static str,
    }
    let raised: Vec<Raised> = data
        .bills
        .iter()
        .filter_map(|b| {
            if b.sponsors
                .iter()
                .any(|s| s.slug.as_deref() == Some(&person.slug))
            {
                Some(Raised {
                    bill: b,
                    role: "Sponsor",
                })
            } else if b
                .movers
                .iter()
                .any(|m| m.slug.as_deref() == Some(&person.slug))
            {
                Some(Raised {
                    bill: b,
                    role: "Introduced",
                })
            } else {
                None
            }
        })
        .collect();

    let mut body = String::new();
    body.push_str("<article data-pagefind-body>");
    body.push_str("<div class=\"profile-head\">");
    body.push_str(&avatar(person, true));
    body.push_str(
        "<div><div class=\"meta-row\" style=\"margin: 0 0 0.5rem;\"><span class=\"ids\">",
    );
    body.push_str(&chamber_chip(person.house));
    body.push_str(&format!(
        "<span data-pagefind-filter=\"chamber\">{}</span>",
        esc(&seat)
    ));
    if let Some(since) = &person.since {
        body.push_str(&format!(
            "<span>· since {}</span>",
            esc(&format_date(since))
        ));
    }
    body.push_str("</span></div>");
    body.push_str(&format!(
        "<h1 data-pagefind-meta=\"title\">{}</h1>",
        esc(&person.name)
    ));
    body.push_str("<div class=\"meta-line\" style=\"margin-top:.5rem\">");
    body.push_str(&group_chip(data, &person.group_slug, &person.group, true));
    if let Some(e) = electorate {
        body.push_str(&format!(
            "<a href=\"/electorates/{}/\">Electorate profile</a>",
            e.slug
        ));
    }
    if let Some(wikipedia) = &person.links.wikipedia {
        body.push_str(&format!(
            "<a href=\"{}\">Wikipedia</a>",
            esc_attr(wikipedia)
        ));
    }
    if person.ids.tvfy.is_some() {
        body.push_str(&format!(
            "<a href=\"https://theyvoteforyou.org.au/people/{}/{}\">They Vote For You</a>",
            person.house,
            person.electorate.as_deref().unwrap_or("")
        ));
    }
    body.push_str("</div></div></div>");

    if let Some(stats) = stats.filter(|s| s.divisions_eligible > 0) {
        body.push_str("<div class=\"stat-strip\">");
        body.push_str(&format!(
            "<div class=\"stat\"><span class=\"n\">{} / {}</span><span class=\"l\">divisions voted in this parliament</span></div>",
            stats.divisions_voted, stats.divisions_eligible
        ));
        body.push_str(&format!(
            "<div class=\"stat\"><span class=\"n\">{}</span><span class=\"l\">votes against their party's majority position</span></div>",
            stats.against_group_majority
        ));
        if let Some(years) = &service_years {
            body.push_str(&format!(
                "<div class=\"stat\"><span class=\"n\">{years}</span><span class=\"l\">years in federal parliament</span></div>"
            ));
        }
        body.push_str("</div>");
        body.push_str("<p class=\"note\">Absence from a division is not abstention: pairing arrangements, leave and parliamentary duties are not distinguished in the official record. <a href=\"/about/methodology/\">How these figures are computed.</a></p>");
    }

    if let Some(note) = &person.ai_note {
        body.push_str("<aside class=\"context-box\"><div class=\"context-top\"><span class=\"context-head\">Voting record in brief</span><span class=\"ai-tag\">AI-generated</span></div><div class=\"context-body\"><p>");
        body.push_str(&data.link_bill_titles(&note.text));
        body.push_str("</p></div><div class=\"context-credit\">Written by AI from the voting record below; describes subjects and votes, never merit. May contain errors — the record is authoritative. <a href=\"/about/methodology/\">How this works.</a></div></aside>");
    }

    if let Some(background) = &person.background {
        body.push_str(
            "<h2>Background</h2><div class=\"table-scroll\"><table class=\"facts\"><tbody>",
        );
        if let Some(born) = &background.born {
            body.push_str(&format!(
                "<tr><th class=\"label\" scope=\"row\">Born</th><td>{}{}</td></tr>",
                esc(&format_date(born)),
                match &background.birthplace {
                    Some(birthplace) => esc(&format!(", {birthplace}")),
                    None => String::new(),
                }
            ));
        }
        if let Some(start) = &background.service_start {
            body.push_str(&format!(
                "<tr><th class=\"label\" scope=\"row\">Entered parliament</th><td>{}</td></tr>",
                esc(&format_date(start))
            ));
        }
        if !background.parliaments.is_empty() {
            body.push_str(&format!(
                "<tr><th class=\"label\" scope=\"row\">Parliaments</th><td>{}</td></tr>",
                esc(&background
                    .parliaments
                    .iter()
                    .map(|n| format!("{n}th"))
                    .collect::<Vec<_>>()
                    .join(", "))
            ));
        }
        if !background.honours.is_empty() {
            body.push_str(&format!(
                "<tr><th class=\"label\" scope=\"row\">Honours</th><td>{}</td></tr>",
                esc(&background.honours.join("; "))
            ));
        }
        body.push_str("</tbody></table></div>");

        if !background.qualifications.is_empty() {
            body.push_str("<div class=\"table-scroll\"><table><thead><tr><th scope=\"col\">Qualification</th><th scope=\"col\">Institution</th></tr></thead><tbody>");
            for q in &background.qualifications {
                let parsed = parse_qualification(q);
                body.push_str(&format!(
                    "<tr><td>{}</td><td>{}</td></tr>",
                    esc(&parsed.qual),
                    esc(&parsed.institution)
                ));
            }
            body.push_str("</tbody></table></div>");
        }
        if !background.occupations.is_empty() {
            body.push_str("<h3 style=\"font-size:.95rem; margin-top:1.2rem;\">Occupations before parliament</h3><div class=\"table-scroll\"><table><thead><tr><th scope=\"col\">Role</th><th scope=\"col\">Organisation</th><th class=\"num\" scope=\"col\">Period</th></tr></thead><tbody>");
            for o in &background.occupations {
                match parse_occupation(o) {
                    Occupation::Raw(raw) => {
                        body.push_str(&format!("<tr><td colspan=\"3\">{}</td></tr>", esc(&raw)));
                    }
                    Occupation::Parsed { role, org, period } => {
                        body.push_str(&format!(
                            "<tr><td>{}</td><td>{}</td><td class=\"num\" style=\"white-space: nowrap;\">{}</td></tr>",
                            esc(&role),
                            esc(&org),
                            esc(&period)
                        ));
                    }
                }
            }
            body.push_str("</tbody></table></div>");
        }
    }

    if let Some(positions) = person.positions.as_deref().filter(|p| !p.is_empty()) {
        body.push_str("<h2>Positions held</h2><div class=\"table-scroll\"><table><thead><tr><th scope=\"col\">Role</th><th scope=\"col\">Ministry</th><th class=\"num\" scope=\"col\">From</th><th class=\"num\" scope=\"col\">To</th></tr></thead><tbody>");
        for pos in positions {
            body.push_str(&format!(
                "<tr><td>{}{}</td><td>{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
                esc(&pos.role),
                if pos.kind == pollywiki_schema::PositionKind::Shadow {
                    " (shadow)"
                } else {
                    ""
                },
                esc(pos.ministry.as_deref().unwrap_or("")),
                pos.from.as_deref().map(format_date).map(|d| esc(&d)).unwrap_or_default(),
                pos.to
                    .as_deref()
                    .map(format_date)
                    .map(|d| esc(&d))
                    .unwrap_or_else(|| "current".to_string()),
            ));
        }
        body.push_str("</tbody></table></div>");
    }

    if let Some(elections) = person.elections.as_deref().filter(|e| !e.is_empty()) {
        body.push_str("<h2>Election history</h2><div class=\"table-scroll\"><table><thead><tr><th scope=\"col\">Election</th><th scope=\"col\">Electorate</th><th scope=\"col\">Party</th><th class=\"num\" scope=\"col\">First pref %</th><th class=\"num\" scope=\"col\">Swing</th><th scope=\"col\">Result</th></tr></thead><tbody>");
        for e in elections {
            body.push_str(&format!(
                "<tr><td>{}</td><td><a href=\"/electorates/{}/\">{}</a></td><td>{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td>{}</td></tr>",
                esc(&e.event_name),
                e.electorate_slug,
                esc(&e.electorate_name),
                esc(&e.party),
                to_fixed(e.pct.0, 1),
                match e.swing {
                    Some(swing) => format!(
                        "{}{}",
                        if swing.0 > 0.0 { "+" } else { "" },
                        to_fixed(swing.0, 1)
                    ),
                    None => String::new(),
                },
                if e.elected { "Elected" } else { "Not elected" },
            ));
        }
        body.push_str("</tbody></table></div><p class=\"note\">House contests only; first preference share at each event.</p>");
    }

    if !raised.is_empty() {
        body.push_str("<h2>Bills raised</h2><div class=\"table-scroll\"><table><thead><tr><th scope=\"col\">Bill</th><th scope=\"col\">Role</th><th scope=\"col\">Status</th></tr></thead><tbody>");
        for r in &raised {
            body.push_str(&format!(
                "<tr><td><a href=\"/bills/{}/\">{}</a></td><td>{}</td><td>{}</td></tr>",
                r.bill.id,
                esc(&r.bill.title),
                r.role,
                esc(&r.bill.status),
            ));
        }
        body.push_str("</tbody></table></div><p class=\"note\">Sponsor marks the member's own bill; Introduced marks bills they moved, typically as the responsible minister.</p>");
    }

    body.push_str("<h2>Voting record</h2>");
    if !votes.is_empty() {
        body.push_str("<div class=\"table-scroll\"><table><thead><tr><th scope=\"col\">Date</th><th scope=\"col\">Division</th><th scope=\"col\">Vote</th><th scope=\"col\">Result</th></tr></thead><tbody>");
        for v in &votes {
            body.push_str(&format!(
                "<tr><td class=\"num\">{}</td><td><a href=\"/divisions/{}/{}/\">{}</a></td><td><span class=\"{}\">{}{}</span></td><td>{}</td></tr>",
                esc(&format_date(&v.division.date)),
                v.division.house,
                division_key(v.division),
                esc(&v.division.name),
                match (v.vote, v.against_group_majority) {
                    (Vote::Aye, true) => "vote-chip aye rebel",
                    (Vote::Aye, false) => "vote-chip aye",
                    (Vote::No, true) => "vote-chip no rebel",
                    (Vote::No, false) => "vote-chip no",
                },
                match v.vote {
                    Vote::Aye => "Aye",
                    Vote::No => "No",
                },
                if v.against_group_majority { " · crossed" } else { "" },
                result_chip(v.division.result),
            ));
        }
        body.push_str("</tbody></table></div>");
    } else {
        body.push_str("<p class=\"note\">No division votes recorded yet for this parliament. Voting records appear once the They Vote For You sync runs.</p>");
    }

    if let Some(photo) = &person.photo {
        body.push_str(&format!(
            "<p class=\"attribution\">Photo: {} ({}), via Wikimedia Commons.</p>",
            esc(&photo.attribution),
            esc(&photo.licence)
        ));
    }
    body.push_str(&format!(
        "<p class=\"attribution\">Sources: Wikidata{}{}{}. Errors? <a href=\"/about/corrections/\">Request a correction.</a></p>",
        if person.background.is_some() {
            ", Parliamentary Handbook"
        } else {
            ""
        },
        if person.elections.as_deref().is_some_and(|e| !e.is_empty()) {
            ", AEC (CC BY 4.0)"
        } else {
            ""
        },
        if !votes.is_empty() {
            ", They Vote For You (ODbL)"
        } else {
            ""
        },
    ));
    body.push_str("</article>");

    let description = format!("{seat}. Voting record and service details from official sources.");
    let path = format!("/people/{}/", person.slug);
    let mut person_ld = serde_json::json!({
        "@context": "https://schema.org",
        "@type": "Person",
        "name": person.name,
        "url": format!("{}{path}", data.site_url),
        "jobTitle": seat,
    });
    if let Some(photo) = &person.photo {
        person_ld["image"] = serde_json::json!(photo.url);
    }
    if let Some(party) = data.party_by_slug(&person.group_slug) {
        person_ld["memberOf"] = serde_json::json!({
            "@type": "Organization",
            "name": party.name,
            "url": format!("{}/parties/{}/", data.site_url, party.slug),
        });
    }
    if let Some(wikipedia) = &person.links.wikipedia {
        person_ld["sameAs"] = serde_json::json!(wikipedia);
    }
    let mut page = Page::new(person.name.clone(), Some(description), path.clone(), body);
    page.og_type = "profile";
    page.jsonld = Some(jsonld_script(vec![
        person_ld,
        breadcrumb(
            &data.site_url,
            &[
                ("pollywiki", "/"),
                ("People", "/people/"),
                (&person.name, &path),
            ],
        ),
    ]));
    page
}

pub fn divisions_index(data: &SiteData) -> Page {
    let mut body = String::new();
    body.push_str("<div class=\"masthead\"><h1>Divisions</h1><p class=\"lede\">");
    body.push_str(&format!(
        "A division is a formally recorded vote. {} recorded in the 48th Parliament so far, newest first.",
        data.divisions.len()
    ));
    body.push_str("</p></div>");
    body.push_str("<div class=\"filter-bar\"><span class=\"segmented\" id=\"division-house\" role=\"group\" aria-label=\"Filter by chamber\"><button aria-pressed=\"true\" data-house=\"\">Both chambers</button><button aria-pressed=\"false\" data-house=\"representatives\">House</button><button aria-pressed=\"false\" data-house=\"senate\">Senate</button></span><input type=\"search\" id=\"division-filter-text\" placeholder=\"Filter by division name\" aria-label=\"Filter divisions\"></div>");
    body.push_str(&filter_feedback("No divisions match these filters."));
    if !data.divisions.is_empty() {
        // Divisions arrive newest first, so each month is already one run.
        let mut months: Vec<(&str, Vec<&Division>)> = Vec::new();
        for d in &data.divisions {
            let key = d.date.get(..7).unwrap_or(d.date.as_str());
            if months.last().map(|(m, _)| *m) == Some(key) {
                months.last_mut().expect("just matched").1.push(d);
            } else {
                months.push((key, vec![d]));
            }
        }
        body.push_str("<ul class=\"ledger\" id=\"division-list\">");
        for (month, rows) in &months {
            body.push_str(&ledger_month(month, rows.len()));
            for d in rows {
                body.push_str(&ledger_row(d));
            }
        }
        body.push_str("</ul>");
    } else {
        body.push_str("<p class=\"note\">No division records loaded yet. They appear once the They Vote For You sync runs.</p>");
    }
    let mut page = Page::new(
        "Divisions",
        Some("Recorded votes of the Australian House of Representatives and Senate.".to_string()),
        "/divisions/",
        body,
    );
    page.page_script = Some(DIVISION_FILTER_JS);
    page
}

pub fn division_page(data: &SiteData, division: &Division) -> Page {
    let breakdown = data.group_breakdown(division);
    let ayes: Vec<_> = division
        .votes
        .iter()
        .filter(|v| v.vote == Vote::Aye)
        .collect();
    let noes: Vec<_> = division
        .votes
        .iter()
        .filter(|v| v.vote == Vote::No)
        .collect();
    let related: Vec<&Bill> = division
        .bill_ids
        .iter()
        .filter_map(|id| data.bill_by_id(id))
        .collect();
    let procedure = procedure_for(&division.name);
    let same_day = data.same_sitting_day(division);
    // The markdown renderer escapes raw HTML, so the output is safe to inline.
    let summary_html = match (&division.summary, division.summary_kind) {
        (Some(summary), kind) if kind != Some(SummaryKind::Transcript) => {
            Some(markdown::to_html(summary))
        }
        _ => None,
    };
    let name_for = |slug: &str| -> String {
        data.person_by_slug(slug)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| slug.replace('-', " "))
    };

    let mut body = String::new();
    body.push_str("<article data-pagefind-body>");
    body.push_str("<div class=\"meta-row\"><span class=\"ids\">");
    body.push_str(&chamber_chip(division.house));
    body.push_str(&format!(
        "Division {} · {}",
        division.number,
        esc(&format_date(&division.date))
    ));
    body.push_str("</span></div>");
    body.push_str(&format!(
        "<h1 class=\"{}\" data-pagefind-meta=\"title\">{}</h1>",
        title_tier(&division.name),
        esc(&division.name)
    ));
    body.push_str(&format!(
        "<div class=\"result-line\"><strong>{}</strong><span class=\"figures\">{} aye · {} no</span>{}</div>",
        result_word(division.result),
        division.ayes,
        division.noes,
        vote_bar(division.ayes, division.noes, true),
    ));

    let tvfy_link = division
        .links
        .tvfy
        .as_deref()
        .unwrap_or("https://theyvoteforyou.org.au");
    if let Some(html) = &summary_html {
        body.push_str("<aside class=\"context-box\"><div class=\"context-top\"><span class=\"context-head\">What this vote was about</span></div><div class=\"context-body\">");
        body.push_str(html);
        body.push_str(&format!(
            "</div><div class=\"context-credit\">Context written by <a href=\"{}\">They Vote For You</a> volunteers (ODbL), reproduced unchanged.</div></aside>",
            esc_attr(tvfy_link)
        ));
    }

    if let Some(ai) = &division.ai_summary {
        body.push_str("<aside class=\"context-box\"><div class=\"context-top\"><span class=\"context-head\">What this vote was about</span><span class=\"ai-tag\">AI-generated</span></div><div class=\"context-body\"><p>");
        body.push_str(&data.link_bill_titles(&ai.text));
        body.push_str(&format!(
            "</p></div><div class=\"context-credit\">Written by AI from the <a href=\"{}\">official record</a>; may contain errors — the record is authoritative. <a href=\"/about/methodology/\">How this works.</a></div></aside>",
            esc_attr(tvfy_link)
        ));
    }

    if let Some(procedure) = procedure {
        body.push_str(&format!(
            "<p class=\"procedure-note\"><strong>{}:</strong> {}</p>",
            esc(procedure.label),
            esc(procedure.text)
        ));
    }

    if !related.is_empty() {
        body.push_str("<h2>Related bills</h2><ul>");
        for bill in &related {
            body.push_str(&format!(
                "<li><a href=\"/bills/{}/\">{}</a></li>",
                bill.id,
                esc(&bill.title)
            ));
        }
        body.push_str("</ul>");
    }

    body.push_str("<h2>By party</h2><div class=\"table-scroll\"><table><thead><tr><th scope=\"col\">Party</th><th class=\"num\" scope=\"col\">Aye</th><th class=\"num\" scope=\"col\">No</th></tr></thead><tbody>");
    for row in &breakdown {
        body.push_str(&format!(
            "<tr><td><span class=\"group-chip\"><span class=\"dot\" style=\"background:{}\" aria-hidden=\"true\"></span>{}</span></td><td class=\"{}\">{}</td><td class=\"{}\">{}</td></tr>",
            row.party.and_then(|p| p.colour.as_deref()).unwrap_or("#6e7b74"),
            esc(row.party.map(|p| p.name.as_str()).unwrap_or(&row.group)),
            if row.aye == 0 { "num zero" } else { "num" },
            row.aye,
            if row.no == 0 { "num zero" } else { "num" },
            row.no,
        ));
    }
    body.push_str("</tbody></table></div>");

    body.push_str("<h2>Every vote</h2><div class=\"vote-columns\">");
    for (label, list) in [("AYE", &ayes), ("NO", &noes)] {
        body.push_str(&format!(
            "<div><div class=\"col-head\">{label} ({})</div><ul>",
            list.len()
        ));
        for v in list.iter() {
            body.push_str(&format!(
                "<li><a href=\"/people/{}/\">{}</a>{}</li>",
                v.person_slug,
                esc(&name_for(&v.person_slug)),
                if v.against_group_majority == Some(true) {
                    "<span class=\"note\"> · crossed</span>"
                } else {
                    ""
                },
            ));
        }
        body.push_str("</ul></div>");
    }
    body.push_str("</div>");

    if !same_day.is_empty() {
        body.push_str(&format!(
            "<h2>Other divisions this sitting day</h2><p class=\"note\">What the {} divided on around this vote, in order.</p><ul class=\"ledger\">",
            chamber_word(division.house)
        ));
        for d in &same_day {
            body.push_str(&ledger_row(d));
        }
        body.push_str("</ul>");
    }
    body.push_str("</article>");

    let mut footer_note = String::from("<p class=\"attribution\" style=\"margin: 0 0 0.9rem;\">");
    if let Some(tvfy) = &division.links.tvfy {
        footer_note.push_str(&format!(
            "Record via <a href=\"{}\">They Vote For You</a> (ODbL). ",
            esc_attr(tvfy)
        ));
    }
    if let Some(hansard) = &division.links.hansard {
        footer_note.push_str(&format!(
            "Official transcript: <a href=\"{}\">Hansard</a>. ",
            esc_attr(hansard)
        ));
    }
    footer_note.push_str("\"Crossed\" marks a vote against the majority of the member's own party in this division.</p>");

    let description = format!(
        "Division in the {}, {}: {} ayes, {} noes.",
        full_chamber(division.house),
        format_date(&division.date),
        division.ayes,
        division.noes
    );
    let path = format!("/divisions/{}/{}/", division.house, division_key(division));
    let mut page = Page::new(division.name.clone(), Some(description), path.clone(), body);
    page.footer_note = Some(footer_note);
    page.og_type = "article";
    page.lastmod = Some(division.date.clone());
    page.jsonld = Some(jsonld_script(vec![breadcrumb(
        &data.site_url,
        &[
            ("pollywiki", "/"),
            ("Divisions", "/divisions/"),
            (&division.name, &path),
        ],
    )]));
    page
}

pub fn bills_index(data: &SiteData) -> Page {
    let mut body = String::new();
    body.push_str("<div class=\"masthead\"><h1>Bills</h1><p class=\"lede\">");
    body.push_str(&format!(
        "{} bills of the 48th Parliament, from the official APH record.",
        data.bills.len()
    ));
    body.push_str("</p></div>");
    body.push_str("<div class=\"filter-bar\"><span class=\"pills\" id=\"bill-status\" role=\"group\" aria-label=\"Filter by status\"><button aria-pressed=\"true\" data-status=\"\">All</button><button aria-pressed=\"false\" data-status=\"open\">Before parliament</button><button aria-pressed=\"false\" data-status=\"act\">Act</button><button aria-pressed=\"false\" data-status=\"other\">Other</button></span><input type=\"search\" id=\"bill-filter\" placeholder=\"Filter by title or portfolio\" aria-label=\"Filter bills\"></div>");
    body.push_str(&filter_feedback("No bills match these filters."));
    if !data.bills.is_empty() {
        body.push_str("<ul class=\"bill-list\" id=\"bill-rows\">");
        for b in &data.bills {
            body.push_str(&bill_row(b, true));
        }
        body.push_str("</ul>");
        body.push_str(BILL_DOTS_LEGEND);
    } else {
        body.push_str("<p class=\"note\">No bill records loaded yet. They appear once the APH bills sync runs.</p>");
    }
    let mut page = Page::new(
        "Bills",
        Some("Bills before the Australian federal parliament and their progress.".to_string()),
        "/bills/",
        body,
    );
    page.page_script = Some(BILL_FILTER_JS);
    page
}

static LEADING_AND_SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^and ").unwrap());
static SUMMARY_SPLIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r";\s+").unwrap());

pub fn bill_page(data: &SiteData, bill: &Bill) -> Page {
    let related: Vec<&Division> = bill
        .division_ids
        .iter()
        .filter_map(|id| data.division_by_id(id))
        .collect();

    // Official summaries are semicolon-delimited lists of amendments. Multi-act
    // bills nest items under act headings; single-act bills render as a flat
    // list when long enough to be unreadable as a paragraph.
    let summary_groups = bill.summary.as_deref().and_then(parse_bill_summary);
    let summary_parts: Vec<&str> = SUMMARY_SPLIT
        .split(bill.summary.as_deref().unwrap_or(""))
        .filter(|s| !s.trim().is_empty())
        .collect();
    let summary_as_list = summary_groups.is_none() && summary_parts.len() >= 4;
    let lead_colon = summary_parts
        .first()
        .and_then(|p| p.rfind(": "))
        .map(|i| i as isize)
        .unwrap_or(-1);
    let summary_lead = summary_parts.first().map(|p| {
        if lead_colon > 0 {
            &p[..(lead_colon as usize + 1)]
        } else {
            *p
        }
    });
    let summary_items: Vec<String> = if summary_as_list {
        let mut items: Vec<String> = Vec::new();
        if lead_colon > 0 {
            if let Some(first) = summary_parts.first() {
                items.push(first[(lead_colon as usize + 2)..].to_string());
            }
        }
        items.extend(summary_parts.iter().skip(1).map(|p| p.to_string()));
        items
            .into_iter()
            .map(|part| LEADING_AND_SPACE.replace(&part, "").into_owned())
            .collect()
    } else {
        Vec::new()
    };

    let mut body = String::new();
    body.push_str("<article data-pagefind-body>");
    body.push_str("<div class=\"meta-row\"><span class=\"ids\">");
    body.push_str(&chamber_chip(bill.chamber));
    body.push_str(&esc(&bill.id));
    body.push_str(
        "</span><span class=\"status\"><span class=\"label\">Status</span><span class=\"value\">",
    );
    body.push_str(&esc(&bill.status));
    body.push_str("</span>");
    body.push_str(&bill_dots(bill));
    body.push_str("</span></div>");
    body.push_str(&format!(
        "<h1 class=\"{}\" data-pagefind-meta=\"title\">{}</h1>",
        title_tier(&bill.title),
        esc(&bill.title)
    ));

    let has_meta_line = bill.bill_type.is_some()
        || bill.portfolio.is_some()
        || !bill.sponsors.is_empty()
        || !bill.movers.is_empty();
    if has_meta_line {
        body.push_str("<div class=\"meta-line\" style=\"margin-top:.4rem\">");
        if let Some(t) = &bill.bill_type {
            body.push_str(&format!("<span>{}</span>", esc(t)));
        }
        if let Some(portfolio) = &bill.portfolio {
            body.push_str(&format!(
                "<span><strong>Portfolio:</strong> {}</span>",
                esc(portfolio)
            ));
        }
        let raiser_links = |raisers: &[pollywiki_schema::BillRaiser]| -> String {
            raisers
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    let name = match &r.slug {
                        Some(slug) => format!("<a href=\"/people/{slug}/\">{}</a>", esc(&r.name)),
                        None => esc(&r.name),
                    };
                    if i > 0 {
                        format!(", {name}")
                    } else {
                        name
                    }
                })
                .collect()
        };
        if !bill.sponsors.is_empty() {
            body.push_str(&format!(
                "<span><strong>Sponsor{}:</strong> {}</span>",
                if bill.sponsors.len() > 1 { "s" } else { "" },
                raiser_links(&bill.sponsors)
            ));
        } else if !bill.movers.is_empty() {
            body.push_str(&format!(
                "<span><strong>Introduced by</strong> {}</span>",
                raiser_links(&bill.movers)
            ));
        }
        body.push_str("</div>");
    }

    if bill.summary.is_some() {
        if let Some(groups) = &summary_groups {
            body.push_str(
                "<div class=\"lede\" style=\"max-width: 66ch; margin-top: 1.1rem;\"><p>Amends:</p>",
            );
            for group in groups {
                body.push_str(&format!(
                    "<div style=\"margin: .7rem 0;\"><p style=\"font-weight: 600; margin-bottom: .15rem;\">{}</p>",
                    esc(&group.acts)
                ));
                if !group.items.is_empty() {
                    body.push_str("<ul style=\"margin: .2rem 0; padding-left: 1.4rem;\">");
                    for item in &group.items {
                        body.push_str(&format!("<li>{}</li>", esc(item)));
                    }
                    body.push_str("</ul>");
                }
                body.push_str("</div>");
            }
            body.push_str("<p class=\"note\">Summary from the official bill homepage.</p></div>");
        } else if !summary_as_list {
            body.push_str(&format!(
                "<div style=\"margin-top: 1.1rem;\"><p class=\"lede\" style=\"max-width: 66ch;\">{}</p><p class=\"note\">Summary from the official bill homepage.</p></div>",
                esc(bill.summary.as_deref().unwrap_or(""))
            ));
        } else {
            body.push_str("<div class=\"lede\" style=\"max-width: 66ch; margin-top: 1.1rem;\"><p>");
            body.push_str(&esc(summary_lead.unwrap_or("")));
            body.push_str("</p><ul style=\"margin: .4rem 0; padding-left: 1.4rem;\">");
            for part in &summary_items {
                body.push_str(&format!("<li>{}</li>", esc(part)));
            }
            body.push_str(
                "</ul><p class=\"note\">Summary from the official bill homepage.</p></div>",
            );
        }
    }

    if let Some(ai) = &bill.ai_summary {
        body.push_str(&format!(
            "<aside class=\"context-box\"><div class=\"context-top\"><span class=\"context-head\">What this bill is about</span><span class=\"ai-tag\">AI-generated</span></div><div class=\"context-body\"><p>{}</p></div><div class=\"context-credit\">Written by AI to explain the official summary in plain terms; specifics come from that summary only. May contain errors — the record is authoritative. <a href=\"/about/methodology/\">How this works.</a></div></aside>",
            esc(&ai.text)
        ));
    }

    if !bill.timeline.is_empty() {
        body.push_str("<h2>Progress</h2><div class=\"table-scroll\"><table><tbody>");
        for step in &bill.timeline {
            body.push_str(&format!(
                "<tr><td class=\"num\">{}</td><td>{}</td></tr>",
                esc(&format_date(&step.date)),
                esc(&step.event)
            ));
        }
        body.push_str("</tbody></table></div>");
    }

    if !related.is_empty() {
        body.push_str("<h2>Divisions on this bill</h2><ul class=\"ledger\">");
        for d in &related {
            body.push_str(&ledger_row(d));
        }
        body.push_str("</ul>");
    }

    body.push_str("<p class=\"attribution\">");
    match &bill.links.aph {
        Some(aph) => {
            body.push_str(&format!(
                "Official record: <a href=\"{}\">bill homepage at aph.gov.au</a>",
                esc_attr(aph)
            ));
            if let Some(parlinfo) = &bill.links.parlinfo {
                body.push_str(&format!(
                    ", <a href=\"{}\">full text and documents on ParlInfo</a>",
                    esc_attr(parlinfo)
                ));
            }
            body.push('.');
        }
        None => body.push_str("Source: Parliament of Australia."),
    }
    body.push_str("</p></article>");

    let description = format!(
        "{}. Progress of this bill through the federal parliament.",
        bill.status
    );
    let path = format!("/bills/{}/", bill.id);
    let mut legislation = serde_json::json!({
        "@context": "https://schema.org",
        "@type": "Legislation",
        "name": bill.title,
        "legislationStatus": bill.status,
        "url": bill
            .links
            .aph
            .clone()
            .unwrap_or_else(|| format!("{}{path}", data.site_url)),
    });
    if let Some(bill_type) = &bill.bill_type {
        legislation["legislationType"] = serde_json::json!(bill_type);
    }
    let mut page = Page::new(bill.title.clone(), Some(description), path.clone(), body);
    page.og_type = "article";
    page.lastmod = latest_step(bill).map(|step| step.date.clone());
    page.jsonld = Some(jsonld_script(vec![
        legislation,
        breadcrumb(
            &data.site_url,
            &[
                ("pollywiki", "/"),
                ("Bills", "/bills/"),
                (&bill.title, &path),
            ],
        ),
    ]));
    page
}

pub fn electorates_index(data: &SiteData) -> Page {
    let mut sorted: Vec<&Electorate> = data.electorates.iter().collect();
    sorted.sort_by(|a, b| pollywiki_schema::js_compare(&a.name, &b.name));

    let mut body = String::new();
    body.push_str("<h1>Electorates</h1><p>");
    body.push_str(&format!(
        "{} House of Representatives seats.",
        data.electorates.len()
    ));
    body.push_str("</p>");
    body.push_str("<div class=\"filter-bar\"><input type=\"search\" id=\"electorate-filter\" placeholder=\"Filter by name or state\" aria-label=\"Filter electorates\"></div>");
    body.push_str(&filter_feedback("No electorates match these filters."));
    body.push_str("<div class=\"table-scroll\"><table id=\"electorate-table\"><thead><tr><th scope=\"col\">Electorate</th><th scope=\"col\">State</th><th scope=\"col\">Member</th></tr></thead><tbody>");
    for e in sorted {
        let member = e
            .member_slug
            .as_deref()
            .and_then(|slug| data.person_by_slug(slug));
        body.push_str(&format!(
            "<tr data-text=\"{}\"><td><a href=\"/electorates/{}/\">{}</a></td><td>{}</td><td>{}</td></tr>",
            esc_attr(
                &format!(
                    "{} {} {} {}",
                    e.name,
                    e.state,
                    state_name(e.state.as_str()).unwrap_or(""),
                    member.map(|m| m.name.as_str()).unwrap_or("")
                )
                .to_lowercase()
            ),
            e.slug,
            esc(&e.name),
            e.state,
            match member {
                Some(m) => format!("<a href=\"/people/{}/\">{}</a>", m.slug, esc(&m.name)),
                None => String::new(),
            },
        ));
    }
    body.push_str("</tbody></table></div>");

    let mut page = Page::new(
        "Electorates",
        Some("All 150 federal electorates and their members.".to_string()),
        "/electorates/",
        body,
    );
    page.page_script = Some(ELECTORATE_FILTER_JS);
    page
}

pub fn electorate_page(data: &SiteData, electorate: &Electorate) -> Page {
    let member = electorate
        .member_slug
        .as_deref()
        .and_then(|slug| data.person_by_slug(slug));
    let result = data.election_for_electorate(&electorate.slug);
    let tcp = result.map(|r| r.tcp.as_slice()).unwrap_or(&[]);
    let tcp_total: i64 = tcp.iter().map(|c| c.votes).sum();

    let mut body = String::new();
    body.push_str("<article data-pagefind-body>");
    body.push_str(&format!(
        "<div class=\"masthead\" style=\"padding: 2.2rem 0 1.6rem;\"><h1 data-pagefind-meta=\"title\">{}</h1><p class=\"motto\">Federal electorate · {}</p></div>",
        esc(&electorate.name),
        esc(state_name(electorate.state.as_str()).unwrap_or(electorate.state.as_str())),
    ));

    let profile = electorate.profile.as_ref();
    if let Some(derivation) = profile.and_then(|p| p.name_derivation.as_deref()) {
        body.push_str(&format!(
            "<p class=\"lede\" style=\"margin-top: -0.4rem;\">{}</p>",
            esc(derivation)
        ));
    }
    if let Some(location) = profile.and_then(|p| p.location.as_deref()) {
        body.push_str(&format!(
            "<p class=\"note\" style=\"max-width: 62ch;\"><strong>Covers:</strong> {}</p>",
            esc(location)
        ));
    }
    if profile.is_some() || electorate.enrolment.is_some() {
        body.push_str("<div class=\"table-scroll\"><table class=\"facts\"><tbody>");
        if let Some(enrolment) = electorate.enrolment {
            body.push_str(&format!(
                "<tr><th class=\"label\" scope=\"row\">Enrolled voters</th><td>{}</td></tr>",
                locale_int(enrolment)
            ));
        }
        if let Some(area) = profile.and_then(|p| p.area.as_deref()) {
            body.push_str(&format!(
                "<tr><th class=\"label\" scope=\"row\">Area</th><td>{}</td></tr>",
                esc(area)
            ));
        }
        if let Some(demographic) = profile.and_then(|p| p.demographic.as_deref()) {
            body.push_str(&format!(
                "<tr><th class=\"label\" scope=\"row\">Demographic rating</th><td>{}</td></tr>",
                esc(demographic)
            ));
        }
        if let Some(first) = profile.and_then(|p| p.first_contested.as_deref()) {
            body.push_str(&format!(
                "<tr><th class=\"label\" scope=\"row\">Name first used</th><td>{}</td></tr>",
                esc(first)
            ));
        }
        if let Some(gazetted) = profile.and_then(|p| p.gazetted.as_deref()) {
            body.push_str(&format!(
                "<tr><th class=\"label\" scope=\"row\">Boundary gazetted</th><td>{}</td></tr>",
                esc(gazetted)
            ));
        }
        body.push_str("</tbody></table></div>");
    }

    if let Some(member) = member {
        body.push_str("<h2>Current member</h2><div style=\"max-width: 24rem\">");
        body.push_str(&person_card(data, member));
        body.push_str("</div>");
    }

    if let Some(result) = result {
        if tcp.len() == 2 {
            body.push_str(&format!(
                "<h2>{}: two-candidate preferred</h2><div class=\"table-scroll\"><table><tbody>",
                esc(&result.event_name)
            ));
            for c in tcp {
                body.push_str(&format!(
                    "<tr><td>{}{}</td><td>{}</td><td class=\"num\">{}</td><td class=\"num\">{}%</td></tr>",
                    esc(&c.name),
                    if c.elected { " ✓" } else { "" },
                    esc(&c.party),
                    locale_int(c.votes),
                    if tcp_total > 0 {
                        to_fixed(c.votes as f64 / tcp_total as f64 * 100.0, 2)
                    } else {
                        "0".to_string()
                    },
                ));
            }
            body.push_str("</tbody></table></div>");
        }
        if !result.first_prefs.is_empty() {
            body.push_str(&format!(
                "<h2>{}: first preferences</h2><div class=\"table-scroll\"><table><thead><tr><th scope=\"col\">Candidate</th><th scope=\"col\">Party</th><th class=\"num\" scope=\"col\">Votes</th><th class=\"num\" scope=\"col\">%</th><th class=\"num\" scope=\"col\">Swing</th></tr></thead><tbody>",
                esc(&result.event_name)
            ));
            for c in &result.first_prefs {
                body.push_str(&format!(
                    "<tr><td>{}{}</td><td>{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
                    esc(&c.name),
                    if c.elected { " ✓" } else { "" },
                    esc(&c.party),
                    locale_int(c.votes),
                    to_fixed(c.pct.0, 2),
                    match c.swing {
                        Some(swing) => format!(
                            "{}{}",
                            if swing.0 > 0.0 { "+" } else { "" },
                            to_fixed(swing.0, 2)
                        ),
                        None => String::new(),
                    },
                ));
            }
            body.push_str("</tbody></table></div>");
        }
    }

    body.push_str("<p class=\"attribution\">Election figures and profile © Commonwealth of Australia (AEC), CC BY 4.0. ✓ marks the elected candidate.</p>");
    body.push_str("</article>");

    let title = format!("{} ({})", electorate.name, electorate.state);
    let description = format!(
        "Federal electorate of {}, {}: member and election results.",
        electorate.name,
        state_name(electorate.state.as_str()).unwrap_or("undefined")
    );
    let path = format!("/electorates/{}/", electorate.slug);
    let mut page = Page::new(title, Some(description), path.clone(), body);
    page.jsonld = Some(jsonld_script(vec![breadcrumb(
        &data.site_url,
        &[
            ("pollywiki", "/"),
            ("Electorates", "/electorates/"),
            (&electorate.name, &path),
        ],
    )]));
    page
}

pub fn parties_index(data: &SiteData) -> Page {
    let mut body = String::new();
    body.push_str("<h1>Parties</h1><p>Parliamentary groups of the 48th Parliament. Grouping follows the official record; Coalition members sit as one parliamentary group.</p>");
    body.push_str("<div class=\"table-scroll\"><table><thead><tr><th scope=\"col\">Group</th><th class=\"num\" scope=\"col\">House</th><th class=\"num\" scope=\"col\">Senate</th><th class=\"num\" scope=\"col\">Total</th></tr></thead><tbody>");
    for p in &data.parties {
        body.push_str(&format!(
            "<tr><td><a class=\"group-chip\" href=\"/parties/{}/\"><span class=\"dot\" style=\"background:{}\" aria-hidden=\"true\"></span>{}</a></td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
            p.slug,
            p.colour.as_deref().unwrap_or("#6e7b74"),
            esc(&p.name),
            p.seats.as_ref().map(|s| s.representatives).unwrap_or(0),
            p.seats.as_ref().map(|s| s.senate).unwrap_or(0),
            data::seat_total(p),
        ));
    }
    body.push_str("</tbody></table></div>");
    Page::new(
        "Parties",
        Some("Parliamentary groups of the 48th Parliament and their seat counts.".to_string()),
        "/parties/",
        body,
    )
}

static LEADERSHIP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(prime minister|^leader|^deputy leader|^manager of (government|opposition) business|whip|president of the senate|^speaker|^deputy speaker)").unwrap()
});
static WEBSITE_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^https?://(www\.)?").unwrap());

pub fn party_page(data: &SiteData, party: &Party) -> Page {
    let members = data.members_of_party(&party.slug);
    let mut reps: Vec<&&Person> = members
        .iter()
        .filter(|m| m.house == House::Representatives)
        .collect();
    reps.sort_by(|a, b| pollywiki_schema::js_compare(&a.name, &b.name));
    let mut senators: Vec<&&Person> = members
        .iter()
        .filter(|m| m.house == House::Senate)
        .collect();
    senators.sort_by(|a, b| pollywiki_schema::js_compare(&a.name, &b.name));

    // Current parliamentary leadership roles held by this group's members,
    // straight from the Handbook position records.
    struct Leader<'b> {
        person: &'b Person,
        role: &'b str,
        from: Option<&'b str>,
    }
    let mut leadership: Vec<Leader> = Vec::new();
    for m in &members {
        for p in m.positions.as_deref().unwrap_or_default() {
            if p.to.is_none() && LEADERSHIP.is_match(&p.role) {
                // The Handbook records one entry per ministry, so a continuing
                // role spans several. This table names the role, not the
                // ministry, so keep one row per person and role, dated from the
                // earliest of them.
                let existing = leadership
                    .iter_mut()
                    .find(|l| l.person.slug == m.slug && l.role == p.role);
                match existing {
                    Some(l) => {
                        if let Some(from) = p.from.as_deref() {
                            if l.from.is_none_or(|held| from < held) {
                                l.from = Some(from);
                            }
                        }
                    }
                    None => leadership.push(Leader {
                        person: m,
                        role: &p.role,
                        from: p.from.as_deref(),
                    }),
                }
            }
        }
    }
    leadership.sort_by(|a, b| {
        pollywiki_schema::js_compare(a.role, b.role)
            .then_with(|| pollywiki_schema::js_compare(&a.person.name, &b.person.name))
    });

    let reps_seats = party.seats.as_ref().map(|s| s.representatives).unwrap_or(0);
    let senate_seats = party.seats.as_ref().map(|s| s.senate).unwrap_or(0);

    let mut body = String::new();
    body.push_str("<article data-pagefind-body>");
    body.push_str(&format!(
        "<div class=\"masthead\" style=\"padding: 2.2rem 0 1.6rem;\"><h1 data-pagefind-meta=\"title\"><span class=\"group-chip\" style=\"font-size: inherit; gap: .7rem;\"><span class=\"dot\" style=\"background:{}; width:.9rem; height:.9rem;\" aria-hidden=\"true\"></span>{}</span></h1><p class=\"motto\">{} House {} ·{} Senate {}</p></div>",
        party.colour.as_deref().unwrap_or("#6e7b74"),
        esc(&party.name),
        reps_seats,
        if reps_seats == 1 { "seat" } else { "seats" },
        senate_seats,
        if senate_seats == 1 { "seat" } else { "seats" },
    ));

    if let Some(facts) = party
        .facts
        .as_ref()
        .filter(|f| f.website.is_some() || f.wikipedia.is_some())
    {
        body.push_str("<div class=\"table-scroll\"><table class=\"facts\"><tbody>");
        if let Some(website) = &facts.website {
            let label = WEBSITE_PREFIX.replace(website, "");
            let label = label.strip_suffix('/').unwrap_or(&label);
            body.push_str(&format!(
                "<tr><th class=\"label\" scope=\"row\">Website</th><td><a href=\"{}\">{}</a></td></tr>",
                esc_attr(website),
                esc(label)
            ));
        }
        if let Some(wikipedia) = &facts.wikipedia {
            let article = wikipedia
                .split_once("/wiki/")
                .map(|x| x.1)
                .map(data::decode_uri_component)
                .unwrap_or_else(|| "article".to_string())
                .replace('_', " ");
            body.push_str(&format!(
                "<tr><th class=\"label\" scope=\"row\">Wikipedia</th><td><a href=\"{}\">{}</a></td></tr>",
                esc_attr(wikipedia),
                esc(&article)
            ));
        }
        body.push_str("</tbody></table></div>");
    }

    if !leadership.is_empty() {
        body.push_str("<h2>Parliamentary leadership</h2><div class=\"table-scroll\"><table><thead><tr><th scope=\"col\">Role</th><th scope=\"col\">Member</th><th class=\"num\" scope=\"col\">Since</th></tr></thead><tbody>");
        for l in &leadership {
            body.push_str(&format!(
                "<tr><td>{}</td><td><a href=\"/people/{}/\">{}</a></td><td class=\"num\">{}</td></tr>",
                esc(l.role),
                l.person.slug,
                esc(&l.person.name),
                l.from.map(format_date).map(|d| esc(&d)).unwrap_or_default(),
            ));
        }
        body.push_str("</tbody></table></div>");
    }

    if !reps.is_empty() {
        body.push_str("<h2>House of Representatives</h2><div class=\"person-grid\">");
        for p in &reps {
            body.push_str(&person_card(data, p));
        }
        body.push_str("</div>");
    }
    if !senators.is_empty() {
        body.push_str("<h2>Senate</h2><div class=\"person-grid\">");
        for p in &senators {
            body.push_str(&person_card(data, p));
        }
        body.push_str("</div>");
    }
    body.push_str("</article>");

    let description = format!("{}: members in the 48th federal parliament.", party.name);
    let path = format!("/parties/{}/", party.slug);
    let mut page = Page::new(party.name.clone(), Some(description), path.clone(), body);
    page.jsonld = Some(jsonld_script(vec![breadcrumb(
        &data.site_url,
        &[
            ("pollywiki", "/"),
            ("Parties", "/parties/"),
            (&party.name, &path),
        ],
    )]));
    page
}

pub fn search_page() -> Page {
    // Pagefind renders its own input, so a ?q= term has to be poured in after
    // init and announced with an input event for the UI to run the query.
    const SEARCH_INLINE_JS: &str = "\n    window.addEventListener('DOMContentLoaded', () => {\n      if (typeof PagefindUI !== 'undefined') {\n        new PagefindUI({ element: '#search', showSubResults: false, showImages: false })\n        const input = document.querySelector('#search input')\n        if (input) {\n          const q = new URLSearchParams(location.search).get('q')\n          if (q) {\n            input.value = q\n            input.dispatchEvent(new Event('input', { bubbles: true }))\n          }\n          input.focus()\n        }\n      } else {\n        document.getElementById('search').textContent =\n          'Search index not built. Run the full build to generate it.'\n      }\n    })\n  ";
    let mut body = String::new();
    body.push_str("<h1>Search the record</h1><p class=\"note\">People, divisions, bills, electorates and parties.</p>");
    body.push_str("<link href=\"/pagefind/pagefind-ui.css\" rel=\"stylesheet\"><script src=\"/pagefind/pagefind-ui.js\"></script><div id=\"search\" style=\"margin-top: 1.4rem;\"></div><script>");
    body.push_str(SEARCH_INLINE_JS);
    body.push_str("</script>");
    Page::new(
        "Search",
        Some(
            "Search people, divisions, bills and electorates across the federal record."
                .to_string(),
        ),
        "/search/",
        body,
    )
}

pub fn not_found() -> Page {
    let body = "<div class=\"masthead\"><h1>Not on the record.</h1><p class=\"motto\">This page doesn't exist. Try <a href=\"/search/\">searching the record</a> or start from the <a href=\"/\">front page</a>.</p></div>".to_string();
    Page::new("Page not found", None, "/404/", body)
}

pub fn about_index() -> Page {
    let body = concat!(
        "<h1>About pollywiki</h1>",
        "<p>pollywiki is a public register of Australia's federal parliament: the people who sit in it, the divisions they voted in, the bills before it, and the results of the elections that put them there.</p>",
        "<p><strong>This service does not evaluate politicians or laws.</strong> There are no scores, rankings, endorsements or opinions here. Every page is generated automatically from official public records, reproduced faithfully, and every figure links back to its source. Where a number needs interpretation (like attendance), the limits of the official record are stated next to it.</p>",
        "<h2>What powers it</h2>",
        "<p>Data comes from the Parliament of Australia, the Australian Electoral Commission, the OpenAustralia Foundation's <a href=\"https://theyvoteforyou.org.au\">They Vote For You</a>, and Wikidata. Full details, licences and sync times are on the <a href=\"/about/data-sources/\">data sources</a> page. The site rebuilds automatically after each sync.</p>",
        "<h2>Who runs it</h2>",
        "<p>pollywiki is an independent, non-commercial project run by <a href=\"https://han.life\">Logan Han</a>. It is not affiliated with the Parliament of Australia, the AEC, any party or any candidate. The <a href=\"https://github.com/logan-han/pollywiki\">source code is open</a>.</p>",
        "<p>Found something wrong? <a href=\"/about/corrections/\">Request a correction.</a></p>",
    )
    .to_string();
    Page::new(
        "About",
        Some("What pollywiki is, what it is not, and how it works.".to_string()),
        "/about/",
        body,
    )
}

pub fn data_sources(data: &SiteData) -> Page {
    struct Source {
        key: &'static str,
        name: &'static str,
        what: &'static str,
        licence: &'static str,
        link: &'static str,
    }
    const SOURCES: [Source; 5] = [
        Source {
            key: "wikidata",
            name: "Wikidata & Wikimedia Commons",
            what: "Current members of both chambers, parliamentary groups, seats, photos.",
            licence: "CC0 (data); photos individually licensed and credited on each page",
            link: "https://www.wikidata.org",
        },
        Source {
            key: "tvfy",
            name: "They Vote For You (OpenAustralia Foundation)",
            what: "Divisions and every individual vote, derived from the official Hansard.",
            licence: "Open Database Licence (ODbL) 1.0",
            link: "https://theyvoteforyou.org.au",
        },
        Source {
            key: "aph-bills",
            name: "Parliament of Australia",
            what: "Bills before parliament and their progress.",
            licence: "Commonwealth of Australia; reproduced fairly and accurately with acknowledgement",
            link: "https://www.aph.gov.au",
        },
        Source {
            key: "aec",
            name: "Australian Electoral Commission",
            what: "Federal election results by electorate, 2019 onwards including by-elections.",
            licence: "CC BY 4.0 © Commonwealth of Australia",
            link: "https://results.aec.gov.au",
        },
        Source {
            key: "handbook",
            name: "Parliamentary Handbook",
            what: "Careers before parliament, qualifications, ministries, shadow ministries and positions held.",
            licence: "Commonwealth of Australia; reproduced fairly and accurately with acknowledgement",
            link: "https://handbook.aph.gov.au",
        },
    ];

    let mut body = String::new();
    body.push_str("<h1>Data sources</h1><p>Everything on this site is generated from the sources below. Nothing is written by hand and nothing is edited after ingestion.</p>");
    body.push_str("<div class=\"table-scroll\"><table><thead><tr><th scope=\"col\">Source</th><th scope=\"col\">Used for</th><th scope=\"col\">Licence</th><th scope=\"col\">Last synced</th></tr></thead><tbody>");
    for s in &SOURCES {
        let status = data.meta.sources.get(s.key);
        body.push_str(&format!(
            "<tr><td><a href=\"{}\">{}</a></td><td>{}</td><td>{}</td><td class=\"num\">{}</td></tr>",
            s.link,
            esc(s.name),
            s.what,
            s.licence,
            match status {
                Some(status) => format!(
                    "{}{}",
                    format_date(&status.last_sync.chars().take(10).collect::<String>()),
                    if status.ok { "" } else { " (failed)" }
                ),
                None => "not yet".to_string(),
            },
        ));
    }
    body.push_str("</tbody></table></div>");
    body.push_str("<h2>Attribution and reuse</h2><p>Voting data is used under the ODbL: it is attributed on every page where it appears and derived data remains open. Election figures are © Commonwealth of Australia (AEC), CC BY 4.0, and this site does not use AEC branding. Parliamentary material is reproduced fairly and accurately with acknowledgement and without any implication of parliamentary endorsement. Photos come only from Wikimedia Commons under free licences, with the photographer credited on the page where the photo appears.</p>");
    body.push_str("<p>The site's own code is <a href=\"https://github.com/logan-han/pollywiki\">open source on GitHub</a>.</p>");
    Page::new(
        "Data sources",
        Some(
            "Where every figure on pollywiki comes from, its licence and when it last synced."
                .to_string(),
        ),
        "/about/data-sources/",
        body,
    )
}

pub fn methodology() -> Page {
    let body = concat!(
        "<h1>Methodology</h1>",
        "<p>pollywiki publishes official records verbatim plus simple arithmetic. This page defines every derived figure that appears on the site.</p>",
        "<h2>Divisions voted</h2>",
        "<p>\"Voted in <em>n</em> of <em>m</em> divisions\" counts the divisions in the member's own chamber during this parliament (<em>m</em>) and the divisions where their name appears in the ayes or noes (<em>n</em>).</p>",
        "<p><strong>Absence is not abstention.</strong> The official record does not distinguish pairing arrangements, approved leave, ministerial or committee duties, or illness from any other reason for not voting. A low attendance figure is not, by itself, evidence of anything. This caveat is shown wherever the figure appears.</p>",
        "<h2>Votes against party majority (\"crossed\")</h2>",
        "<p>For each division, each parliamentary group's majority position is the side (aye or no) with more of that group's votes. A member's vote is marked \"crossed\" when it lands on the other side. Groups that split evenly have no majority position, so no vote in that division is marked. Independents have no group majority and are never marked.</p>",
        "<h2>Chamber composition</h2>",
        "<p>Seat counts group members by parliamentary group as recorded on Wikidata, which follows the official record: Coalition members are shown as one group. Counts reflect current sitting members, not the election result.</p>",
        "<h2>Election percentages</h2>",
        "<p>First preference and two-candidate-preferred percentages are computed from AEC final totals per electorate. Swing figures are the AEC's own published swings.</p>",
        "<h2>AI summaries and notes</h2>",
        "<p>Machine-written text appears in three places, always labelled \"AI-generated\".</p>",
        "<p><strong>Bill context.</strong> Each bill's page explains the official summary in plain terms. General knowledge of Australian parliamentary practice is used to explain the mechanism (what an appropriation bill is, for example); anything specific to the bill comes from its official summary only.</p>",
        "<p><strong>Division context.</strong> They Vote For You provides written context for most bill votes; for procedural motions their context field is a raw Hansard excerpt. For those, a one-to-two sentence note explains what the bill, amendment or motion was about and what question was being decided, grounded on that excerpt and the official bill summaries. It never restates the result, which the page already shows, and links to the full record.</p>",
        "<p><strong>Voting record in brief.</strong> Each member's page carries a short note describing patterns their voting table cannot show at a glance: the subject areas that recur among their votes and which way they voted on them, plus any divisions where they voted against their own party grouping. The generator is instructed to describe, never evaluate: no praise, no criticism, no motives, no ideology. Notes regenerate as the record grows.</p>",
        "<p>AI text is never part of the record. If a summary or note misstates the record,<a href=\"/about/corrections/\">request a correction</a> and it will be regenerated or removed.</p>",
        "<h2>What this site never does</h2>",
        "<p>No scoring, no ranking, no summarising of speeches, no inference of positions from votes. Where They Vote For You provides a plain-English description of a motion, it is shown with explicit attribution to them.</p>",
    )
    .to_string();
    Page::new(
        "Methodology",
        Some("How every derived figure on pollywiki is computed, and what the official record cannot tell you.".to_string()),
        "/about/methodology/",
        body,
    )
}

pub fn corrections() -> Page {
    let body = concat!(
        "<h1>Corrections</h1>",
        "<p>Every page here is generated from official sources, but pipelines have bugs and sources have errors. If anything on this site is wrong, incomplete or misleading, report it and it will be fixed or taken down quickly.</p>",
        "<h2>How to report</h2>",
        "<p><a href=\"https://github.com/logan-han/pollywiki/issues\">Open an issue on GitHub</a> with a link to the affected page and, if you can, a link to the official record that shows the correct information.</p>",
        "<h2>What happens</h2>",
        "<p>Reports about a person are treated with priority. If a claim about a person cannot be verified against the official source promptly, the content is removed while it is checked. Corrections are applied at the data pipeline level so they cannot regress on the next sync.</p>",
        "<h2>Upstream errors</h2>",
        "<p>Where the error is in the source itself (Hansard, the AEC, Wikidata or They Vote For You), it needs fixing there; this site will pick up the fix on the next sync. Reports are forwarded upstream where possible.</p>",
    )
    .to_string();
    Page::new(
        "Corrections",
        Some("How to report an error on pollywiki and what happens next.".to_string()),
        "/about/corrections/",
        body,
    )
}

/// new Date(str).getTime() for the two shapes the record contains:
/// date-only strings parse as UTC; date-times without a zone parse as local
/// time the build runs in, which is UTC in CI.
fn parse_js_date_millis(input: &str) -> Option<i64> {
    if let Ok(date) = chrono::NaiveDate::parse_from_str(input, "%Y-%m-%d") {
        return Some(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis());
    }
    if let Ok(datetime) = chrono::NaiveDateTime::parse_from_str(input, "%Y-%m-%dT%H:%M:%S") {
        return Some(datetime.and_utc().timestamp_millis());
    }
    chrono::DateTime::parse_from_rfc3339(input)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bill(json: &str) -> Bill {
        serde_json::from_str(json).expect("bill fixture")
    }

    #[test]
    fn short_dates_drop_the_year() {
        assert_eq!(short_date("2025-08-01"), "1 Aug");
        assert_eq!(short_date("2026-12-31"), "31 Dec");
        assert_eq!(short_date("not-a-date"), "not-a-date");
    }

    #[test]
    fn status_pills_bucket_every_bill() {
        let key = |status: &str| {
            bill_status_key(&bill(&format!(
                r#"{{"id":"a","title":"A","parliament":48,"chamber":"senate","status":"{status}"}}"#
            )))
        };
        // The five statuses APH actually reports, plus the longer "Before the
        // Senate" wording the sample bundles use.
        assert_eq!(key("Before Senate"), "open");
        assert_eq!(key("Before Reps"), "open");
        assert_eq!(key("Before the Senate"), "open");
        assert_eq!(key("Act"), "act");
        assert_eq!(key("Assent"), "act");
        assert_eq!(key("Not Proceeding"), "other");
        assert_eq!(key("Discharged"), "other");
    }

    #[test]
    fn event_descriptions_drop_only_the_chamber_suffix() {
        assert_eq!(
            event_description("Referred to Federation Chamber (House of Representatives)"),
            "Referred to Federation Chamber"
        );
        assert_eq!(
            event_description("Committee of the Whole debate (Senate)"),
            "Committee of the Whole debate"
        );
        // No chamber suffix, and a parenthetical that is not one, both survive.
        assert_eq!(event_description("Assent"), "Assent");
        assert_eq!(
            event_description("Second reading (adjourned)"),
            "Second reading (adjourned)"
        );
    }

    #[test]
    fn latest_step_is_the_newest_by_date_not_by_position() {
        let out_of_order = bill(
            r#"{"id":"b","title":"B","parliament":48,"chamber":"senate","status":"Act",
                "timeline":[{"date":"2026-05-01","event":"Assent"},
                            {"date":"2026-01-01","event":"Introduced"}]}"#,
        );
        assert_eq!(latest_step(&out_of_order).unwrap().event, "Assent");
        let empty =
            bill(r#"{"id":"c","title":"C","parliament":48,"chamber":"senate","status":"Act"}"#);
        assert!(latest_step(&empty).is_none());
    }

    #[test]
    fn jsonld_cannot_close_its_own_script_element() {
        let out = jsonld_script(vec![serde_json::json!({ "name": "</script><b>x" })]);
        assert!(!out.contains("</script>"));
        assert!(out.contains("\\u003c/script\\u003e"));
    }

    #[test]
    fn breadcrumbs_are_absolute_and_ordered() {
        let value = breadcrumb(
            "https://pollywiki.au",
            &[("pollywiki", "/"), ("Bills", "/bills/")],
        );
        let items = value["itemListElement"].as_array().expect("list");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["position"], 1);
        assert_eq!(items[1]["item"], "https://pollywiki.au/bills/");
    }
}
