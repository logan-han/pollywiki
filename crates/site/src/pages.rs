//! Every page template. Text flows are written pre-collapsed, so these
//! strings carry the exact bytes the pages render; the original constraint of
//! matching an earlier build byte-for-byte no longer applies.

use crate::components::{
    avatar, bill_dots, chamber_chip, composition_bar, federal_segments, group_chip,
    group_chip_labelled, ledger_month, ledger_row_labelled, mini_bar, month_jump, month_label,
    person_card, portrait, result_chip, result_word, seat_bar, series_row, series_row_in_month,
    sitting_day_row, vote_bar, BarSegment, BILL_DOTS_LEGEND, GROUP_FALLBACK_COLOUR,
};
use crate::data::{
    self, division_key, format_date, locale_int, parse_bill_summary, parse_occupation,
    parse_qualification, short_date, state_bill_path, state_bills_path, state_member_path,
    state_members_path, state_name, state_path, title_from_slug, title_tier, to_fixed, Occupation,
    SiteData,
};
use crate::html::{esc, esc_attr};
use crate::layout::Page;
use crate::markdown;
use crate::procedures::procedure_for;
use crate::strip::js_without_comment_lines;
use pollywiki_schema::{
    Bill, CandidateResult, ChamberKind, Division, Electorate, House, JsNum, Party, Person,
    StateBill, StateChamber, StateCode, StateMember, StateParliament, SummaryKind, Vote,
};
use regex::Regex;
use std::sync::LazyLock;

// One filter script per index page, inlined where the page needs it, each
// behind the helpers all four share: folded any-order matching, state in the
// URL, a settled live count. Inlined scripts ship without their comment
// lines; the commented sources stay in assets/js.
fn filter_script(page_js: &str) -> String {
    js_without_comment_lines(&format!(
        "{}{page_js}",
        include_str!("../assets/js/filter-kit.js")
    ))
}
static PEOPLE_FILTER_JS: LazyLock<String> =
    LazyLock::new(|| filter_script(include_str!("../assets/js/people-filter.js")));
static DIVISION_FILTER_JS: LazyLock<String> =
    LazyLock::new(|| filter_script(include_str!("../assets/js/division-filter.js")));
static BILL_FILTER_JS: LazyLock<String> =
    LazyLock::new(|| filter_script(include_str!("../assets/js/bill-filter.js")));
static ELECTORATE_FILTER_JS: LazyLock<String> =
    LazyLock::new(|| filter_script(include_str!("../assets/js/electorate-filter.js")));
// Pagefind's interface on /search/, labelled, announced and kept in the URL.
static SEARCH_JS: LazyLock<String> =
    LazyLock::new(|| js_without_comment_lines(include_str!("../assets/js/search.js")));

/// Live count plus an explicit empty state, shared by the four index pages.
/// The count element stays in the DOM so its aria-live region is stable; it
/// reads empty while nothing is filtered.
fn filter_feedback(empty_message: &str) -> String {
    format!(
        "<p class=\"note filter-count\" id=\"filter-count\" aria-live=\"polite\"></p><div class=\"filter-empty\" id=\"filter-empty\" hidden><p>{}</p><button type=\"button\" id=\"filter-clear\">Clear filters</button></div>",
        esc(empty_message)
    )
}

/// Sideways-scrolling wrapper for a table that can outgrow a phone screen.
/// It is a named region, so a keyboard user who tabs to it hears which table
/// the arrow keys will scroll; the layout's table-scroll script gives it that
/// tab stop only while the table overflows. A region is a landmark, so every
/// label must be unique on its page: use the section heading, or a short noun
/// where the table has none.
fn table_scroll(label: &str) -> String {
    format!(
        "<div class=\"table-scroll\" role=\"region\" aria-label=\"{}\">",
        esc_attr(label)
    )
}

/// Opens a record page: the article Pagefind indexes, carrying the facts
/// /search/ offers as filters. Type says what kind of record it is; chamber
/// is given only where the record belongs to one. Pagefind takes a literal
/// `key:value` only as the last filter in its attribute, so the chamber is
/// read by reference from an attribute of its own.
fn record_article(kind: &str, chamber: Option<House>) -> String {
    match chamber {
        Some(house) => format!(
            "<article data-pagefind-body data-pagefind-filter=\"chamber[data-chamber], type:{kind}\" data-chamber=\"{}\">",
            chamber_word(house)
        ),
        None => format!("<article data-pagefind-body data-pagefind-filter=\"type:{kind}\">"),
    }
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

/// The site node every page hangs off, so a crawler can tell an index of this
/// site from a page that happens to list things.
fn part_of_site(site_url: &str) -> serde_json::Value {
    serde_json::json!({
        "@type": "WebSite",
        "name": "pollywiki",
        "url": format!("{site_url}/"),
    })
}

/// A listing page, typed as what it collects and how much of it there is.
fn collection_page(
    site_url: &str,
    name: &str,
    description: &str,
    path: &str,
    count: usize,
) -> serde_json::Value {
    serde_json::json!({
        "@context": "https://schema.org",
        "@type": "CollectionPage",
        "name": name,
        "description": description,
        "url": format!("{site_url}{path}"),
        "inLanguage": "en-AU",
        "isPartOf": part_of_site(site_url),
        "mainEntity": { "@type": "ItemList", "numberOfItems": count },
    })
}

/// A page of prose about the site itself.
fn about_page(site_url: &str, name: &str, description: &str, path: &str) -> serde_json::Value {
    serde_json::json!({
        "@context": "https://schema.org",
        "@type": "AboutPage",
        "name": name,
        "description": description,
        "url": format!("{site_url}{path}"),
        "inLanguage": "en-AU",
        "isPartOf": part_of_site(site_url),
    })
}

/// Newest division date in the bundle. ISO dates sort lexically.
fn newest_division_date(data: &SiteData) -> Option<String> {
    data.divisions.iter().map(|d| d.date.clone()).max()
}

/// Newest timeline step across every bill.
fn newest_bill_date(data: &SiteData) -> Option<String> {
    data.bills
        .iter()
        .filter_map(|b| latest_step(b).map(|s| s.date.clone()))
        .max()
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

/// Index row: when the bill last moved, title, chamber, progress, status. The
/// date drops its year because the month divider above the run already carries
/// it; the full date stays machine-readable in the <time> element.
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
    let when = match latest_step(bill) {
        Some(step) => format!(
            "<span class=\"when\" title=\"{}\"><time datetime=\"{}\">{}</time></span>",
            esc_attr(&format!(
                "{} \u{b7} {}",
                event_description(&step.event),
                format_date(&step.date)
            )),
            esc_attr(&step.date),
            esc(&short_date(&step.date)),
        ),
        None => "<span class=\"when\"></span>".to_string(),
    };
    format!(
        "{li}{when}<span class=\"what\"><a href=\"/bills/{id}/\">{title}</a></span><span class=\"chamber\">{chamber}</span><span>{dots}</span><span class=\"{status_class}\">{status}</span></li>",
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

/// Homepage row, in the bills index's column order: when the bill last moved,
/// title, progress dots, and what that step was. The date drops its year, as
/// it does on the index; the full date stays machine-readable in the <time>
/// and in the event's title.
fn bill_activity_row(bill: &Bill) -> String {
    let (when, event) = match latest_step(bill) {
        Some(step) => (
            format!(
                "<span class=\"when\"><time datetime=\"{}\">{}</time></span>",
                esc_attr(&step.date),
                esc(&short_date(&step.date)),
            ),
            format!(
                "<span class=\"event\" title=\"{}\">{}</span>",
                esc_attr(&format!(
                    "{} \u{b7} {}",
                    step.event,
                    format_date(&step.date)
                )),
                esc(event_description(&step.event)),
            ),
        ),
        None => (
            "<span class=\"when\"></span>".to_string(),
            format!("<span class=\"event\">{}</span>", esc(&bill.status)),
        ),
    };
    format!(
        "<li>{when}<span class=\"what\"><a href=\"/bills/{id}/\">{title}</a></span><span>{dots}</span>{event}</li>",
        id = bill.id,
        title = esc(&bill.title),
        dots = bill_dots(bill),
    )
}

pub fn home(data: &SiteData) -> Page {
    let mut body = String::new();
    body.push_str("<div class=\"masthead\"><h1>What Australia's parliaments actually did.</h1><p class=\"lede\">Decisions that change people's lives should stay on the record. pollywiki keeps the record of all nine of Australia's parliaments, federal, state and territory: who sits in them and what they pass, and for federal parliament how every member voted, straight from official sources, with no commentary.</p></div>");
    body.push_str("<h2 class=\"visually-hidden\">The nine parliaments</h2>");
    body.push_str(&parliament_ledger(data, true));
    body.push_str(&latest_activity_everywhere(data));
    body.push_str(&latest_divisions_section(data, "Latest federal divisions"));

    let mut page = Page::new("pollywiki", None, "/", body);
    // The front page turns over with any parliament's bills.
    let newest_state_bill = data
        .states
        .iter()
        .flat_map(|p| p.bills.iter())
        .filter_map(|b| b.latest().map(|s| s.date.clone()))
        .max();
    page.lastmod = newest_division_date(data)
        .max(newest_bill_date(data))
        .max(newest_state_bill);
    page.jsonld = Some(jsonld_script(vec![serde_json::json!({
        "@context": "https://schema.org",
        "@type": "WebSite",
        "name": "pollywiki",
        "url": format!("{}/", data.site_url),
        "description": crate::layout::DEFAULT_DESCRIPTION,
        "inLanguage": "en-AU",
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

/// The federal parliament's own front page, laid out as each state's is.
pub fn federal_page(data: &SiteData) -> Page {
    let mut body = String::new();
    body.push_str("<div class=\"masthead\"><h1>Parliament of Australia</h1><p class=\"lede\">The federal parliament: who sits in the House of Representatives and the Senate, how every member voted, and what became law.</p></div>");
    body.push_str(&table_scroll("Parliament facts"));
    body.push_str("<table class=\"facts\"><tbody>");
    if let Some(pm) = data.prime_minister() {
        body.push_str(&format!(
            "<tr><th class=\"label\" scope=\"row\">Prime Minister</th><td><a href=\"/people/{}/\">{}</a></td></tr>",
            pm.slug,
            esc(&pm.name)
        ));
    }
    body.push_str("<tr><th class=\"label\" scope=\"row\">Website</th><td><a href=\"https://www.aph.gov.au\">aph.gov.au</a></td></tr></tbody></table></div>");
    body.push_str("<h2>Composition of the 48th Parliament</h2>");
    body.push_str(&format!(
        "<p class=\"note\">{} sitting parliamentarians, grouped by parliamentary group.</p>",
        data.sitting().count()
    ));
    body.push_str(&seat_bar(data, House::Representatives));
    body.push_str(&seat_bar(data, House::Senate));
    body.push_str(&latest_divisions_section(data, "Latest divisions"));
    body.push_str(&latest_bills_section(data, "Latest bill activity"));

    const DESCRIPTION: &str = "The Parliament of Australia: who sits in the House of Representatives and the Senate, how every member voted, and what became law.";
    let mut page = Page::new(
        "Parliament of Australia",
        Some(DESCRIPTION.to_string()),
        "/federal/",
        body,
    );
    page.lastmod = newest_division_date(data).max(newest_bill_date(data));
    page.jsonld = Some(jsonld_script(vec![
        serde_json::json!({
            "@context": "https://schema.org",
            "@type": "GovernmentOrganization",
            "name": "Parliament of Australia",
            "url": format!("{}/federal/", data.site_url),
            "sameAs": "https://www.aph.gov.au",
            "areaServed": "Australia",
        }),
        breadcrumb(
            &data.site_url,
            &[("pollywiki", "/"), ("Parliament of Australia", "/federal/")],
        ),
    ]));
    page
}

/// The newest divisions, one row per matter: a contested bill can be put
/// ten times in a sitting day and would otherwise fill the list on its own.
fn latest_divisions_section(data: &SiteData, heading: &str) -> String {
    let mut out = format!(
        "<div class=\"section-head\"><h2>{heading}</h2><a href=\"/divisions/\">All divisions →</a></div>"
    );
    let latest = data.latest_series(8);
    if !latest.is_empty() {
        out.push_str("<ul class=\"ledger\">");
        for series in &latest {
            out.push_str(&series_row(data, series));
        }
        out.push_str("</ul>");
    } else {
        out.push_str("<p class=\"note\">No division records loaded yet. Votes appear here once the They Vote For You sync runs.</p>");
    }
    out
}

/// The bills that moved most recently, newest step first. The sort is
/// stable, so bills with no timeline keep the bundle's order at the bottom.
fn latest_bills_section(data: &SiteData, heading: &str) -> String {
    let mut out = format!(
        "<div class=\"section-head\"><h2>{heading}</h2><a href=\"/bills/\">All bills →</a></div>"
    );
    let mut by_activity: Vec<&Bill> = data.bills.iter().collect();
    by_activity.sort_by(|a, b| {
        latest_step(b)
            .map(|s| s.date.as_str())
            .cmp(&latest_step(a).map(|s| s.date.as_str()))
    });
    let recent: Vec<&Bill> = by_activity.into_iter().take(8).collect();
    if !recent.is_empty() {
        out.push_str("<ul class=\"bill-list activity\">");
        for b in recent {
            out.push_str(&bill_activity_row(b));
        }
        out.push_str("</ul>");
        out.push_str(BILL_DOTS_LEGEND);
    } else {
        out.push_str("<p class=\"note\">No bill records loaded yet.</p>");
    }
    out
}

/// The bills that moved most recently in any parliament, newest step first,
/// each tagged with its parliament: the tag links to that parliament's bills.
fn latest_activity_everywhere(data: &SiteData) -> String {
    struct Row {
        date: Option<String>,
        tag: String,
        title: String,
        dots: String,
        event: String,
    }
    let mut rows: Vec<Row> = Vec::new();
    for bill in &data.bills {
        let step = latest_step(bill);
        rows.push(Row {
            date: step.map(|s| s.date.clone()),
            tag: "<a class=\"jur\" href=\"/bills/\">Federal</a>".to_string(),
            title: format!("<a href=\"/bills/{}/\">{}</a>", bill.id, esc(&bill.title)),
            dots: bill_dots(bill),
            event: match step {
                Some(step) => format!(
                    "<span class=\"event\" title=\"{}\">{}</span>",
                    esc_attr(&format!(
                        "{} \u{b7} {}",
                        step.event,
                        format_date(&step.date)
                    )),
                    esc(event_description(&step.event)),
                ),
                None => format!("<span class=\"event\">{}</span>", esc(&bill.status)),
            },
        });
    }
    for parliament in &data.states {
        let short = crate::layout::PARLIAMENTS
            .iter()
            .find(|(_, _, href)| *href == state_path(parliament.state))
            .map(|(short, _, _)| *short)
            .unwrap_or(parliament.state.as_str());
        for bill in &parliament.bills {
            let (_, event) = state_bill_event(bill);
            rows.push(Row {
                date: bill.latest().map(|s| s.date.clone()),
                tag: format!(
                    "<a class=\"jur\" href=\"{}\">{short}</a>",
                    state_bills_path(parliament.state)
                ),
                title: format!(
                    "<a href=\"{}\">{}</a>",
                    state_bill_path(parliament.state, &bill.slug),
                    esc(&bill.title)
                ),
                dots: state_bill_dots(bill, parliament.houses),
                event,
            });
        }
    }
    // Newest first; the sort is stable, so undated bills keep their order last.
    rows.sort_by(|a, b| b.date.cmp(&a.date));
    let mut out = String::from("<div class=\"section-head\"><h2>Latest bill activity</h2></div>");
    if rows.is_empty() {
        out.push_str("<p class=\"note\">No bill records loaded yet.</p>");
        return out;
    }
    // Parliaments sit at different times of year, so a row from an earlier
    // year than the newest says which.
    let newest_year: String = rows[0]
        .date
        .as_deref()
        .map(|d| d.chars().take(4).collect())
        .unwrap_or_default();
    out.push_str("<ul class=\"bill-list activity everywhere\">");
    for row in rows.into_iter().take(10) {
        let when = match &row.date {
            Some(date) => format!(
                "<span class=\"when\"><time datetime=\"{}\">{}</time></span>",
                esc_attr(date),
                esc(&if date.starts_with(&newest_year) {
                    short_date(date)
                } else {
                    format_date(date)
                }),
            ),
            None => "<span class=\"when\"></span>".to_string(),
        };
        out.push_str(&format!(
            "<li>{when}<span class=\"what\">{} {}</span><span>{}</span>{}</li>",
            row.tag, row.title, row.dots, row.event
        ));
    }
    out.push_str("</ul>");
    out.push_str("<p class=\"note\">Dots show how far each bill has come: four stages in a parliament of two chambers, three in a parliament of one. Each parliament's own list has the rest.</p>");
    out
}

/// Australia's parliaments as one ledger, federal first and then the states
/// and territories in the Style Manual's order, every row laid out alike: who
/// leads its government, the house that government is formed in by party,
/// and the bills still before it. The bars share one width and line up, so
/// the nine compare at a glance; the mark on each is a majority.
fn parliament_ledger(data: &SiteData, with_federal: bool) -> String {
    struct Row {
        short: &'static str,
        name: &'static str,
        href: String,
        leader: Option<String>,
        house: Option<String>,
        seats: Option<String>,
        unread: Option<&'static str>,
        bills: Option<(usize, String)>,
    }
    let mut rows: Vec<Row> = Vec::new();
    if with_federal {
        let segments = federal_segments(data, House::Representatives);
        let total: i64 = segments.iter().map(|s| s.seats).sum();
        let pm = data.prime_minister();
        let governing = pm
            .and_then(|p| data.party_by_slug(&p.group_slug))
            .map(|party| {
                format!(
                    "{} {} of {total}",
                    party.code.as_deref().unwrap_or(&party.name),
                    party
                        .seats
                        .as_ref()
                        .map(|s| s.get(House::Representatives))
                        .unwrap_or(0)
                )
            });
        rows.push(Row {
            short: "Federal",
            name: "Parliament of Australia",
            href: "/federal/".to_string(),
            leader: pm.map(|p| format!("Prime Minister {}", p.name)),
            house: Some(mini_bar("House of Representatives", &segments, 0)),
            seats: governing,
            unread: None,
            bills: Some((
                data.bills
                    .iter()
                    .filter(|b| b.status.starts_with("Before"))
                    .count(),
                "/bills/".to_string(),
            )),
        });
    }
    for (short, name, href) in crate::layout::PARLIAMENTS.iter().skip(1) {
        let code = href.trim_matches('/').rsplit('/').next().unwrap_or("");
        let record = StateCode::parse(&code.to_uppercase()).and_then(|s| data.state_parliament(s));
        let mut row = Row {
            short,
            name,
            href: href.to_string(),
            leader: None,
            house: None,
            seats: None,
            unread: None,
            bills: None,
        };
        match record.filter(|p| !p.chambers.is_empty()) {
            Some(parliament) => {
                let lower = parliament
                    .chambers
                    .iter()
                    .find(|c| c.kind == ChamberKind::Lower)
                    .unwrap_or(&parliament.chambers[0]);
                row.leader = parliament
                    .leader()
                    .map(|(_, l)| format!("{} {}", parliament.leader_title, l.name));
                row.house = Some(mini_bar(
                    &lower.name,
                    &state_segments(data, lower),
                    lower.vacancies(),
                ));
                // The governing party's share of that house, or the largest
                // party's where no leader is listed.
                row.seats = parliament
                    .leader()
                    .and_then(|(_, l)| lower.parties.iter().find(|p| p.party_slug == l.party_slug))
                    .or_else(|| lower.parties.first())
                    .map(|party| {
                        format!(
                            "{} {} of {}",
                            party.code.as_deref().unwrap_or(&party.party),
                            party.seats,
                            lower.seats
                        )
                    });
            }
            None => {
                row.unread = Some(if record.is_some_and(|p| p.unavailable.is_some()) {
                    "Member list not readable yet"
                } else {
                    "Member list not read yet"
                });
            }
        }
        if let Some(parliament) = record.filter(|p| !p.bills.is_empty()) {
            // Still before it: not yet through every chamber it has. A bill
            // that has passed and waits only on assent is no longer.
            let through = if parliament.houses == 1 { 2 } else { 3 };
            let open = parliament
                .bills
                .iter()
                .filter(|b| b.stage < through && !b.stopped)
                .count();
            row.bills = Some((open, state_bills_path(parliament.state)));
        }
        rows.push(row);
    }

    let mut out = String::from("<div class=\"parliaments\"><p class=\"parliaments-head\" aria-hidden=\"true\"><span>Parliament</span><span>House government is formed in</span><span>Bills before it</span></p><ul role=\"list\">");
    for row in &rows {
        let label = if row.short == "Federal" {
            row.short.to_string()
        } else {
            format!(
                "{}<span class=\"visually-hidden\"> ({})</span>",
                row.short, row.name
            )
        };
        out.push_str(&format!(
            "<li{}><span class=\"who\"><a class=\"name\" href=\"{}\">{label}</a><span class=\"leader\">{}</span></span>",
            if row.unread.is_some() { " class=\"unread\"" } else { "" },
            row.href,
            esc(row.leader.as_deref().unwrap_or(row.name)),
        ));
        match (&row.house, row.unread) {
            (Some(bar), _) => out.push_str(&format!(
                "<span class=\"house\">{bar}</span><span class=\"seats\">{}</span>",
                esc(row.seats.as_deref().unwrap_or(""))
            )),
            (None, unread) => out.push_str(&format!(
                "<span class=\"house\">{}</span>",
                unread.unwrap_or("")
            )),
        }
        match &row.bills {
            Some((open, href)) => out.push_str(&format!(
                "<span class=\"open\"><a href=\"{href}\">{open}</a><span class=\"unit\"> {} before it</span></span>",
                if *open == 1 { "bill" } else { "bills" }
            )),
            None => out.push_str("<span class=\"open none\">Not read</span>"),
        }
        out.push_str("</li>");
    }
    out.push_str("</ul><p class=\"note\">Seats by party in the house each government is formed in, marked at a majority.</p></div>");
    out
}

pub fn people_index(data: &SiteData) -> Page {
    let by_name = |list: &mut Vec<&Person>| {
        list.sort_by(|a, b| pollywiki_schema::js_compare(&a.name, &b.name));
    };
    let mut sorted: Vec<&Person> = data.sitting().collect();
    by_name(&mut sorted);
    // Members who have left sit outside the filters: the chamber and party
    // controls describe the parliament as it stands.
    let mut former: Vec<&Person> = data.former().collect();
    by_name(&mut former);

    let mut body = String::new();
    body.push_str("<div class=\"masthead\"><h1>People</h1><p class=\"lede\">");
    body.push_str(&format!(
        "{} sitting parliamentarians in the 48th Parliament.",
        sorted.len()
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
    body.push_str("</span><input type=\"search\" id=\"people-filter\" placeholder=\"Name, electorate or state\" enterkeyhint=\"done\" aria-label=\"Filter people\"></div>");
    body.push_str(&filter_feedback("No one matches these filters."));

    // What the text filter matches a card against: the name, the seat, and
    // the state both as its code and in full, so 'tas' and 'tasmania' both
    // answer.
    let haystack = |person: &Person| {
        let state = person.state.map(|s| s.as_str()).unwrap_or("");
        esc_attr(
            &format!(
                "{} {} {} {}",
                person.name,
                person.electorate.as_deref().unwrap_or(""),
                state,
                state_name(state).unwrap_or("")
            )
            .to_lowercase(),
        )
    };

    let sitting_count = sorted.len();
    body.push_str("<div class=\"person-grid\" id=\"person-grid\">");
    for person in sorted {
        body.push_str(&format!(
            "<div class=\"person-cell\" data-name=\"{}\" data-house=\"{}\" data-group=\"{}\">{}</div>",
            haystack(person),
            person.house,
            person.group_slug,
            person_card(data, person),
        ));
    }
    body.push_str("</div>");

    if !former.is_empty() {
        let one = former.len() == 1;
        body.push_str(&format!(
            "<section id=\"former-members\"><h2>Former members</h2><p class=\"note\">{} sat in this parliament and {} since left. Their {} because the divisions they voted in are permanent.</p><div class=\"person-grid\">",
            if one {
                "One member".to_string()
            } else {
                format!("{} members", former.len())
            },
            if one { "has" } else { "have" },
            if one { "page stays" } else { "pages stay" },
        ));
        // The text filter covers these cards too, so a name that matches is
        // never reported missing while it sits on the page. They carry no
        // chamber or party to match: those controls describe the parliament
        // as it stands.
        for person in former {
            body.push_str(&format!(
                "<div class=\"person-cell\" data-name=\"{}\">{}</div>",
                haystack(person),
                person_card(data, person),
            ));
        }
        body.push_str("</div></section>");
    }

    const DESCRIPTION: &str =
        "Every sitting member of the Australian House of Representatives and Senate.";
    let mut page = Page::new("People", Some(DESCRIPTION.to_string()), "/people/", body);
    page.page_script = Some(PEOPLE_FILTER_JS.as_str());
    page.jsonld = Some(jsonld_script(vec![
        collection_page(
            &data.site_url,
            "People",
            DESCRIPTION,
            "/people/",
            sitting_count,
        ),
        breadcrumb(
            &data.site_url,
            &[("pollywiki", "/"), ("People", "/people/")],
        ),
    ]));
    page
}

pub fn person_page(data: &SiteData, person: &Person) -> Page {
    let electorate = person
        .electorate
        .as_deref()
        .and_then(|slug| data.electorate_by_slug(slug));
    let (role, seat_name) = match person.house {
        House::Senate => (
            "Senator for",
            person
                .state
                .map(|s| state_name(s.as_str()).unwrap_or(s.as_str()))
                .unwrap_or("")
                .to_string(),
        ),
        House::Representatives => (
            "Member for",
            match electorate {
                Some(e) => e.name.clone(),
                // A seat abolished at a redistribution leaves the bundle, but
                // whoever sat for it still did.
                None => title_from_slug(person.electorate.as_deref()),
            },
        ),
    };
    let seat = if person.is_former() {
        format!("Former {role} {seat_name}")
    } else {
        format!("{role} {seat_name}")
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
        // A finished term stops accruing years the day it ended.
        let end_ms = match person.until.as_deref().and_then(parse_js_date_millis) {
            Some(ms) => ms,
            None => chrono::Utc::now().timestamp_millis(),
        };
        let years = (end_ms - start_ms) as f64 / 31_557_600_000.0;
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
    body.push_str(&record_article("Person", Some(person.house)));
    body.push_str("<div class=\"profile-head\">");
    body.push_str(&avatar(person, true));
    body.push_str("<div><div class=\"meta-row\"><span class=\"ids\">");
    // The spaces between the identifiers cost nothing in the inline-flex
    // row; they keep the words apart in search excerpts and when copied.
    body.push_str(&chamber_chip(person.house));
    body.push_str(&format!(" <span class=\"seat\">{}</span>", esc(&seat)));
    // The term's middot is a span of its own, so the stylesheet can hang it
    // in the gap after the seat and drop it when the term wraps to a line of
    // its own. It stays in the text for excerpts and copying, but a screen
    // reader has no use for it.
    const TERM_SEP: &str = "<span class=\"sep\" aria-hidden=\"true\">\u{b7} </span>";
    match (&person.since, &person.until) {
        (Some(since), Some(until)) => body.push_str(&format!(
            " <span class=\"term\">{TERM_SEP}{} to {}</span>",
            esc(&format_date(since)),
            esc(&format_date(until))
        )),
        (Some(since), None) => body.push_str(&format!(
            " <span class=\"term\">{TERM_SEP}since {}</span>",
            esc(&format_date(since))
        )),
        (None, Some(until)) => body.push_str(&format!(
            " <span class=\"term\">{TERM_SEP}until {}</span>",
            esc(&format_date(until))
        )),
        (None, None) => {}
    }
    body.push_str("</span></div>");
    body.push_str(&format!(
        "<h1 data-pagefind-meta=\"title\">{}</h1>",
        esc(&person.name)
    ));
    // As in the meta row, a space before each item keeps them apart in an
    // excerpt at no cost to the flex line.
    body.push_str("<div class=\"meta-line\">");
    body.push_str(&group_chip(data, &person.group_slug, &person.group, true));
    if let Some(e) = electorate {
        body.push_str(&format!(
            " <a href=\"/electorates/{}/\">Electorate profile</a>",
            e.slug
        ));
    }
    if let Some(wikipedia) = &person.links.wikipedia {
        body.push_str(&format!(
            " <a href=\"{}\">Wikipedia</a>",
            esc_attr(wikipedia)
        ));
    }
    if person.ids.tvfy.is_some() {
        body.push_str(&format!(
            " <a href=\"https://theyvoteforyou.org.au/people/{}/{}\">They Vote For You</a>",
            person.house,
            person.electorate.as_deref().unwrap_or("")
        ));
    }
    body.push_str("</div></div></div>");

    if let Some(until) = &person.until {
        body.push_str(&format!(
            "<p class=\"procedure-note\"><strong>Former member:</strong> left the {} on {}. The page stays because the divisions recorded below are permanent.</p>",
            chamber_word(person.house),
            esc(&format_date(until))
        ));
    }

    if let Some(stats) = stats.filter(|s| s.divisions_eligible > 0) {
        body.push_str("<div class=\"stat-strip\">");
        // The link is named by its figure and the label beside it, so it
        // reads as more than "2 / 2" in a list of links.
        body.push_str(&format!(
            "<div class=\"stat\"><span class=\"n\"><a id=\"stat-voted\" href=\"#voting-record\" aria-labelledby=\"stat-voted stat-voted-l\">{} / {}</a></span> <span class=\"l\" id=\"stat-voted-l\">divisions voted in this parliament</span></div>",
            stats.divisions_voted, stats.divisions_eligible
        ));
        body.push_str(&format!(
            "<div class=\"stat\"><span class=\"n\">{}</span> <span class=\"l\">votes against their party's majority position</span></div>",
            stats.against_group_majority
        ));
        if let Some(years) = &service_years {
            body.push_str(&format!(
                "<div class=\"stat\"><span class=\"n\">{years}</span> <span class=\"l\">years in federal parliament</span></div>"
            ));
        }
        body.push_str("</div>");
        body.push_str("<p class=\"note\">Counted against the divisions held while they sat, not the whole parliament. Absence from a division is not abstention: pairing arrangements, leave and parliamentary duties are not distinguished in the official record. <a href=\"/about/methodology/\">How these figures are computed.</a></p>");
    }

    // A long profile opens on a line of links to its sections, in page order,
    // so the voting record is one step away however much biography sits above
    // it. It follows the figures' caveat rather than splitting the two.
    let positions = person.positions.as_deref().filter(|p| !p.is_empty());
    let committees = person.committees.as_deref().filter(|c| !c.is_empty());
    let elections = person.elections.as_deref().filter(|e| !e.is_empty());
    let expenses = person.expenses.as_deref().filter(|e| !e.is_empty());
    let funding = person
        .funding
        .as_ref()
        .filter(|f| !f.elections.is_empty() || !f.annual.is_empty());
    // One word each, so a full profile's contents still sit on one line;
    // the headings they point at carry the full names.
    let sections: Vec<(&str, &str)> = [
        ("background", "Background", person.background.is_some()),
        ("positions", "Positions", positions.is_some()),
        ("committees", "Committees", committees.is_some()),
        ("elections", "Elections", elections.is_some()),
        ("expenses", "Expenses", expenses.is_some()),
        ("funding", "Funding", funding.is_some()),
        ("bills-raised", "Bills", !raised.is_empty()),
        ("voting-record", "Votes", true),
    ]
    .into_iter()
    .filter(|(_, _, present)| *present)
    .map(|(id, label, _)| (id, label))
    .collect();
    if sections.len() > 1 {
        body.push_str("<nav class=\"on-page\" aria-labelledby=\"on-page-label\"><span class=\"label\" id=\"on-page-label\">On this page</span>");
        for (id, label) in &sections {
            body.push_str(&format!("<a href=\"#{id}\">{label}</a>"));
        }
        body.push_str("</nav>");
    }

    if let Some(note) = &person.ai_note {
        body.push_str("<aside class=\"context-box\"><div class=\"context-top\" data-pagefind-ignore><span class=\"context-head\">Voting record in brief</span><span class=\"ai-tag\">AI-generated</span></div><div class=\"context-body\"><p>");
        body.push_str(&data.link_bill_titles(&note.text));
        body.push_str("</p></div><div class=\"context-credit\">Written by AI from the voting record below; describes subjects and votes, never merit. May contain errors — the record is authoritative. <a href=\"/about/methodology/\">How this works.</a></div></aside>");
    }

    if let Some(background) = &person.background {
        body.push_str("<h2 id=\"background\">Background</h2>");
        body.push_str(&table_scroll("Background"));
        body.push_str("<table class=\"facts\"><tbody>");
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
                    .map(|&n| data::ordinal(n))
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
            body.push_str(&table_scroll("Qualifications"));
            body.push_str("<table><thead data-pagefind-ignore><tr><th scope=\"col\">Qualification</th><th scope=\"col\">Institution</th></tr></thead><tbody>");
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
            let rows: Vec<Occupation> = background
                .occupations
                .iter()
                .map(|o| parse_occupation(o))
                .collect();
            // A career of bare titles gets a one-column table, not two empty ones.
            let has_org = rows.iter().any(|o| !o.org.is_empty());
            let has_period = rows.iter().any(|o| !o.period.is_empty());
            body.push_str("<h3>Occupations before parliament</h3>");
            body.push_str(&table_scroll("Occupations before parliament"));
            body.push_str("<table><thead data-pagefind-ignore><tr><th scope=\"col\">Role</th>");
            if has_org {
                body.push_str("<th scope=\"col\">Organisation</th>");
            }
            if has_period {
                body.push_str("<th class=\"num\" scope=\"col\">Period</th>");
            }
            body.push_str("</tr></thead><tbody>");
            for o in &rows {
                body.push_str(&format!("<tr><td>{}</td>", esc(&o.role)));
                if has_org {
                    body.push_str(&format!("<td>{}</td>", esc(&o.org)));
                }
                if has_period {
                    body.push_str(&format!("<td class=\"num\">{}</td>", esc(&o.period)));
                }
                body.push_str("</tr>");
            }
            body.push_str("</tbody></table></div>");
        }
    }

    if let Some(positions) = positions {
        // The Handbook records an office once per ministry, so a minister who
        // served under Rudd, Gillard and Rudd again holds one office over one
        // term three times. The table keeps one row per office and term and
        // names every ministry it spanned; offices still held come first.
        let mut rows: Vec<(&pollywiki_schema::PositionRecord, Vec<&str>)> = Vec::new();
        for pos in positions {
            let existing = rows.iter_mut().find(|(p, _)| {
                p.role == pos.role && p.kind == pos.kind && p.from == pos.from && p.to == pos.to
            });
            match existing {
                Some((_, ministries)) => {
                    if let Some(ministry) = pos.ministry.as_deref() {
                        if !ministries.contains(&ministry) {
                            ministries.push(ministry);
                        }
                    }
                }
                None => rows.push((pos, pos.ministry.as_deref().into_iter().collect())),
            }
        }
        rows.sort_by_key(|(p, _)| p.to.is_some());
        body.push_str("<h2 id=\"positions\">Positions held</h2>");
        body.push_str(&table_scroll("Positions held"));
        body.push_str("<table><thead data-pagefind-ignore><tr><th scope=\"col\">Role</th><th scope=\"col\">Ministry</th><th class=\"num\" scope=\"col\">From</th><th class=\"num\" scope=\"col\">To</th></tr></thead><tbody>");
        for (pos, ministries) in &rows {
            body.push_str(&format!(
                "<tr><td>{}{}</td><td>{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
                esc(&pos.role),
                // A shadow office that says so in its title needs no second
                // label.
                if pos.kind == pollywiki_schema::PositionKind::Shadow
                    && !pos.role.starts_with("Shadow")
                {
                    " (shadow)"
                } else {
                    ""
                },
                esc(&ministries.join("; ")),
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

    if let Some(committees) = committees {
        body.push_str("<h2 id=\"committees\">Committee service</h2>");
        body.push_str(&table_scroll("Committee service"));
        body.push_str("<table><thead data-pagefind-ignore><tr><th scope=\"col\">Committee</th><th scope=\"col\">Role</th><th class=\"num\" scope=\"col\">From</th><th class=\"num\" scope=\"col\">To</th></tr></thead><tbody>");
        let date = |d: Option<&str>| d.map(format_date).map(|d| esc(&d));
        for c in committees {
            // A role held on dates of its own within the membership says so.
            let role = match (&c.role, &c.role_from) {
                (Some(role), Some(from)) => format!(
                    "{} from {}{}",
                    esc(role),
                    esc(&format_date(from)),
                    date(c.role_to.as_deref())
                        .map(|to| format!(" to {to}"))
                        .unwrap_or_default()
                ),
                (Some(role), None) => esc(role),
                (None, _) => "Member".to_string(),
            };
            body.push_str(&format!(
                "<tr><td>{}{}</td><td>{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
                esc(&c.name),
                if c.kind.is_empty() {
                    String::new()
                } else {
                    format!(" <span class=\"sub\">{}</span>", esc(&c.kind))
                },
                role,
                date(c.from.as_deref()).unwrap_or_default(),
                date(c.to.as_deref()).unwrap_or_else(|| "current".to_string()),
            ));
        }
        body.push_str("</tbody></table></div><p class=\"note\">Memberships as the Parliamentary Handbook records them; current means no end date is recorded.</p>");
    }

    if let Some(elections) = elections {
        body.push_str("<h2 id=\"elections\">Election history</h2>");
        body.push_str(&table_scroll("Election history"));
        body.push_str("<table><thead data-pagefind-ignore><tr><th scope=\"col\">Election</th><th scope=\"col\">Electorate</th><th scope=\"col\">Party</th><th class=\"num\" scope=\"col\">First pref %</th><th class=\"num\" scope=\"col\">Swing</th><th scope=\"col\">Result</th></tr></thead><tbody>");
        for e in elections {
            // A seat abolished at a redistribution has no page of its own; the
            // contest stays on the record as plain text. A Senate count is the
            // whole state's.
            let seat = match (&e.senate, data.electorate_by_slug(&e.electorate_slug)) {
                (Some(senate), _) => format!(
                    "Senate, {}",
                    esc(state_name(senate.state.as_str()).unwrap_or(senate.state.as_str()))
                ),
                (None, Some(_)) => format!(
                    "<a href=\"/electorates/{}/\">{}</a>",
                    e.electorate_slug,
                    esc(&e.electorate_name)
                ),
                (None, None) => esc(&e.electorate_name),
            };
            let result = match (&e.senate, e.elected) {
                (Some(senate), true) => match senate.elected_order {
                    Some(order) => {
                        format!("Elected {} of {}", data::ordinal(order), senate.vacancies)
                    }
                    None => "Elected".to_string(),
                },
                (_, true) => "Elected".to_string(),
                (_, false) => "Not elected".to_string(),
            };
            body.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td>{}</td></tr>",
                esc(&e.event_name),
                seat,
                esc(&e.party),
                to_fixed(e.pct.0, 2),
                signed(e.swing),
                result,
            ));
        }
        body.push_str("</tbody></table></div><p class=\"note\">First-preference share of the formal vote at each event, matched by name within the member's state.");
        if elections.iter().any(|e| e.senate.is_some()) {
            body.push_str(" A Senate row gives the whole group's share of the state's formal vote, since most Senate votes are cast for a group above the line.");
        }
        body.push_str("</p>");
    }

    if let Some(expenses) = expenses {
        // Rows are IPEA's own categories, the biggest over the quarters shown
        // first; columns are quarters, newest first.
        let quarters = &expenses[..expenses.len().min(4)];
        let mut categories: Vec<(&str, f64)> = Vec::new();
        for line in quarters.iter().flat_map(|q| &q.categories) {
            match categories.iter_mut().find(|(c, _)| *c == line.category) {
                Some((_, total)) => *total += line.amount.0,
                None => categories.push((&line.category, line.amount.0)),
            }
        }
        categories.sort_by(|a, b| b.1.total_cmp(&a.1));
        body.push_str("<h2 id=\"expenses\">Parliamentary expenses</h2>");
        body.push_str(&table_scroll("Parliamentary expenses"));
        body.push_str("<table><thead data-pagefind-ignore><tr><th scope=\"col\">Category</th>");
        for q in quarters {
            body.push_str(&format!(
                "<th class=\"num\" scope=\"col\">{}</th>",
                esc(&q.label)
            ));
        }
        body.push_str("</tr></thead><tbody>");
        for (category, _) in &categories {
            body.push_str(&format!("<tr><td>{}</td>", esc(category)));
            for q in quarters {
                match q.categories.iter().find(|l| l.category == *category) {
                    Some(line) => body.push_str(&format!(
                        "<td class=\"num\">{}</td>",
                        dollars(line.amount.0)
                    )),
                    None => body.push_str("<td class=\"num zero\">$0</td>"),
                }
            }
            body.push_str("</tr>");
        }
        body.push_str("</tbody><tfoot><tr><td>Total</td>");
        for q in quarters {
            body.push_str(&format!("<td class=\"num\">{}</td>", dollars(q.total.0)));
        }
        body.push_str("</tr></tfoot></table></div>");
        body.push_str("<p class=\"note\">Expenditure as the Independent Parliamentary Expenses Authority publishes it each quarter, in its own categories, rounded to the dollar. Travel distances, office locations and roles differ, so totals do not compare between members. <a href=\"https://www.ipea.gov.au/reporting\">IPEA's reports and notes.</a></p>");
    }

    if let Some(funding) = funding {
        body.push_str("<h2 id=\"funding\">Funding disclosures</h2>");
        if !funding.elections.is_empty() {
            body.push_str(&table_scroll("Election returns"));
            body.push_str("<table><thead data-pagefind-ignore><tr><th scope=\"col\">Election</th><th scope=\"col\">Electorate</th><th class=\"num\" scope=\"col\">Donations</th><th class=\"num\" scope=\"col\">Donors</th><th class=\"num\" scope=\"col\">Expenditure</th></tr></thead><tbody>");
            for e in &funding.elections {
                // A Senate candidate's return names the state as its electorate.
                let senate = pollywiki_schema::STATES
                    .iter()
                    .any(|code| state_name(code) == Some(e.electorate.as_str()));
                body.push_str(&format!(
                    "<tr><td>{}</td><td>{}{}</td>{}</tr>",
                    esc(&e.event),
                    if senate { "Senate, " } else { "" },
                    esc(&e.electorate),
                    if e.nil {
                        "<td class=\"nil\" colspan=\"3\">Nil return</td>".to_string()
                    } else {
                        format!(
                            "<td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td>",
                            dollars(e.gifts as f64),
                            locale_int(e.donors),
                            dollars(e.expenditure as f64)
                        )
                    }
                ));
            }
            body.push_str("</tbody></table></div>");
        }
        if !funding.annual.is_empty() {
            body.push_str(&table_scroll("Annual returns"));
            body.push_str("<table><thead data-pagefind-ignore><tr><th scope=\"col\">Financial year</th><th class=\"num\" scope=\"col\">Donations</th><th class=\"num\" scope=\"col\">Donors</th></tr></thead><tbody>");
            for a in &funding.annual {
                body.push_str(&format!(
                    "<tr><td>{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
                    esc(&a.year),
                    dollars(a.donations as f64),
                    locale_int(a.donors)
                ));
            }
            body.push_str("</tbody></table></div>");
        }
        body.push_str("<p class=\"note\">Returns as lodged with the AEC: a candidate's return covers the gifts received and electoral expenditure for that election, a member's annual return the gifts received in the financial year.");
        if funding.elections.iter().any(|e| e.nil) {
            body.push_str(" A nil return records no gifts or spending of the candidate's own. A party-endorsed candidate lodges one when the party or its campaign committee handled the campaign's money, and the party discloses it in its own returns.");
        }
        body.push_str(
            " <a href=\"https://transparency.aec.gov.au\">AEC Transparency Register.</a></p>",
        );
    }

    if !raised.is_empty() {
        body.push_str("<h2 id=\"bills-raised\">Bills raised</h2>");
        body.push_str(&table_scroll("Bills raised"));
        body.push_str("<table><thead data-pagefind-ignore><tr><th scope=\"col\">Bill</th><th scope=\"col\">Role</th><th scope=\"col\">Status</th></tr></thead><tbody>");
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

    // The record runs newest first under month dividers, as the divisions
    // index does: each month is its own row group, headed by its name, and a
    // date drops the year its divider already gives.
    if votes.is_empty() {
        body.push_str("<h2 id=\"voting-record\">Voting record</h2>");
        body.push_str("<p class=\"note\">No division votes recorded yet for this parliament. Voting records appear once the They Vote For You sync runs.</p>");
    } else {
        body.push_str(&format!(
            "<h2 id=\"voting-record\">Voting record <span class=\"count\">{} division{}</span></h2>",
            votes.len(),
            if votes.len() == 1 { "" } else { "s" }
        ));
        body.push_str(&table_scroll("Voting record"));
        body.push_str("<table class=\"vote-record\"><thead data-pagefind-ignore><tr><th scope=\"col\">Date</th><th scope=\"col\">Division</th><th scope=\"col\">Vote</th><th scope=\"col\">Result</th></tr></thead>");
        fn month_of<'v>(vote: &data::PersonVote<'v>) -> &'v str {
            vote.division.date.get(..7).unwrap_or("")
        }
        for month in votes.chunk_by(|a, b| month_of(a) == month_of(b)) {
            body.push_str(&format!(
                "<tbody><tr class=\"month\"><th colspan=\"4\" scope=\"rowgroup\">{}</th></tr>",
                esc(&month_label(month_of(&month[0])))
            ));
            for v in month {
                body.push_str(&format!(
                    "<tr><td class=\"date\"><time datetime=\"{}\">{}</time></td><td><a href=\"/divisions/{}/{}/\">{}</a></td><td><span class=\"{}\">{}{}</span></td><td>{}</td></tr>",
                    esc_attr(&v.division.date),
                    esc(&short_date(&v.division.date)),
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
            body.push_str("</tbody>");
        }
        body.push_str("</table></div>");
    }

    if let Some(photo) = &person.photo {
        body.push_str(&format!(
            "<p class=\"attribution\">Photo: {} ({}), via Wikimedia Commons.</p>",
            esc(&photo.attribution),
            esc(&photo.licence)
        ));
    }
    body.push_str(&format!(
        "<p class=\"attribution\">Sources: Wikidata{}{}{}{}{}. Errors? <a href=\"/about/corrections/\">Request a correction.</a></p>",
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
        if expenses.is_some() {
            ", IPEA (CC BY 3.0 AU)"
        } else {
            ""
        },
        if funding.is_some() {
            ", AEC disclosures (CC BY 4.0)"
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
    page.lastmod = votes.iter().map(|v| v.division.date.clone()).max();
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
    body.push_str("<div class=\"filter-bar\"><span class=\"segmented\" id=\"division-house\" role=\"group\" aria-label=\"Filter by chamber\"><button aria-pressed=\"true\" data-house=\"\">Both chambers</button><button aria-pressed=\"false\" data-house=\"representatives\">House</button><button aria-pressed=\"false\" data-house=\"senate\">Senate</button></span><input type=\"search\" id=\"division-filter-text\" placeholder=\"Filter by division name\" enterkeyhint=\"done\" aria-label=\"Filter divisions\"></div>");
    body.push_str(&filter_feedback("No divisions match these filters."));
    if !data.divisions.is_empty() {
        // One entry per matter a chamber divided on in a sitting day, as on
        // home: a bill put fifteen times in a day reads as one story rather
        // than fifteen lines told apart only by their tallies. Series sit
        // where their newest division does, so each month is already one
        // run, and its divider counts divisions, not entries.
        let mut months: Vec<(&str, Vec<data::DivisionSeries>)> = Vec::new();
        for series in data.latest_series(usize::MAX) {
            let key = series.date.get(..7).unwrap_or(series.date);
            match months.last_mut() {
                Some((month, entries)) if *month == key => entries.push(series),
                _ => months.push((key, vec![series])),
            }
        }
        let counts: Vec<(&str, usize)> = months
            .iter()
            .map(|(month, entries)| (*month, entries.iter().map(|s| s.divisions.len()).sum()))
            .collect();
        body.push_str(&month_jump(&counts, "division"));
        body.push_str("<ul class=\"ledger\" id=\"division-list\">");
        for ((month, entries), (_, count)) in months.iter().zip(&counts) {
            body.push_str(&ledger_month(month, *count, "division"));
            for series in entries {
                body.push_str(&series_row_in_month(data, series));
            }
        }
        body.push_str("</ul>");
    } else {
        body.push_str("<p class=\"note\">No division records loaded yet. They appear once the They Vote For You sync runs.</p>");
    }
    const DESCRIPTION: &str =
        "Recorded votes of the Australian House of Representatives and Senate.";
    let mut page = Page::new(
        "Divisions",
        Some(DESCRIPTION.to_string()),
        "/divisions/",
        body,
    );
    page.lastmod = newest_division_date(data);
    page.page_script = Some(DIVISION_FILTER_JS.as_str());
    page.jsonld = Some(jsonld_script(vec![
        collection_page(
            &data.site_url,
            "Divisions",
            DESCRIPTION,
            "/divisions/",
            data.divisions.len(),
        ),
        breadcrumb(
            &data.site_url,
            &[("pollywiki", "/"), ("Divisions", "/divisions/")],
        ),
    ]));
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
    let sitting_day = data.sitting_day(division);
    // The markdown renderer escapes raw HTML, so the output is safe to inline.
    let summary_html = match (&division.summary, division.summary_kind) {
        (Some(summary), kind) if kind != Some(SummaryKind::Transcript) => {
            Some(markdown::to_html(summary))
        }
        _ => None,
    };
    // A vote can name someone the register has no page for: a member who
    // arrived too recently for Wikidata to have recorded the seat. The record
    // carries their name, so it renders as plain text rather than as a link
    // into nothing.
    let voter = |v: &pollywiki_schema::VoteCast| -> String {
        match data.person_by_slug(&v.person_slug) {
            Some(person) => format!(
                "<a href=\"/people/{}/\">{}</a>",
                person.slug,
                esc(&person.name)
            ),
            None => esc(&data.voter_name(v)),
        }
    };

    let mut body = String::new();
    body.push_str(&record_article("Division", Some(division.house)));
    body.push_str("<div class=\"meta-row\"><span class=\"ids\">");
    body.push_str(&chamber_chip(division.house));
    body.push_str(&format!(
        " Division {} · {}",
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
        "<div class=\"result-line\"><strong>{}</strong> <span class=\"figures\">{} aye · {} no</span>{}</div>",
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
        body.push_str("<aside class=\"context-box\"><div class=\"context-top\" data-pagefind-ignore><span class=\"context-head\">What this vote was about</span></div><div class=\"context-body\">");
        body.push_str(html);
        body.push_str(&format!(
            "</div><div class=\"context-credit\">Context written by <a href=\"{}\">They Vote For You</a> volunteers (ODbL), reproduced unchanged.</div></aside>",
            esc_attr(tvfy_link)
        ));
    }

    if let Some(ai) = &division.ai_summary {
        body.push_str("<aside class=\"context-box\"><div class=\"context-top\" data-pagefind-ignore><span class=\"context-head\">What this vote was about</span><span class=\"ai-tag\">AI-generated</span></div><div class=\"context-body\"><p>");
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

    body.push_str("<h2>By party</h2>");
    body.push_str(&table_scroll("By party"));
    body.push_str("<table><thead data-pagefind-ignore><tr><th scope=\"col\">Party</th><th class=\"num\" scope=\"col\">Aye</th><th class=\"num\" scope=\"col\">No</th></tr></thead><tbody>");
    for row in &breakdown {
        body.push_str(&format!(
            "<tr><td>{}</td><td class=\"{}\">{}</td><td class=\"{}\">{}</td></tr>",
            group_chip(
                data,
                row.party.map(|p| p.slug.as_str()).unwrap_or(""),
                &row.group,
                true
            ),
            if row.aye == 0 { "num zero" } else { "num" },
            row.aye,
            if row.no == 0 { "num zero" } else { "num" },
            row.no,
        ));
    }
    body.push_str("</tbody></table></div>");

    // Every name, grouped as the By party table counts them and in its order,
    // so a party's split reads straight off the list. The groups nest: a
    // group's label heads its own list and is never counted as a voter.
    body.push_str("<h2>Every vote</h2>");
    if division
        .votes
        .iter()
        .any(|v| v.against_group_majority == Some(true))
    {
        body.push_str("<p class=\"note\">\u{201c}Crossed\u{201d} marks a vote against the majority of the member's own party in this division.</p>");
    }
    body.push_str("<div class=\"vote-columns\">");
    // Each column heads its own list, so a screen reader can jump past up to
    // 150 aye votes straight to the noes.
    for (label, list) in [("AYE", &ayes), ("NO", &noes)] {
        body.push_str(&format!(
            "<div><h3 class=\"col-head\">{label} ({})</h3><ul>",
            list.len()
        ));
        for (row, votes) in data.votes_by_group(&breakdown, list) {
            // The chip gives the party's code, or its name where it has none,
            // and the count of names under it.
            let chip = group_chip_labelled(
                data,
                &row.slug,
                row.party
                    .map(|p| p.code.as_deref().unwrap_or(&p.name))
                    .unwrap_or(&row.group),
                &format!(" <span class=\"n\">{}</span>", votes.len()),
                false,
            );
            body.push_str(&format!("<li class=\"vote-group\">{chip}<ul>"));
            for v in votes {
                body.push_str(&format!(
                    "<li>{}{}</li>",
                    voter(v),
                    if v.against_group_majority == Some(true) {
                        "<span class=\"note\"> · crossed</span>"
                    } else {
                        ""
                    },
                ));
            }
            body.push_str("</ul></li>");
        }
        body.push_str("</ul></div>");
    }
    body.push_str("</div>");

    if sitting_day.len() > 1 {
        body.push_str(&format!(
            "<h2>Divisions this sitting day</h2><p class=\"note\">Every division the {} held that day, in order; the one on this page is marked.</p><ul class=\"ledger\">",
            chamber_word(division.house)
        ));
        for d in &sitting_day {
            body.push_str(&sitting_day_row(d, d.id == division.id));
        }
        body.push_str("</ul>");
    }
    body.push_str("</article>");

    // The page's sources. What "crossed" means is said beside the list that
    // uses the word, not down here.
    let mut sources = Vec::new();
    if let Some(tvfy) = &division.links.tvfy {
        sources.push(format!(
            "Record via <a href=\"{}\">They Vote For You</a> (ODbL).",
            esc_attr(tvfy)
        ));
    }
    if let Some(hansard) = &division.links.hansard {
        sources.push(format!(
            "Official transcript: <a href=\"{}\">Hansard</a>.",
            esc_attr(hansard)
        ));
    }
    let footer_note = (!sources.is_empty())
        .then(|| format!("<p class=\"attribution\">{}</p>", sources.join(" ")));

    let description = format!(
        "Division in the {}, {}: {} ayes, {} noes.",
        full_chamber(division.house),
        format_date(&division.date),
        division.ayes,
        division.noes
    );
    let path = format!("/divisions/{}/{}/", division.house, division_key(division));
    let mut page = Page::new(division.name.clone(), Some(description), path.clone(), body);
    page.footer_note = footer_note;
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
        "{} bills of the 48th Parliament, from the official APH record, most recent step first.",
        data.bills.len()
    ));
    body.push_str("</p></div>");
    body.push_str("<div class=\"filter-bar\"><span class=\"pills\" id=\"bill-status\" role=\"group\" aria-label=\"Filter by status\"><button aria-pressed=\"true\" data-status=\"\">All</button><button aria-pressed=\"false\" data-status=\"open\">Before parliament</button><button aria-pressed=\"false\" data-status=\"act\">Act</button><button aria-pressed=\"false\" data-status=\"other\">Other</button></span><input type=\"search\" id=\"bill-filter\" placeholder=\"Filter by title or portfolio\" enterkeyhint=\"done\" aria-label=\"Filter bills\"></div>");
    body.push_str(&filter_feedback("No bills match these filters."));
    if !data.bills.is_empty() {
        // The bundle arrives alphabetically, so the ledger order is built here.
        // The sort is stable, which keeps that alphabetical order inside a
        // month and among any bills the APH record has yet to date.
        let mut by_activity: Vec<&Bill> = data.bills.iter().collect();
        by_activity.sort_by(|a, b| {
            latest_step(b)
                .map(|s| s.date.as_str())
                .cmp(&latest_step(a).map(|s| s.date.as_str()))
        });
        // Sorted newest first, so each month is one run. Undated bills land in
        // a final run of their own; "undated" is not a month key, so it never
        // collides with one, and month_label passes it through to the divider.
        let mut months: Vec<(&str, Vec<&Bill>)> = Vec::new();
        for b in by_activity {
            let key = latest_step(b)
                .map(|s| s.date.get(..7).unwrap_or(s.date.as_str()))
                .unwrap_or("undated");
            if months.last().map(|(m, _)| *m) == Some(key) {
                months.last_mut().expect("just matched").1.push(b);
            } else {
                months.push((key, vec![b]));
            }
        }
        let counts: Vec<(&str, usize)> = months
            .iter()
            .map(|(month, rows)| (*month, rows.len()))
            .collect();
        body.push_str(&month_jump(&counts, "bill"));
        // The key comes before the rows it explains, not after the last of them.
        body.push_str(BILL_DOTS_LEGEND);
        body.push_str("<ul class=\"bill-list\" id=\"bill-rows\">");
        // Column labels for the eye. Each row already says what its cells are
        // (the date's title, the dots' label), so the header stays out of the
        // accessibility tree, and with neither data-month nor data-status the
        // filter passes over it. The date is the bill's latest step, and the
        // label says so; "Moved" read as the day the bill was introduced.
        body.push_str("<li class=\"bill-head\" aria-hidden=\"true\"><span>Latest</span><span>Bill</span><span>Origin</span><span>Stage</span><span>Status</span></li>");
        for (month, rows) in &months {
            body.push_str(&ledger_month(month, rows.len(), "bill"));
            for b in rows {
                body.push_str(&bill_row(b, true));
            }
        }
        body.push_str("</ul>");
    } else {
        body.push_str("<p class=\"note\">No bill records loaded yet. They appear once the APH bills sync runs.</p>");
    }
    const DESCRIPTION: &str = "Bills before the Australian federal parliament and their progress.";
    let mut page = Page::new("Bills", Some(DESCRIPTION.to_string()), "/bills/", body);
    page.lastmod = newest_bill_date(data);
    page.page_script = Some(BILL_FILTER_JS.as_str());
    page.jsonld = Some(jsonld_script(vec![
        collection_page(
            &data.site_url,
            "Bills",
            DESCRIPTION,
            "/bills/",
            data.bills.len(),
        ),
        breadcrumb(&data.site_url, &[("pollywiki", "/"), ("Bills", "/bills/")]),
    ]));
    page
}

static LEADING_AND_SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^and ").unwrap());
static SUMMARY_SPLIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r";\s+").unwrap());

/// The chamber a timeline step names, from the "(Chamber)" the ingest
/// writes after it. Steps outside either chamber, such as assent, name none.
fn step_chamber(event: &str) -> Option<House> {
    if event.ends_with(" (Senate)") {
        Some(House::Senate)
    } else if event.ends_with(" (House of Representatives)") {
        Some(House::Representatives)
    } else {
        None
    }
}

/// A bill's progress as the chamber phases it passed through: each run of
/// steps in one chamber is its own row group, headed by the chamber, and
/// its steps drop the chamber they would otherwise repeat. A run of steps
/// that names no chamber gets a group with no header, so a heading never
/// claims assent, or a step off the Notice Paper, for a chamber the record
/// does not name.
fn progress_phases(bill: &Bill) -> String {
    let mut out = String::new();
    for run in bill
        .timeline
        .chunk_by(|a, b| step_chamber(&a.event) == step_chamber(&b.event))
    {
        out.push_str("<tbody>");
        if let Some(house) = step_chamber(&run[0].event) {
            out.push_str(&format!(
                "<tr class=\"phase\"><th colspan=\"2\" scope=\"rowgroup\">{}</th></tr>",
                chamber_chip(house)
            ));
        }
        for step in run {
            out.push_str(&format!(
                "<tr><td class=\"date\">{}</td><td>{}</td></tr>",
                esc(&format_date(&step.date)),
                esc(event_description(&step.event))
            ));
        }
        out.push_str("</tbody>");
    }
    out
}

/// What a division on a bill's page is called there. Under the bill's own
/// heading, a division on this bill alone reads as its stage. A division
/// that names other bills too, as a cognate debate's does, keeps its whole
/// name: the stage alone would hide what else the vote decided.
fn bill_division_label<'d>(bill: &Bill, division: &'d Division) -> &'d str {
    match (
        data::division_matter(&division.name).strip_prefix("Bills \u{2014} "),
        data::division_stage(&division.name),
    ) {
        (Some(matter), Some(stage)) if matter == bill.title => stage,
        _ => &division.name,
    }
}

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
    body.push_str(&record_article("Bill", Some(bill.chamber)));
    body.push_str("<div class=\"meta-row\"><span class=\"ids\">");
    body.push_str(&chamber_chip(bill.chamber));
    body.push(' ');
    body.push_str(&esc(&bill.id));
    body.push_str(
        "</span> <span class=\"status\"><span class=\"label\">Status</span> <span class=\"value\">",
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
        body.push_str("<div class=\"meta-line\">");
        if let Some(t) = &bill.bill_type {
            body.push_str(&format!(" <span>{}</span>", esc(t)));
        }
        if let Some(portfolio) = &bill.portfolio {
            body.push_str(&format!(
                " <span><strong>Portfolio:</strong> {}</span>",
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
                " <span><strong>Sponsor{}:</strong> {}</span>",
                if bill.sponsors.len() > 1 { "s" } else { "" },
                raiser_links(&bill.sponsors)
            ));
        } else if !bill.movers.is_empty() {
            body.push_str(&format!(
                " <span><strong>Introduced by</strong> {}</span>",
                raiser_links(&bill.movers)
            ));
        }
        body.push_str("</div>");
    }

    if bill.summary.is_some() {
        if let Some(groups) = &summary_groups {
            body.push_str("<div class=\"lede bill-summary\"><p>Amends:</p>");
            for group in groups {
                body.push_str(&format!("<div class=\"act\"><p>{}</p>", esc(&group.acts)));
                if !group.items.is_empty() {
                    body.push_str("<ul>");
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
                "<div class=\"lede bill-summary\"><p>{}</p><p class=\"note\">Summary from the official bill homepage.</p></div>",
                esc(bill.summary.as_deref().unwrap_or(""))
            ));
        } else {
            body.push_str("<div class=\"lede bill-summary\"><p>");
            body.push_str(&esc(summary_lead.unwrap_or("")));
            body.push_str("</p><ul>");
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
            "<aside class=\"context-box\"><div class=\"context-top\" data-pagefind-ignore><span class=\"context-head\">What this bill is about</span><span class=\"ai-tag\">AI-generated</span></div><div class=\"context-body\"><p>{}</p></div><div class=\"context-credit\">Written by AI to explain the official summary in plain terms; specifics come from that summary only. May contain errors — the record is authoritative. <a href=\"/about/methodology/\">How this works.</a></div></aside>",
            esc(&ai.text)
        ));
    }

    if let Some(act) = &bill.act {
        body.push_str("<h2 id=\"act\">Became law</h2>");
        body.push_str(&table_scroll("Became law"));
        body.push_str("<table class=\"facts\"><tbody>");
        body.push_str(&format!(
            "<tr><th class=\"label\" scope=\"row\">Act</th><td><a href=\"https://www.legislation.gov.au/{}/asmade/text\">{}</a></td></tr>",
            esc_attr(&act.id),
            esc(&act.name)
        ));
        if let (Some(number), Some(year)) = (act.number, act.year) {
            body.push_str(&format!(
                "<tr><th class=\"label\" scope=\"row\">Number</th><td>No. {number}, {year}</td></tr>"
            ));
        }
        if let Some(assent) = &act.assent {
            body.push_str(&format!(
                "<tr><th class=\"label\" scope=\"row\">Assent</th><td>{}</td></tr>",
                esc(&format_date(assent))
            ));
        }
        body.push_str(&format!(
            "<tr><th class=\"label\" scope=\"row\">Status</th><td>{}</td></tr>",
            esc(&register_status(&act.status))
        ));
        body.push_str("</tbody></table></div>");
        if act.status != "InForce" {
            body.push_str("<p class=\"note\">The status is the Register's own. An amending Act is spent once its amendments take effect; the changes live on in the Acts it amended.</p>");
        }
        // The Register's licence asks for this wording, dated by the sync.
        let synced = data
            .meta
            .sources
            .get("legislation")
            .map(|s| {
                format!(
                    " at {}",
                    format_date(&s.last_sync.chars().take(10).collect::<String>())
                )
            })
            .unwrap_or_default();
        body.push_str(&format!(
            "<p class=\"attribution\">Sourced from the Federal Register of Legislation{synced}. For the latest information on Australian Government law please go to <a href=\"https://www.legislation.gov.au\">https://www.legislation.gov.au</a>.</p>"
        ));
    }

    if !bill.timeline.is_empty() {
        body.push_str("<h2>Progress</h2>");
        body.push_str(&table_scroll("Progress"));
        body.push_str("<table class=\"progress\"><thead data-pagefind-ignore><tr><th scope=\"col\">Date</th><th scope=\"col\">Step</th></tr></thead>");
        body.push_str(&progress_phases(bill));
        body.push_str("</table></div>");
    }

    if !related.is_empty() {
        body.push_str("<h2>Divisions on this bill</h2><ul class=\"ledger\">");
        for d in &related {
            body.push_str(&ledger_row_labelled(d, bill_division_label(bill, d)));
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
    body.push_str("<div class=\"masthead\"><h1>Electorates</h1><p class=\"lede\">");
    body.push_str(&format!(
        "{} House of Representatives seats.",
        data.electorates.len()
    ));
    body.push_str("</p></div>");
    body.push_str("<div class=\"filter-bar\"><input type=\"search\" id=\"electorate-filter\" placeholder=\"Filter by name, state or member\" enterkeyhint=\"done\" aria-label=\"Filter electorates\"></div>");
    body.push_str(&filter_feedback("No electorates match these filters."));
    body.push_str(&table_scroll("Electorates"));
    body.push_str("<table id=\"electorate-table\"><thead><tr><th scope=\"col\">Electorate</th><th scope=\"col\">State</th><th scope=\"col\">Member</th></tr></thead><tbody>");
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

    const DESCRIPTION: &str = "All 150 federal electorates and their members.";
    let mut page = Page::new(
        "Electorates",
        Some(DESCRIPTION.to_string()),
        "/electorates/",
        body,
    );
    page.page_script = Some(ELECTORATE_FILTER_JS.as_str());
    page.jsonld = Some(jsonld_script(vec![
        collection_page(
            &data.site_url,
            "Electorates",
            DESCRIPTION,
            "/electorates/",
            data.electorates.len(),
        ),
        breadcrumb(
            &data.site_url,
            &[("pollywiki", "/"), ("Electorates", "/electorates/")],
        ),
    ]));
    page
}

/// Both result tables on an electorate page open with one head and one set
/// of columns, so the deciding count and the first preferences under it
/// read down the same grid.
const RESULT_HEAD: &str = "<colgroup><col class=\"cand\"><col class=\"party\"><col class=\"votes\"><col class=\"pct\"><col class=\"swing\"></colgroup><thead data-pagefind-ignore><tr><th scope=\"col\">Candidate</th><th scope=\"col\">Party</th><th class=\"num\" scope=\"col\">Votes</th><th class=\"num\" scope=\"col\">%</th><th class=\"num\" scope=\"col\">Swing</th></tr></thead>";

/// A dollar figure rounded to the dollar, "-$12" for a credit.
fn dollars(amount: f64) -> String {
    let whole = amount.round() as i64;
    if whole < 0 {
        format!("-${}", locale_int(-whole))
    } else {
        format!("${}", locale_int(whole))
    }
}

/// The Register's status words, "InForce" or "NotYetInForce", as a phrase.
fn register_status(status: &str) -> String {
    let mut out = String::new();
    for (i, c) in status.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push(' ');
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Votes as a percentage of a total, or nothing of nothing.
fn share(votes: i64, total: i64) -> f64 {
    if total > 0 {
        votes as f64 / total as f64 * 100.0
    } else {
        0.0
    }
}

/// A swing to the AEC's two places, signed when it is a gain; blank where
/// the AEC published none.
fn signed(swing: Option<JsNum>) -> String {
    match swing {
        Some(swing) => format!(
            "{}{}",
            if swing.0 > 0.0 { "+" } else { "" },
            to_fixed(swing.0, 2)
        ),
        None => String::new(),
    }
}

/// One candidate in a result table, at the share the caller worked out.
fn result_row(c: &CandidateResult, share: f64) -> String {
    format!(
        "<tr><td>{}{}</td><td>{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
        esc(&c.name),
        if c.elected { " ✓" } else { "" },
        esc(&c.party),
        locale_int(c.votes),
        to_fixed(share, 2),
        signed(c.swing),
    )
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
    body.push_str(&record_article("Electorate", None));
    body.push_str(&format!(
        "<div class=\"masthead\"><h1 data-pagefind-meta=\"title\">{}</h1><p class=\"motto\">Federal electorate · {}</p></div>",
        esc(&electorate.name),
        esc(state_name(electorate.state.as_str()).unwrap_or(electorate.state.as_str())),
    ));

    let profile = electorate.profile.as_ref();
    if let Some(derivation) = profile.and_then(|p| p.name_derivation.as_deref()) {
        body.push_str(&format!("<p class=\"lede\">{}</p>", esc(derivation)));
    }
    if let Some(location) = profile.and_then(|p| p.location.as_deref()) {
        body.push_str(&format!(
            "<p class=\"note covers\"><strong>Covers:</strong> {}</p>",
            esc(location)
        ));
    }
    if let Some(map) = &electorate.boundary {
        body.push_str(&format!(
            "<figure class=\"boundary-map\"><svg viewBox=\"{}\" role=\"img\" aria-labelledby=\"map-title\"><title id=\"map-title\">Map of {} and the divisions around it</title><g class=\"neighbours\">",
            esc_attr(&map.view_box),
            esc(&electorate.name)
        ));
        for n in &map.neighbours {
            body.push_str(&format!("<path d=\"{}\"/>", esc_attr(&n.path)));
        }
        body.push_str(&format!(
            "</g><path class=\"division\" d=\"{}\"/><g class=\"labels\" aria-hidden=\"true\">",
            esc_attr(&map.path)
        ));
        for n in &map.neighbours {
            if let Some((x, y)) = n.label {
                body.push_str(&format!(
                    "<text x=\"{x}\" y=\"{y}\">{}</text>",
                    esc(&n.name)
                ));
            }
        }
        body.push_str(&format!(
            "</g></svg><figcaption>Boundary as the ABS approximates it ({}), © Commonwealth of Australia (ABS), CC BY 4.0. The AEC's own maps show the legal boundary.</figcaption></figure>",
            esc(&map.set)
        ));
    }
    if profile.is_some() || electorate.enrolment.is_some() || electorate.established.is_some() {
        body.push_str(&table_scroll("Electorate profile"));
        body.push_str("<table class=\"facts\"><tbody>");
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
        if let Some(established) = &electorate.established {
            body.push_str(&format!(
                "<tr><th class=\"label\" scope=\"row\">Established</th><td>{}</td></tr>",
                esc(&format_date(established))
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
        body.push_str("<h2>Current member</h2><div class=\"current-member\">");
        body.push_str(&person_card(data, member));
        body.push_str("</div>");
    }

    if let Some(result) = result {
        if tcp.len() == 2 {
            let heading = format!("{}: two-candidate preferred", result.event_name);
            body.push_str(&format!("<h2>{}</h2>", esc(&heading)));
            body.push_str(&table_scroll(&heading));
            body.push_str(&format!("<table class=\"results\">{RESULT_HEAD}<tbody>"));
            for c in tcp {
                body.push_str(&result_row(c, share(c.votes, tcp_total)));
            }
            body.push_str("</tbody></table></div>");
        }
        if result.first_prefs.iter().any(|c| !c.is_informal()) {
            // Each share is worked out from the votes over the formal total,
            // as the AEC does, so a result stored with the informal ballots
            // counted in still reads right. The informal ballots are no
            // candidate's: they close the table, as a share of every ballot
            // cast, beside the AEC's swing in informality.
            let formal = result.formal_votes();
            let heading = format!("{}: first preferences", result.event_name);
            body.push_str(&format!("<h2>{}</h2>", esc(&heading)));
            body.push_str(&table_scroll(&heading));
            body.push_str(&format!("<table class=\"results\">{RESULT_HEAD}<tbody>"));
            for c in result.first_prefs.iter().filter(|c| !c.is_informal()) {
                body.push_str(&result_row(c, share(c.votes, formal)));
            }
            body.push_str("</tbody>");
            let informal = result.informal();
            if let Some(informal) = informal {
                body.push_str(&format!(
                    "<tfoot><tr><td colspan=\"2\">Informal ballots</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr></tfoot>",
                    locale_int(informal.votes),
                    to_fixed(share(informal.votes, formal + informal.votes), 2),
                    signed(informal.swing),
                ));
            }
            body.push_str("</table></div>");
            if informal.is_some() {
                body.push_str("<p class=\"note\">Candidate percentages are of formal votes; the informal share is of all ballots cast.</p>");
            }
        }
    }

    if let Some(history) = electorate.history.as_deref().filter(|h| !h.is_empty()) {
        body.push_str("<h2 id=\"past-results\">Results at each election</h2>");
        body.push_str(&table_scroll("Results at each election"));
        body.push_str("<table><thead data-pagefind-ignore><tr><th scope=\"col\">Election</th><th scope=\"col\">Elected</th><th scope=\"col\">Party</th><th class=\"num\" scope=\"col\">Two-candidate %</th></tr></thead><tbody>");
        for h in history {
            let member = match &h.person_slug {
                Some(slug) => format!("<a href=\"/people/{slug}/\">{}</a>", esc(&h.member)),
                None => esc(&h.member),
            };
            body.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td class=\"num\">{}</td></tr>",
                esc(&h.event_name),
                member,
                esc(&h.party),
                h.tcp_pct.map(|p| to_fixed(p.0, 2)).unwrap_or_default(),
            ));
        }
        body.push_str("</tbody></table></div><p class=\"note\">Every contest for a division of this name since 2004, each on the boundaries in force at the time. The two-candidate figure is the winner's share of the final count.</p>");
    }

    body.push_str(&format!(
        "<p class=\"attribution\">Election figures and profile © Commonwealth of Australia (AEC), CC BY 4.0.{} ✓ marks the elected candidate.</p>",
        if electorate.established.is_some() {
            " Date established from the Parliamentary Handbook."
        } else {
            ""
        }
    ));
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
    body.push_str("<div class=\"masthead\"><h1>Parties</h1><p class=\"lede\">Parliamentary groups of the 48th Parliament. Grouping follows the official record; Coalition members sit as one parliamentary group.</p></div>");
    body.push_str(&table_scroll("Parties"));
    body.push_str("<table><thead><tr><th scope=\"col\">Group</th><th class=\"num\" scope=\"col\">House</th><th class=\"num\" scope=\"col\">Senate</th><th class=\"num\" scope=\"col\">Total</th></tr></thead><tbody>");
    for p in &data.parties {
        body.push_str(&format!(
            "<tr><td>{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
            group_chip(data, &p.slug, &p.name, true),
            p.seats.as_ref().map(|s| s.representatives).unwrap_or(0),
            p.seats.as_ref().map(|s| s.senate).unwrap_or(0),
            data::seat_total(p),
        ));
    }
    body.push_str("</tbody></table></div>");
    const DESCRIPTION: &str = "Parliamentary groups of the 48th Parliament and their seat counts.";
    let mut page = Page::new("Parties", Some(DESCRIPTION.to_string()), "/parties/", body);
    page.jsonld = Some(jsonld_script(vec![
        collection_page(
            &data.site_url,
            "Parties",
            DESCRIPTION,
            "/parties/",
            data.parties.len(),
        ),
        breadcrumb(
            &data.site_url,
            &[("pollywiki", "/"), ("Parties", "/parties/")],
        ),
    ]));
    page
}

// Every office is anchored to the start of the role, so a junior post named
// after one ("Assistant Minister to the Prime Minister") is not taken for the
// office itself. Whips alone match anywhere: the Handbook writes "Chief
// Government Whip", "Opposition Whip in the Senate" and so on.
static LEADERSHIP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(prime minister|deputy prime minister|leader|deputy leader|manager of (government|opposition) business|president of the senate|deputy president|speaker|deputy speaker)|whip").unwrap()
});

/// Whether a Handbook role belongs in a party's parliamentary leadership table.
pub(crate) fn is_leadership_role(role: &str) -> bool {
    LEADERSHIP.is_match(role)
}

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
            if p.to.is_none() && is_leadership_role(&p.role) {
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
    body.push_str(&record_article("Party", None));
    body.push_str(&format!(
        "<div class=\"masthead\"><h1 data-pagefind-meta=\"title\">{}</h1><p class=\"motto\">{} House {} · {} Senate {}</p></div>",
        group_chip(data, &party.slug, &party.name, false),
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
        body.push_str(&table_scroll("Party links"));
        body.push_str("<table class=\"facts\"><tbody>");
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
        body.push_str("<h2>Parliamentary leadership</h2>");
        body.push_str(&table_scroll("Parliamentary leadership"));
        body.push_str("<table><thead data-pagefind-ignore><tr><th scope=\"col\">Role</th><th scope=\"col\">Member</th><th class=\"num\" scope=\"col\">Since</th></tr></thead><tbody>");
        for l in &leadership {
            body.push_str(&format!(
                "<tr><td>{}</td><td><a href=\"/people/{}/\">{}</a></td>{}</tr>",
                esc(l.role),
                l.person.slug,
                esc(&l.person.name),
                // The Handbook leaves some start dates out. The cell says so,
                // with a dash to the eye and in words to a screen reader,
                // rather than sitting empty like a rendering fault.
                match l.from {
                    Some(from) => format!("<td class=\"num\">{}</td>", esc(&format_date(from))),
                    None => "<td class=\"num zero\"><span aria-hidden=\"true\">\u{2014}</span><span class=\"visually-hidden\">Not recorded</span></td>".to_string(),
                },
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

    let state_seats = data.state_seats_of_party(&party.slug);
    if !state_seats.is_empty() {
        body.push_str("<h2 id=\"states\">State and territory parliaments</h2>");
        body.push_str(&table_scroll("State and territory seats"));
        body.push_str("<table><thead data-pagefind-ignore><tr><th scope=\"col\">Chamber</th><th class=\"num\" scope=\"col\">Seats</th><th class=\"num\" scope=\"col\">Of</th></tr></thead><tbody>");
        for (parliament, chamber, seats) in &state_seats {
            body.push_str(&format!(
                "<tr><td><a href=\"{}#{}\">{} {}</a></td><td class=\"num\">{seats}</td><td class=\"num\">{}</td></tr>",
                state_members_path(parliament.state),
                chamber.slug,
                parliament.state,
                esc(&chamber.name),
                chamber.seats
            ));
        }
        body.push_str("</tbody></table></div>");
        body.push_str("<p class=\"note\">Members each parliament lists under this party or its state branch. <a href=\"/states/\">Every state and territory parliament.</a></p>");
    }

    if let Some(funding) = party.funding.as_ref().filter(|f| !f.returns.is_empty()) {
        body.push_str("<h2 id=\"funding\">Funding disclosures</h2>");
        body.push_str(&table_scroll("Annual returns"));
        // One row group per financial year, headed by it, newest first.
        body.push_str("<table><thead data-pagefind-ignore><tr><th scope=\"col\">Return</th><th class=\"num\" scope=\"col\">Receipts</th><th class=\"num\" scope=\"col\">Payments</th><th class=\"num\" scope=\"col\">Debts</th></tr></thead>");
        for year in funding.returns.chunk_by(|a, b| a.year == b.year) {
            body.push_str(&format!(
                "<tbody><tr class=\"month\"><th colspan=\"4\" scope=\"rowgroup\">{}</th></tr>",
                esc(&year[0].year)
            ));
            for r in year {
                body.push_str(&format!(
                    "<tr><td>{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
                    esc(&r.name),
                    dollars(r.receipts as f64),
                    dollars(r.payments as f64),
                    dollars(r.debts as f64)
                ));
            }
            body.push_str("</tbody>");
        }
        body.push_str("</table></div>");
        if let (Some(year), false) = (&funding.donations_year, funding.donations.is_empty()) {
            let total: i64 = funding.donations.iter().map(|d| d.value).sum();
            let donors = funding.donations.len();
            body.push_str(&format!(
                "<details class=\"donations\"><summary>Donations itemised in {} returns: {} from {} donor{}</summary>",
                esc(year),
                dollars(total as f64),
                locale_int(donors as i64),
                if donors == 1 { "" } else { "s" }
            ));
            body.push_str(&table_scroll("Donations itemised"));
            body.push_str("<table><thead data-pagefind-ignore><tr><th scope=\"col\">Donor</th><th class=\"num\" scope=\"col\">Gifts</th><th class=\"num\" scope=\"col\">Value</th></tr></thead><tbody>");
            for d in &funding.donations {
                body.push_str(&format!(
                    "<tr><td>{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
                    esc(&d.donor),
                    locale_int(d.gifts),
                    dollars(d.value as f64)
                ));
            }
            body.push_str("</tbody></table></div></details>");
        }
        body.push_str("<p class=\"note\">Annual returns as lodged with the AEC. A party's branches lodge their own and pass money between themselves, so the returns are listed, not added together. Only gifts above the disclosure threshold are itemised; donations are those the returns list as donations, summed by donor across the party's returns for the latest year. <a href=\"https://transparency.aec.gov.au\">AEC Transparency Register.</a></p>");
        body.push_str("<p class=\"attribution\">Funding disclosures © Commonwealth of Australia (AEC), CC BY 4.0.</p>");
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

/// A state chamber's seats by party. The key links a party wherever the site
/// has a page for it, which is wherever it sits in federal parliament.
fn state_chamber_bar(data: &SiteData, chamber: &StateChamber) -> String {
    composition_bar(
        &chamber.name,
        &state_segments(data, chamber),
        chamber.vacancies(),
    )
}

/// A state chamber's parties by seats held, as the derive step ordered them.
fn state_segments<'a>(data: &SiteData, chamber: &'a StateChamber) -> Vec<BarSegment<'a>> {
    chamber
        .parties
        .iter()
        .map(|p| BarSegment {
            name: &p.party,
            code: p.code.as_deref().unwrap_or(&p.party),
            colour: p.colour.as_deref().unwrap_or(GROUP_FALLBACK_COLOUR),
            seats: p.seats,
            href: data
                .party_by_slug(&p.party_slug)
                .map(|_| format!("/parties/{}/", p.party_slug)),
        })
        .collect()
}

/// A member's party as its chip, named and coloured as the chamber's bar
/// has it, and linked as the bar's key is.
fn state_party_chip(data: &SiteData, chamber: &StateChamber, member: &StateMember) -> String {
    let count = chamber
        .parties
        .iter()
        .find(|p| p.party_slug == member.party_slug);
    let colour = count
        .and_then(|c| c.colour.as_deref())
        .unwrap_or(GROUP_FALLBACK_COLOUR);
    let label = count.map_or(member.party.as_str(), |c| c.party.as_str());
    let inner = format!(
        "<span class=\"dot\" style=\"background:{colour}\" aria-hidden=\"true\"></span>{}",
        esc(label)
    );
    match data.party_by_slug(&member.party_slug) {
        Some(_) => format!(
            "<a class=\"group-chip\" href=\"/parties/{}/\">{inner}</a>",
            member.party_slug
        ),
        None => format!("<span class=\"group-chip\">{inner}</span>"),
    }
}

/// The chamber as a parliament's members speak of it: "Assembly", "Council".
fn chamber_short(chamber: &StateChamber) -> &str {
    chamber.name.rsplit(' ').next().unwrap_or(&chamber.name)
}

/// A state chamber as a chip, coloured as the federal chambers are: green for
/// the house government is formed in, red for the house of review.
fn state_chamber_chip(state: StateCode, chamber: &StateChamber) -> String {
    format!(
        "<span class=\"chip {}\">{} {}</span>",
        match chamber.kind {
            ChamberKind::Lower => "house",
            ChamberKind::Upper => "senate",
        },
        state.as_str(),
        esc(chamber_short(chamber))
    )
}

/// What a member sits for: their seat, or the chamber itself for a member
/// elected state-wide.
fn state_seat(chamber: &StateChamber, member: &StateMember) -> String {
    match &member.seat {
        Some(seat) => format!("Member for {seat}"),
        None => format!("Member of the {}", chamber.name),
    }
}

/// "1 seat", "2 seats": a count with its noun.
fn counted(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

pub fn states_index(data: &SiteData) -> Page {
    let mut body = String::new();
    body.push_str("<div class=\"masthead\"><h1>States and territories</h1><p class=\"lede\">The eight state and territory parliaments as they stand: who sits in each chamber, for which party, and who leads the government, from the member lists each parliament publishes.</p></div>");
    body.push_str("<h2 class=\"visually-hidden\">The eight parliaments</h2>");
    body.push_str(&parliament_ledger(data, false));
    for code in pollywiki_schema::STATES {
        let Some(state) = StateCode::parse(code) else {
            continue;
        };
        let name = state_name(code).unwrap_or(code);
        let id = code.to_lowercase();
        let record = data.state_parliament(state);
        let Some(parliament) = record.filter(|p| !p.chambers.is_empty()) else {
            // Not read: the reason where there is one, and the parliament's
            // own site, which lists its members for a person to read.
            let reason = record.and_then(|p| p.unavailable.as_deref()).unwrap_or(
                "No member list loaded yet. This parliament appears here once its sync runs.",
            );
            let website = match record {
                Some(p) => {
                    let label = WEBSITE_PREFIX.replace(&p.url, "");
                    format!(
                        " Its members are listed at <a href=\"{}\">{}</a>.",
                        esc_attr(&p.url),
                        esc(label.strip_suffix('/').unwrap_or(&label))
                    )
                }
                None => String::new(),
            };
            body.push_str(&format!(
                "<section aria-labelledby=\"{id}\"><h2 id=\"{id}\"><a href=\"{}\">{}</a></h2><p class=\"note\">{}{website}</p></section>",
                state_path(state),
                esc(name),
                esc(reason)
            ));
            continue;
        };
        let members: usize = parliament.chambers.iter().map(|c| c.members.len()).sum();
        body.push_str(&format!(
            "<section aria-labelledby=\"{id}\"><div class=\"section-head\"><h2 id=\"{id}\"><a href=\"{}\">{}</a></h2><a href=\"{}\">All {} \u{2192}</a></div>",
            state_path(state),
            esc(name),
            state_members_path(state),
            counted(members, "member"),
        ));
        if let Some((_, leader)) = parliament.leader() {
            body.push_str(&format!(
                "<p class=\"note\">{}: <a href=\"{}\">{}</a></p>",
                esc(&parliament.leader_title),
                state_member_path(state, &leader.slug),
                esc(&leader.name)
            ));
        }
        for chamber in &parliament.chambers {
            body.push_str(&state_chamber_bar(data, chamber));
        }
        body.push_str("</section>");
    }
    body.push_str("<p class=\"note\">Seats are counted by the party each parliament lists a member under, and a chamber's majority is of all its seats, empty ones included. <a href=\"/about/methodology/#states\">How the state figures are read.</a></p>");

    const DESCRIPTION: &str = "The eight Australian state and territory parliaments: members, parties and who leads each government.";
    let mut page = Page::new(
        "States and territories",
        Some(DESCRIPTION.to_string()),
        "/states/",
        body,
    );
    page.jsonld = Some(jsonld_script(vec![
        collection_page(
            &data.site_url,
            "States and territories",
            DESCRIPTION,
            "/states/",
            data.listed_states().count(),
        ),
        breadcrumb(
            &data.site_url,
            &[("pollywiki", "/"), ("States and territories", "/states/")],
        ),
    ]));
    page
}

/// "127 sitting members: 87 in the Legislative Assembly and 40 in the
/// Legislative Council."
fn state_members_lede(parliament: &StateParliament) -> String {
    let members: usize = parliament.chambers.iter().map(|c| c.members.len()).sum();
    match parliament.chambers.as_slice() {
        [only] => format!(
            "{} in a single chamber, the {}.",
            counted(members, "sitting member"),
            only.name
        ),
        chambers => format!(
            "{}: {}.",
            counted(members, "sitting member"),
            chambers
                .iter()
                .map(|c| format!("{} in the {}", c.members.len(), c.name))
                .collect::<Vec<_>>()
                .join(" and ")
        ),
    }
}

/// The parliament's website, named by its host.
fn website_link(url: &str) -> String {
    let label = WEBSITE_PREFIX.replace(url, "");
    format!(
        "<a href=\"{}\">{}</a>",
        esc_attr(url),
        esc(label.strip_suffix('/').unwrap_or(&label))
    )
}

/// The member lists a parliament's figures come from, credited.
fn state_lists_attribution(parliament: &StateParliament) -> String {
    format!(
        "<p class=\"attribution\">Source: {}, member lists ({}). Parliamentary material reproduced fairly and accurately with acknowledgement. Errors? <a href=\"/about/corrections/\">Request a correction.</a></p>",
        esc(&parliament.name),
        parliament
            .chambers
            .iter()
            .map(|c| format!("<a href=\"{}\">{}</a>", esc_attr(&c.source), esc(&c.name)))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// A state or territory parliament's front page: who leads it, how its
/// chambers divide, and where to go next. A parliament whose lists cannot be
/// read still has one, saying why and where its members are listed.
pub fn state_page(data: &SiteData, parliament: &StateParliament) -> Page {
    let state = parliament.state;
    let path = state_path(state);
    let read = !parliament.chambers.is_empty();

    let mut body = String::new();
    body.push_str(&record_article("Parliament", None));
    let lede = if read {
        state_members_lede(parliament)
    } else {
        parliament.unavailable.clone().unwrap_or_else(|| {
            "No member list loaded yet. Its members appear here once its sync runs.".to_string()
        })
    };
    body.push_str(&format!(
        "<div class=\"masthead\"><h1 data-pagefind-meta=\"title\">{}</h1><p class=\"lede\">{}</p></div>",
        esc(&parliament.name),
        esc(&lede)
    ));

    body.push_str(&table_scroll("Parliament facts"));
    body.push_str("<table class=\"facts\"><tbody>");
    if let Some((chamber, leader)) = parliament.leader() {
        body.push_str(&format!(
            "<tr><th class=\"label\" scope=\"row\">{}</th><td><a href=\"{}\">{}</a>, {}</td></tr>",
            esc(&parliament.leader_title),
            state_member_path(state, &leader.slug),
            esc(&leader.name),
            esc(&state_seat(chamber, leader))
        ));
    }
    body.push_str(&format!(
        "<tr><th class=\"label\" scope=\"row\">Website</th><td>{}</td></tr>",
        website_link(&parliament.url)
    ));
    if let Some(listed) = &parliament.listed_at {
        body.push_str(&format!(
            "<tr><th class=\"label\" scope=\"row\">Members listed</th><td>{}</td></tr>",
            esc(&format_date(&listed.chars().take(10).collect::<String>()))
        ));
    }
    body.push_str("</tbody></table></div>");
    if let Some(note) = &parliament.hand_listed {
        body.push_str(&format!(
            "<p class=\"procedure-note\"><strong>Downloaded by hand:</strong> {}</p>",
            esc(note)
        ));
    }

    if read {
        body.push_str("<h2 id=\"composition\">Composition</h2>");
        for chamber in &parliament.chambers {
            body.push_str(&state_chamber_bar(data, chamber));
        }
        let members: usize = parliament.chambers.iter().map(|c| c.members.len()).sum();
        body.push_str(&format!(
            "<div class=\"section-head\"><h2 id=\"members\">Members</h2><a href=\"{}\">All {} \u{2192}</a></div>",
            state_members_path(state),
            counted(members, "member"),
        ));
        body.push_str("<ul class=\"chamber-links\">");
        for chamber in &parliament.chambers {
            body.push_str(&format!(
                "<li><a href=\"{}#{}\">{}</a> <span class=\"note\">{} of {}</span></li>",
                state_members_path(state),
                chamber.slug,
                esc(&chamber.name),
                counted(chamber.members.len(), "member"),
                counted(chamber.seats as usize, "seat"),
            ));
        }
        body.push_str("</ul>");
    } else {
        body.push_str(&format!(
            "<p class=\"note\">Its members are listed on its own website, {}.</p>",
            website_link(&parliament.url)
        ));
    }
    if !parliament.bills.is_empty() {
        body.push_str(&format!(
            "<div class=\"section-head\"><h2 id=\"bills\">Latest bill activity</h2><a href=\"{}\">All {} \u{2192}</a></div>",
            state_bills_path(state),
            counted(parliament.bills.len(), "bill"),
        ));
        body.push_str(&state_bill_activity(parliament, 6));
    }
    if read {
        body.push_str(&state_lists_attribution(parliament));
    }
    body.push_str(&state_bills_attribution(parliament));
    body.push_str("</article>");

    let description = if read {
        format!(
            "{}: who leads the government and how its chambers divide, from the parliament's own member lists.",
            parliament.name
        )
    } else {
        format!(
            "{}: why its member list is not shown here yet.",
            parliament.name
        )
    };
    // No lastmod on any state page: the lists are read every night, and the
    // date of a reading would claim the page changed when only the reading did.
    let mut page = Page::new(
        parliament.name.clone(),
        Some(description),
        path.clone(),
        body,
    );
    page.jsonld = Some(jsonld_script(vec![
        serde_json::json!({
            "@context": "https://schema.org",
            "@type": "GovernmentOrganization",
            "name": parliament.name,
            "url": format!("{}{path}", data.site_url),
            "sameAs": parliament.url,
            "areaServed": state_name(state.as_str()).unwrap_or(state.as_str()),
        }),
        breadcrumb(
            &data.site_url,
            &[
                ("pollywiki", "/"),
                ("States and territories", "/states/"),
                (&parliament.name, &path),
            ],
        ),
    ]));
    page
}

/// Everyone sitting in a parliament, chamber by chamber.
pub fn state_members_page(data: &SiteData, parliament: &StateParliament) -> Page {
    let state = parliament.state;
    let path = state_members_path(state);
    let title = format!("Members of the {}", parliament.name);

    let mut body = String::new();
    body.push_str(&format!(
        "<div class=\"masthead\"><h1 class=\"{}\">{}</h1><p class=\"lede\">{}</p></div>",
        title_tier(&title),
        esc(&title),
        esc(&state_members_lede(parliament))
    ));
    for chamber in &parliament.chambers {
        body.push_str(&format!(
            "<h2 id=\"{}\">{}</h2>",
            chamber.slug,
            esc(&chamber.name)
        ));
        let vacant = chamber.vacancies();
        let mut note = format!(
            "{} of {}.",
            counted(chamber.members.len(), "member"),
            counted(chamber.seats as usize, "seat")
        );
        if vacant > 0 {
            note.push_str(&format!(
                " {} vacant at the last listing.",
                if vacant == 1 {
                    "One seat was".to_string()
                } else {
                    format!("{vacant} seats were")
                }
            ));
        }
        if chamber.seat_label.is_none() {
            note.push_str(" Members are elected by the whole state.");
        }
        body.push_str(&format!("<p class=\"note\">{}</p>", esc(&note)));
        body.push_str(&table_scroll(&format!("{} members", chamber.name)));
        body.push_str("<table><thead data-pagefind-ignore><tr><th scope=\"col\">Member</th>");
        if let Some(label) = &chamber.seat_label {
            body.push_str(&format!("<th scope=\"col\">{}</th>", esc(label)));
        }
        body.push_str(
            "<th scope=\"col\">Party</th><th scope=\"col\">Offices</th></tr></thead><tbody>",
        );
        for member in &chamber.members {
            body.push_str(&format!(
                "<tr><td><a href=\"{}\">{}</a></td>",
                state_member_path(state, &member.slug),
                esc(&member.name)
            ));
            if chamber.seat_label.is_some() {
                body.push_str(&format!(
                    "<td>{}</td>",
                    esc(member.seat.as_deref().unwrap_or(""))
                ));
            }
            body.push_str(&format!(
                "<td>{}</td><td>{}</td></tr>",
                state_party_chip(data, chamber, member),
                member
                    .offices
                    .iter()
                    .map(|o| esc(o))
                    .collect::<Vec<_>>()
                    .join("<br>")
            ));
        }
        body.push_str("</tbody></table></div>");
    }
    body.push_str(&state_lists_attribution(parliament));

    let members: usize = parliament.chambers.iter().map(|c| c.members.len()).sum();
    let description = format!(
        "The {members} sitting members of the {}, chamber by chamber, with their seats, parties and offices.",
        parliament.name
    );
    let mut page = Page::new(title.clone(), Some(description.clone()), path.clone(), body);
    page.jsonld = Some(jsonld_script(vec![
        collection_page(&data.site_url, &title, &description, &path, members),
        breadcrumb(
            &data.site_url,
            &[
                ("pollywiki", "/"),
                ("States and territories", "/states/"),
                (&parliament.name, &state_path(state)),
                ("Members", &path),
            ],
        ),
    ]));
    page
}

pub fn state_member_page(
    data: &SiteData,
    parliament: &StateParliament,
    chamber: &StateChamber,
    member: &StateMember,
) -> Page {
    let state = parliament.state;
    let path = state_member_path(state, &member.slug);
    let seat = state_seat(chamber, member);

    let mut body = String::new();
    body.push_str(&record_article("Person", None));
    body.push_str("<div class=\"profile-head\">");
    body.push_str(&portrait(&member.name, member.photo.as_ref(), true));
    body.push_str("<div><div class=\"meta-row\"><span class=\"ids\">");
    body.push_str(&state_chamber_chip(state, chamber));
    body.push_str(&format!(" <span class=\"seat\">{}</span>", esc(&seat)));
    if let Some(since) = &member.since {
        body.push_str(&format!(
            " <span class=\"term\"><span class=\"sep\" aria-hidden=\"true\">\u{b7} </span>since {}</span>",
            esc(&format_date(since))
        ));
    }
    body.push_str("</span></div>");
    body.push_str(&format!(
        "<h1 data-pagefind-meta=\"title\">{}</h1>",
        esc(&member.name)
    ));
    body.push_str("<div class=\"meta-line\">");
    body.push_str(&state_party_chip(data, chamber, member));
    body.push_str(&format!(
        " <a href=\"{}\">{}</a>",
        state_path(state),
        esc(&parliament.name)
    ));
    if let Some(profile) = &member.profile {
        body.push_str(&format!(
            " <a href=\"{}\">Parliament profile</a>",
            esc_attr(profile)
        ));
    }
    body.push_str("</div></div></div>");

    if !member.offices.is_empty() {
        body.push_str("<h2 id=\"offices\">Offices</h2><ul class=\"offices\">");
        for office in &member.offices {
            body.push_str(&format!("<li>{}</li>", esc(office)));
        }
        body.push_str("</ul>");
    }
    // A territory's parliament is named for its one chamber: "the
    // Legislative Assembly for the Australian Capital Territory", not the
    // Legislative Assembly of it.
    let house = if parliament.name.starts_with(&chamber.name) {
        parliament.name.clone()
    } else {
        format!("{} of the {}", chamber.name, parliament.name)
    };
    body.push_str(&format!(
        "<p class=\"note\">One of {} in the {}. State members' pages carry what their parliament lists: seat, party, offices and service. Voting records, bills and election results are kept for federal parliament only.</p>",
        counted(chamber.members.len(), "member"),
        esc(&house),
    ));
    if let Some(photo) = &member.photo {
        body.push_str(&format!(
            "<p class=\"attribution\">Photo: {} ({}), via Wikimedia Commons.</p>",
            esc(&photo.attribution),
            esc(&photo.licence)
        ));
    }
    body.push_str(&format!(
        "<p class=\"attribution\">Source: {}, <a href=\"{}\">{} member list</a>. Errors? <a href=\"/about/corrections/\">Request a correction.</a></p>",
        esc(&parliament.name),
        esc_attr(&chamber.source),
        esc(&chamber.name)
    ));
    body.push_str("</article>");

    let job = format!(
        "{seat}, {} {}",
        state_name(state.as_str()).unwrap_or(state.as_str()),
        chamber.name
    );
    let description = format!("{job}. Party and offices from the parliament's member list.");
    let mut person_ld = serde_json::json!({
        "@context": "https://schema.org",
        "@type": "Person",
        "name": member.name,
        "url": format!("{}{path}", data.site_url),
        "jobTitle": job,
        "affiliation": {
            "@type": "GovernmentOrganization",
            "name": parliament.name,
            "url": format!("{}{}", data.site_url, state_path(state)),
        },
    });
    if let Some(profile) = &member.profile {
        person_ld["sameAs"] = serde_json::json!(profile);
    }
    if let Some(photo) = &member.photo {
        person_ld["image"] = serde_json::json!(photo.url);
    }
    let mut page = Page::new(member.name.clone(), Some(description), path.clone(), body);
    page.og_type = "profile";
    page.jsonld = Some(jsonld_script(vec![
        person_ld,
        breadcrumb(
            &data.site_url,
            &[
                ("pollywiki", "/"),
                ("States and territories", "/states/"),
                (&parliament.name, &state_path(state)),
                (&member.name, &path),
            ],
        ),
    ]));
    page
}

/// A state bill's progress as dots, filled as far as it has come: four for a
/// parliament of two chambers (introduced, passed the first, passed the
/// second, assent), three for a parliament of one (introduced, passed,
/// assent).
fn state_bill_dots(bill: &StateBill, houses: u8) -> String {
    let (filled, total, reached): (u8, u8, &str) = if houses == 1 {
        match bill.stage {
            0 => (0, 3, "not yet introduced"),
            1 => (1, 3, "introduced"),
            2 | 3 => (2, 3, "passed"),
            _ => (3, 3, "assented"),
        }
    } else {
        match bill.stage {
            0 => (0, 4, "not yet introduced"),
            1 => (1, 4, "introduced"),
            2 => (2, 4, "passed the chamber it began in"),
            3 => (3, 4, "passed both chambers"),
            _ => (4, 4, "assented"),
        }
    };
    let mut out = format!(
        "<span class=\"bill-dots\" role=\"img\" aria-label=\"Stage {filled} of {total}: {reached}\" title=\"Stage {filled} of {total}: {reached}\">"
    );
    for step in 1..=total {
        out.push_str(if step <= filled {
            "<i class=\"on\"></i>"
        } else {
            "<i class=\"off\"></i>"
        });
    }
    out.push_str("</span>");
    out
}

/// The key to a state's progress dots: the federal key for a parliament of
/// two chambers, three stages for a parliament of one.
fn state_dots_legend(houses: u8) -> &'static str {
    if houses == 1 {
        "<p class=\"dots-legend\"><span><span class=\"bill-dots\" aria-hidden=\"true\"><i class=\"on\"></i><i class=\"off\"></i><i class=\"off\"></i></span>Introduced</span> <span><span class=\"bill-dots\" aria-hidden=\"true\"><i class=\"on\"></i><i class=\"on\"></i><i class=\"off\"></i></span>Passed</span> <span><span class=\"bill-dots\" aria-hidden=\"true\"><i class=\"on\"></i><i class=\"on\"></i><i class=\"on\"></i></span>Assent</span></p>"
    } else {
        BILL_DOTS_LEGEND
    }
}

/// The bills index's status filter key: still before the parliament, an Act,
/// or gone no further.
fn state_bill_status_key(bill: &StateBill) -> &'static str {
    if bill.stage >= 4 {
        "act"
    } else if bill.stopped {
        "other"
    } else {
        "open"
    }
}

/// The chamber a bill began in as its members speak of it: "Assembly",
/// "Council".
fn origin_short(bill: &StateBill) -> &str {
    bill.origin
        .as_deref()
        .and_then(|o| o.rsplit(' ').next())
        .unwrap_or("")
}

/// A state bill's latest dated step, else its status, for the activity rows.
fn state_bill_event(bill: &StateBill) -> (String, String) {
    match bill.latest() {
        Some(step) => (
            format!(
                "<span class=\"when\"><time datetime=\"{}\">{}</time></span>",
                esc_attr(&step.date),
                esc(&short_date(&step.date)),
            ),
            format!(
                "<span class=\"event\" title=\"{}\">{}</span>",
                esc_attr(&format!(
                    "{} \u{b7} {}",
                    step.event,
                    format_date(&step.date)
                )),
                esc(&step.event),
            ),
        ),
        None => (
            "<span class=\"when\"></span>".to_string(),
            format!("<span class=\"event\">{}</span>", esc(&bill.status)),
        ),
    }
}

/// Bills index row, in the federal index's columns: latest step, title,
/// chamber of origin, progress and status.
fn state_bill_row(state: StateCode, bill: &StateBill, houses: u8) -> String {
    let when = match bill.latest() {
        Some(step) => format!(
            "<span class=\"when\" title=\"{}\"><time datetime=\"{}\">{}</time></span>",
            esc_attr(&format!(
                "{} \u{b7} {}",
                step.event,
                format_date(&step.date)
            )),
            esc_attr(&step.date),
            esc(&short_date(&step.date)),
        ),
        None => "<span class=\"when\"></span>".to_string(),
    };
    format!(
        "<li data-status=\"{key}\" data-text=\"{text}\">{when}<span class=\"what\"><a href=\"{href}\">{title}</a></span><span class=\"chamber\">{origin}</span><span>{dots}</span><span class=\"{status_class}\">{status}</span></li>",
        key = state_bill_status_key(bill),
        text = esc_attr(&bill.title.to_lowercase()),
        href = state_bill_path(state, &bill.slug),
        title = esc(&bill.title),
        origin = esc(origin_short(bill)),
        dots = state_bill_dots(bill, houses),
        status_class = if state_bill_status_key(bill) == "open" {
            "status open"
        } else {
            "status"
        },
        status = esc(&bill.status),
    )
}

/// The bills that moved most recently in one parliament, newest step first.
fn state_bill_activity(parliament: &StateParliament, limit: usize) -> String {
    let mut out = String::from("<ul class=\"bill-list activity\">");
    for bill in parliament.bills.iter().take(limit) {
        let (when, event) = state_bill_event(bill);
        out.push_str(&format!(
            "<li>{when}<span class=\"what\"><a href=\"{}\">{}</a></span><span>{}</span>{event}</li>",
            state_bill_path(parliament.state, &bill.slug),
            esc(&bill.title),
            state_bill_dots(bill, parliament.houses),
        ));
    }
    out.push_str("</ul>");
    out.push_str(state_dots_legend(parliament.houses));
    out
}

/// Where a parliament's bills were read from, credited.
fn state_bills_attribution(parliament: &StateParliament) -> String {
    match &parliament.bills_source {
        Some(source) => {
            let read = format_date(&source.listed_at.chars().take(10).collect::<String>());
            format!(
                "<p class=\"attribution\">Source: {}, <a href=\"{}\">bills list</a>, read {}. Parliamentary material reproduced fairly and accurately with acknowledgement.{} Errors? <a href=\"/about/corrections/\">Request a correction.</a></p>",
                esc(&source.name),
                esc_attr(&source.url),
                esc(&read),
                state_bills_credit(parliament.state, &esc(&read)),
            )
        }
        None => String::new(),
    }
}

/// Every bill in a parliament's current term, laid out as the federal index
/// is: by the month each last moved, filterable by status and title.
pub fn state_bills_index(data: &SiteData, parliament: &StateParliament) -> Page {
    let state = parliament.state;
    let path = state_bills_path(state);
    let title = format!("Bills in the {}", parliament.name);
    let publisher = parliament
        .bills_source
        .as_ref()
        .map(|s| s.name.as_str())
        .unwrap_or(&parliament.name);

    let mut body = String::new();
    body.push_str(&format!(
        "<div class=\"masthead\"><h1 class=\"{}\">{}</h1><p class=\"lede\">{} of the current parliament, from the record the {} keeps, most recent step first.</p></div>",
        title_tier(&title),
        esc(&title),
        esc(&counted(parliament.bills.len(), "bill")),
        esc(publisher),
    ));
    body.push_str("<div class=\"filter-bar\"><span class=\"pills\" id=\"bill-status\" role=\"group\" aria-label=\"Filter by status\"><button aria-pressed=\"true\" data-status=\"\">All</button><button aria-pressed=\"false\" data-status=\"open\">Before parliament</button><button aria-pressed=\"false\" data-status=\"act\">Act</button><button aria-pressed=\"false\" data-status=\"other\">Other</button></span><input type=\"search\" id=\"bill-filter\" placeholder=\"Filter by title\" enterkeyhint=\"done\" aria-label=\"Filter bills\"></div>");
    body.push_str(&filter_feedback("No bills match these filters."));
    let mut months: Vec<(&str, Vec<&StateBill>)> = Vec::new();
    for bill in &parliament.bills {
        let key = bill
            .latest()
            .map(|s| s.date.get(..7).unwrap_or(s.date.as_str()))
            .unwrap_or("undated");
        if months.last().map(|(m, _)| *m) == Some(key) {
            months.last_mut().expect("just matched").1.push(bill);
        } else {
            months.push((key, vec![bill]));
        }
    }
    let counts: Vec<(&str, usize)> = months.iter().map(|(m, rows)| (*m, rows.len())).collect();
    body.push_str(&month_jump(&counts, "bill"));
    body.push_str(state_dots_legend(parliament.houses));
    body.push_str("<ul class=\"bill-list\" id=\"bill-rows\">");
    body.push_str("<li class=\"bill-head\" aria-hidden=\"true\"><span>Latest</span><span>Bill</span><span>Origin</span><span>Stage</span><span>Status</span></li>");
    for (month, rows) in &months {
        body.push_str(&ledger_month(month, rows.len(), "bill"));
        for bill in rows {
            body.push_str(&state_bill_row(state, bill, parliament.houses));
        }
    }
    body.push_str("</ul>");
    body.push_str(&state_bills_attribution(parliament));

    let description = format!(
        "Bills before the {} in its current term and how far each has come, from the official record.",
        parliament.name
    );
    let mut page = Page::new(title.clone(), Some(description.clone()), path.clone(), body);
    page.page_script = Some(BILL_FILTER_JS.as_str());
    page.jsonld = Some(jsonld_script(vec![
        collection_page(
            &data.site_url,
            &title,
            &description,
            &path,
            parliament.bills.len(),
        ),
        breadcrumb(
            &data.site_url,
            &[
                ("pollywiki", "/"),
                ("States and territories", "/states/"),
                (&parliament.name, &state_path(state)),
                ("Bills", &path),
            ],
        ),
    ]));
    page
}

/// One state bill: where it stands, who brought it in, the steps it has
/// taken, and its documents on the official site.
pub fn state_bill_page(data: &SiteData, parliament: &StateParliament, bill: &StateBill) -> Page {
    let state = parliament.state;
    let path = state_bill_path(state, &bill.slug);

    let mut body = String::new();
    body.push_str(&record_article("Bill", None));
    body.push_str("<div class=\"meta-row\"><span class=\"ids\">");
    if let Some(origin) = &bill.origin {
        body.push_str(&format!(
            "<span class=\"chip {}\">{} {}</span>",
            match bill.origin_kind {
                Some(ChamberKind::Upper) => "senate",
                _ => "house",
            },
            state.as_str(),
            esc(origin.rsplit(' ').next().unwrap_or(origin))
        ));
    }
    if let Some(number) = &bill.number {
        body.push_str(&format!(" {}", esc(number)));
    }
    body.push_str(
        "</span> <span class=\"status\"><span class=\"label\">Status</span> <span class=\"value\">",
    );
    body.push_str(&esc(&bill.status));
    body.push_str("</span>");
    body.push_str(&state_bill_dots(bill, parliament.houses));
    body.push_str("</span></div>");
    body.push_str(&format!(
        "<h1 class=\"{}\" data-pagefind-meta=\"title\">{}</h1>",
        title_tier(&bill.title),
        esc(&bill.title)
    ));
    let mut meta: Vec<String> = Vec::new();
    if let Some(kind) = &bill.kind {
        meta.push(format!("<span>{}</span>", esc(kind)));
    }
    if let Some(sponsor) = &bill.sponsor {
        let name = match &bill.sponsor_slug {
            Some(slug) => format!(
                "<a href=\"{}\">{}</a>",
                state_member_path(state, slug),
                esc(sponsor)
            ),
            None => esc(sponsor),
        };
        meta.push(format!(
            "<span><strong>Introduced by</strong> {name}</span>"
        ));
    }
    meta.push(format!(
        "<span><a href=\"{}\">{}</a></span>",
        state_path(state),
        esc(&parliament.name)
    ));
    body.push_str(&format!(
        "<div class=\"meta-line\">{}</div>",
        meta.join(" ")
    ));

    if let Some(act) = &bill.act {
        body.push_str(&format!(
            "<p class=\"procedure-note\"><strong>Became law</strong> as the {}.</p>",
            esc(act)
        ));
    } else if bill.stopped {
        body.push_str(&format!(
            "<p class=\"procedure-note\"><strong>Went no further:</strong> {}.</p>",
            esc(&bill.status.to_lowercase())
        ));
    }

    body.push_str("<h2>Progress</h2>");
    if bill.timeline.is_empty() {
        body.push_str("<p class=\"note\">The record gives no dated steps for this bill yet.</p>");
    } else {
        body.push_str(&table_scroll("Progress"));
        body.push_str("<table class=\"progress\"><tbody>");
        for step in &bill.timeline {
            body.push_str(&format!(
                "<tr><td class=\"date\">{}</td><td>{}</td></tr>",
                esc(&format_date(&step.date)),
                esc(&step.event)
            ));
        }
        body.push_str("</tbody></table></div>");
    }

    body.push_str("<h2>Documents</h2><ul class=\"doc-links\">");
    if let Some(text) = &bill.text {
        body.push_str(&format!(
            "<li><a href=\"{}\">Text of the bill</a></li>",
            esc_attr(text)
        ));
    }
    if let Some(explanatory) = &bill.explanatory {
        body.push_str(&format!(
            "<li><a href=\"{}\">Explanatory memorandum</a></li>",
            esc_attr(explanatory)
        ));
    }
    body.push_str(&format!(
        "<li><a href=\"{}\">The bill's record on the official site</a></li></ul>",
        esc_attr(&bill.source)
    ));
    body.push_str(&state_bills_attribution(parliament));
    body.push_str("</article>");

    let description = format!(
        "{}. Progress of this bill through the {}.",
        bill.status, parliament.name
    );
    let mut legislation = serde_json::json!({
        "@context": "https://schema.org",
        "@type": "Legislation",
        "name": bill.title,
        "legislationStatus": bill.status,
        "legislationJurisdiction": state_name(state.as_str()).unwrap_or(state.as_str()),
        "url": bill.source,
    });
    if let Some(number) = &bill.number {
        legislation["legislationIdentifier"] = serde_json::json!(number);
    }
    let mut page = Page::new(bill.title.clone(), Some(description), path.clone(), body);
    page.og_type = "article";
    page.lastmod = bill.latest().map(|step| step.date.clone());
    page.jsonld = Some(jsonld_script(vec![
        legislation,
        breadcrumb(
            &data.site_url,
            &[
                ("pollywiki", "/"),
                ("States and territories", "/states/"),
                (&parliament.name, &state_path(state)),
                ("Bills", &state_bills_path(state)),
                (&bill.title, &path),
            ],
        ),
    ]));
    page
}

pub fn search_page() -> Page {
    let mut body = String::new();
    body.push_str("<div class=\"masthead\"><h1 id=\"search-heading\">Search the record</h1><p class=\"lede\">People, divisions, bills, electorates and parties.</p></div>");
    body.push_str("<link href=\"/pagefind/pagefind-ui.css\" rel=\"stylesheet\"><script src=\"/pagefind/pagefind-ui.js\"></script><p id=\"search-status\" class=\"visually-hidden\" role=\"status\"></p><div id=\"search\"></div>");
    let mut page = Page::new(
        "Search",
        Some(
            "Search people, divisions, bills and electorates across Australia's parliaments."
                .to_string(),
        ),
        "/search/",
        body,
    );
    page.page_script = Some(SEARCH_JS.as_str());
    // A search box holds no record of its own, and indexed result pages are
    // exactly what search engines ask you not to publish.
    page.robots = Some("noindex, follow");
    page
}

pub fn not_found() -> Page {
    let body = "<div class=\"masthead\"><h1>Not on the record.</h1><p class=\"motto\">This page doesn't exist. Try <a href=\"/search/\">searching the record</a> or start from the <a href=\"/\">front page</a>.</p></div>".to_string();
    let mut page = Page::new("Page not found", None, "/404/", body);
    page.robots = Some("noindex, follow");
    page
}

pub fn about_index(data: &SiteData) -> Page {
    let body = concat!(
        "<div class=\"masthead\"><h1>About pollywiki</h1>",
        "<p class=\"lede\">pollywiki is a public register of Australia's nine parliaments, federal, state and territory: who sits in each and the bills before them. For federal parliament it also keeps every division vote and the results of the elections that put its members there.</p></div>",
        "<p><strong>This service does not evaluate politicians or laws.</strong> There are no scores, rankings, endorsements or opinions here. Every page is generated automatically from official public records, reproduced faithfully, and every figure links back to its source. Where a number needs interpretation (like attendance), the limits of the official record are stated next to it.</p>",
        "<h2>What powers it</h2>",
        "<p>Data comes from the Parliament of Australia, the eight state and territory parliaments, the Australian Electoral Commission, the Federal Register of Legislation, the Independent Parliamentary Expenses Authority, the Australian Bureau of Statistics, the OpenAustralia Foundation's <a href=\"https://theyvoteforyou.org.au\">They Vote For You</a>, and Wikidata. Full details, licences and sync times are on the <a href=\"/about/data-sources/\">data sources</a> page. The site rebuilds automatically after each sync.</p>",
        "<h2>Who runs it</h2>",
        "<p>pollywiki is an independent, non-commercial project run by <a href=\"https://han.life\">Logan Han</a>. It is not affiliated with the Parliament of Australia, the AEC, any party or any candidate. The <a href=\"https://github.com/logan-han/pollywiki\">source code is open</a>.</p>",
        "<p>Found something wrong? <a href=\"/about/corrections/\">Request a correction.</a></p>",
    )
    .to_string();
    const DESCRIPTION: &str = "What pollywiki is, what it is not, and how it works.";
    let mut page = Page::new("About", Some(DESCRIPTION.to_string()), "/about/", body);
    page.jsonld = Some(jsonld_script(vec![
        about_page(&data.site_url, "About", DESCRIPTION, "/about/"),
        breadcrumb(&data.site_url, &[("pollywiki", "/"), ("About", "/about/")]),
    ]));
    page
}

/// The terms a state or territory parliament publishes its member list
/// under, as its own site states them.
fn state_licence(parliament: &StateParliament) -> &'static str {
    if parliament.chambers.is_empty() {
        return "Not read";
    }
    match parliament.state {
        StateCode::VIC => {
            "All rights reserved; the facts of membership reproduced with acknowledgement"
        }
        StateCode::ACT => "CC BY-NC-ND 4.0, Legislative Assembly for the ACT",
        StateCode::QLD | StateCode::SA | StateCode::TAS => {
            "No licence stated; the facts of membership reproduced with acknowledgement"
        }
        StateCode::NSW | StateCode::WA | StateCode::NT => {
            "Downloaded by hand; the facts of membership reproduced with acknowledgement"
        }
    }
}

/// The terms a parliament's bills are read under, where its bills are read.
fn state_bills_licence(state: StateCode) -> &'static str {
    match state {
        StateCode::VIC => {
            "Victorian Legislation: all rights reserved; the facts of each bill reproduced with acknowledgement"
        }
        StateCode::QLD => "Queensland Legislation: CC BY 4.0",
        StateCode::ACT => "ACT Legislation Register: CC BY 4.0, \u{a9} Australian Capital Territory",
        StateCode::NT => {
            "NT Legislation: published under its terms of use, never as the official version"
        }
        _ => "No licence stated; the facts of each bill reproduced with acknowledgement",
    }
}

/// What a bill's page owes its source beyond the usual acknowledgement, in
/// the words the source's licence asks for. `read` is the day of download.
fn state_bills_credit(state: StateCode, read: &str) -> String {
    match state {
        StateCode::QLD => format!(
            " Sourced from the Queensland Legislation website at {read}. For the latest information on Queensland Government legislation please go to <a href=\"https://www.legislation.qld.gov.au\">https://www.legislation.qld.gov.au</a>."
        ),
        StateCode::ACT => " Register material \u{a9} Australian Capital Territory, CC BY 4.0.".to_string(),
        StateCode::NT => {
            " This is not an official version of the legislation; the official version is the NT Legislation database's.".to_string()
        }
        _ => String::new(),
    }
}

pub fn data_sources(data: &SiteData) -> Page {
    struct Source {
        key: &'static str,
        name: &'static str,
        what: &'static str,
        licence: &'static str,
        link: &'static str,
    }
    const SOURCES: [Source; 9] = [
        Source {
            key: "wikidata",
            name: "Wikidata & Wikimedia Commons",
            what: "Federal members of both chambers, parliamentary groups, seats and photos; state and territory members' photos.",
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
            key: "aec-disclosures",
            name: "AEC Transparency Register",
            what: "Parties' and members' annual returns, the donations party returns itemise, and candidates' election returns.",
            licence: "CC BY 4.0 © Commonwealth of Australia (AEC)",
            link: "https://transparency.aec.gov.au",
        },
        Source {
            key: "ipea",
            name: "Independent Parliamentary Expenses Authority",
            what: "Each parliamentarian's expenses for the latest four quarters, by IPEA category.",
            licence: "CC BY 3.0 AU, via data.gov.au",
            link: "https://www.ipea.gov.au",
        },
        Source {
            key: "boundaries",
            name: "Australian Bureau of Statistics",
            what: "Electorate maps, from the ABS's yearly approximation of each division's boundary.",
            licence: "CC BY 4.0 © Commonwealth of Australia (ABS)",
            link: "https://www.abs.gov.au/statistics/standards/australian-statistical-geography-standard-asgs-edition-3/jul2021-jun2026/non-abs-structures/commonwealth-electoral-divisions",
        },
        Source {
            key: "legislation",
            name: "Federal Register of Legislation",
            what: "The Act each passed bill became: its number, assent date and status.",
            licence: "CC BY 4.0, sourced from the Federal Register of Legislation",
            link: "https://www.legislation.gov.au",
        },
        Source {
            key: "aec",
            name: "Australian Electoral Commission",
            what: "Federal election results: House by electorate and Senate by state, from 2004 including by-elections.",
            licence: "CC BY 4.0 © Commonwealth of Australia",
            link: "https://results.aec.gov.au",
        },
        Source {
            key: "handbook",
            name: "Parliamentary Handbook",
            what: "Careers before parliament, qualifications, ministries, shadow ministries, positions held, committee service and when each division was established.",
            licence: "Commonwealth of Australia; reproduced fairly and accurately with acknowledgement",
            link: "https://handbook.aph.gov.au",
        },
    ];

    let mut body = String::new();
    body.push_str("<div class=\"masthead\"><h1>Data sources</h1><p class=\"lede\">Everything on this site is generated from the sources below. Nothing is written by hand and nothing is edited after ingestion.</p></div>");
    body.push_str(&table_scroll("Data sources"));
    body.push_str("<table><thead><tr><th scope=\"col\">Source</th><th scope=\"col\">Used for</th><th scope=\"col\">Licence</th><th scope=\"col\">Last synced</th></tr></thead><tbody>");
    let last_synced = |key: &str| match data.meta.sources.get(key) {
        Some(status) => format!(
            "{}{}",
            format_date(&status.last_sync.chars().take(10).collect::<String>()),
            if status.ok { "" } else { " (failed)" }
        ),
        None => "not yet".to_string(),
    };
    for s in &SOURCES {
        body.push_str(&format!(
            "<tr><td><a href=\"{}\">{}</a></td><td>{}</td><td>{}</td><td class=\"num\">{}</td></tr>",
            s.link,
            esc(s.name),
            s.what,
            s.licence,
            last_synced(s.key),
        ));
    }
    body.push_str("</tbody></table></div>");

    if !data.states.is_empty() {
        body.push_str("<h2 id=\"states\">State and territory parliaments</h2><p>Each parliament's members, parties, seats and offices are read from the member list that parliament publishes, and its bills from the official record of them that it or its legislation register keeps. Only those facts are reproduced: no photos, biographies or other material from the parliaments' sites. Members' photos, where shown, come from Wikimedia Commons under free licences, found through Wikidata as federal members' are.</p>");
        body.push_str(&table_scroll("State and territory sources"));
        body.push_str("<table><thead><tr><th scope=\"col\">Parliament</th><th scope=\"col\">Read from</th><th scope=\"col\">Licence</th><th scope=\"col\">Last synced</th></tr></thead><tbody>");
        for parliament in &data.states {
            let mut read_from = if parliament.chambers.is_empty() {
                esc(parliament.unavailable.as_deref().unwrap_or("Not read yet."))
            } else {
                let lists = parliament
                    .chambers
                    .iter()
                    .map(|c| format!("<a href=\"{}\">{}</a>", esc_attr(&c.source), esc(&c.name)))
                    .collect::<Vec<_>>()
                    .join(", ");
                match parliament.hand_listed {
                    Some(_) => format!("{lists} (downloaded by hand)"),
                    None => lists,
                }
            };
            if let Some(source) = &parliament.bills_source {
                read_from.push_str(&format!(
                    "<br>Bills: <a href=\"{}\">{}</a>",
                    esc_attr(&source.url),
                    esc(&source.name)
                ));
            }
            let mut licence = state_licence(parliament).to_string();
            let mut synced = last_synced(&format!(
                "{}-parliament",
                parliament.state.as_str().to_lowercase()
            ));
            if parliament.bills_source.is_some() {
                licence.push_str(&format!(
                    "<br>Bills: {}",
                    state_bills_licence(parliament.state)
                ));
                synced.push_str(&format!(
                    "<br>{}",
                    last_synced(&format!(
                        "{}-bills",
                        parliament.state.as_str().to_lowercase()
                    ))
                ));
            }
            body.push_str(&format!(
                "<tr><td><a href=\"{}\">{}</a></td><td>{read_from}</td><td>{licence}</td><td class=\"num\">{synced}</td></tr>",
                esc_attr(&parliament.url),
                esc(&parliament.name),
            ));
        }
        body.push_str("</tbody></table></div>");
    }
    body.push_str("<h2>Attribution and reuse</h2><p>Voting data is used under the ODbL: it is attributed on every page where it appears and derived data remains open. Election figures are © Commonwealth of Australia (AEC), CC BY 4.0, and this site does not use AEC branding. Parliamentary material is reproduced fairly and accurately with acknowledgement and without any implication of parliamentary endorsement. Photos come only from Wikimedia Commons under free licences, with the photographer credited on the page where the photo appears.</p>");
    body.push_str("<p>The site's own code is <a href=\"https://github.com/logan-han/pollywiki\">open source on GitHub</a>.</p>");
    const DESCRIPTION: &str =
        "Where every figure on pollywiki comes from, its licence and when it last synced.";
    let mut page = Page::new(
        "Data sources",
        Some(DESCRIPTION.to_string()),
        "/about/data-sources/",
        body,
    );
    page.jsonld = Some(jsonld_script(vec![
        about_page(
            &data.site_url,
            "Data sources",
            DESCRIPTION,
            "/about/data-sources/",
        ),
        breadcrumb(
            &data.site_url,
            &[
                ("pollywiki", "/"),
                ("About", "/about/"),
                ("Data sources", "/about/data-sources/"),
            ],
        ),
    ]));
    page
}

pub fn methodology(data: &SiteData) -> Page {
    let body = concat!(
        "<div class=\"masthead\"><h1>Methodology</h1>",
        "<p class=\"lede\">pollywiki publishes official records verbatim plus simple arithmetic. This page defines every derived figure that appears on the site.</p></div>",
        "<h2>Divisions voted</h2>",
        "<p>\"Voted in <em>n</em> of <em>m</em> divisions\" counts the divisions in the member's own chamber held while they sat (<em>m</em>) and the divisions where their name appears in the ayes or noes (<em>n</em>). A member sworn in at a by-election, or one who has since left, is measured against their own time in the seat, not the whole parliament.</p>",
        "<p><strong>Absence is not abstention.</strong> The official record does not distinguish pairing arrangements, approved leave, ministerial or committee duties, or illness from any other reason for not voting. A low attendance figure is not, by itself, evidence of anything. This caveat is shown wherever the figure appears.</p>",
        "<h2>Votes against party majority (\"crossed\")</h2>",
        "<p>For each division, each parliamentary group's majority position is the side (aye or no) with more of that group's votes. A member's vote is marked \"crossed\" when it lands on the other side. Groups that split evenly have no majority position, so no vote in that division is marked. Independents have no group majority and are never marked.</p>",
        "<h2>Chamber composition</h2>",
        "<p>Seat counts group members by parliamentary group as recorded on Wikidata, which follows the official record: Coalition members are shown as one group. Counts reflect current sitting members, not the election result.</p>",
        "<h2>Election percentages</h2>",
        "<p>First preference and two-candidate-preferred percentages are computed from AEC final totals per electorate. Candidate shares are of formal votes, the base the AEC's own percentages and swings use. Informal ballots are shown after the candidates as a share of all ballots cast, beside the AEC's swing in informality. Swing figures are the AEC's own published swings.</p>",
        "<h2>Senate results</h2>",
        "<p>A senator's election history gives their group's first preferences as a share of the state's formal vote: above-the-line votes for the group plus every group candidate's own first preferences, over all formal votes in the state. Most Senate votes are cast for a group, so a candidate's personal count would understate the support they were elected on. Places elected are the AEC's order of election.</p>",
        "<h2>Results at each election</h2>",
        "<p>An electorate's list of past results names the winner of every contest for a division of that name since 2004, with their share of the two-candidate-preferred count where the AEC published one. Each contest was held on the boundaries then in force, so the list follows a name, not an unchanged area. Election history on a profile is matched by name within the member's state; a candidate on the roll under a longer first name (\"Raffaele\" for \"Raff\") counts only where exactly one member fits.</p>",
        "<h2>Expenses</h2>",
        "<p>Parliamentary expenses are IPEA's published transactions, summed per parliamentarian and per IPEA category for each of the latest four quarters and rounded to the dollar. Travel distances, office locations and roles differ, so totals are not compared or ranked.</p>",
        "<h2>Funding disclosures</h2>",
        "<p>Party returns are listed as lodged with the AEC, one row per return, never added together: a party's branches lodge separately and money passed between them counts in each. Donations are the gifts a party's returns list as donations received, summed by donor across the party's returns for the latest financial year. A member's returns are matched by name with titles removed; candidate returns as election history is.</p>",
        "<h2 id=\"states\">State and territory parliaments</h2>",
        "<p>Each state and territory parliament's members are read from the member list that parliament publishes, and nothing else: who sits in which chamber, for which party, for which seat, and the offices the list gives them. A member who leaves drops off at the next reading, and a seat the list leaves empty is shown as vacant.</p>",
        "<p>Seats are counted by the party each parliament lists a member under, with a state branch counted as its party. A chamber's majority is more than half of all its seats, vacancies included, so a party can hold most of the members sitting today and still lack a majority. The Premier or Chief Minister is the member the parliament's list gives that office.</p>",
        "<p>Where a parliament's website turns away automated readers, its member list is downloaded by hand from that site about once a month, and its page says so and dates the list by the day it was downloaded. Until a download is filed, its place on the site says why the list cannot be read and links to the parliament's own list.</p>",
        "<p>Each state and territory parliament's bills are read from the official list of them that the parliament or its legislation register publishes: title, the chamber it began in, who introduced it, where it stands and the dated steps it took. How far a bill has come is shown as for federal bills, in four stages for a parliament of two chambers and three for a parliament of one, worked out from those steps and the list's own status. State bills carry no machine-written summaries.</p>",
        "<p>For the states and territories there are no voting records or election results yet: only federal parliament has a source that records every vote in a form this site can read.</p>",
        "<h2>Maps</h2>",
        "<p>Electorate maps draw the ABS's approximation of each division, built from mesh blocks, simplified to what the map can show. The AEC's own maps show the legal boundary.</p>",
        "<h2>AI summaries and notes</h2>",
        "<p>Machine-written text appears in four places, always labelled \"AI-generated\".</p>",
        "<p><strong>Bill context.</strong> Each bill's page explains the official summary in plain terms. General knowledge of Australian parliamentary practice is used to explain the mechanism (what an appropriation bill is, for example); anything specific to the bill comes from its official summary only. A bill with no official summary yet gets no note; one is written once APH publishes a summary.</p>",
        "<p><strong>Division context.</strong> They Vote For You provides written context for most bill votes; for procedural motions their context field is a raw Hansard excerpt. For those, a one-to-two sentence note explains what the bill, amendment or motion was about and what question was being decided, grounded on that excerpt and the official bill summaries. It never restates the result, which the page already shows, and links to the full record.</p>",
        "<p><strong>Series of divisions.</strong> When a chamber divides on one matter several times in a sitting day, the home page and the divisions index fold those divisions into one entry; the index's counts are still of divisions, not entries. On the home page each vote in a series is labelled with the first sentence of its context: They Vote For You's where volunteers have written it, otherwise the machine-written division note, marked AI. The sequence of amendments put and lost before the question itself then reads as one story. The divisions index names each vote by its official stage, or by its division number, and describes none.</p>",
        "<p><strong>Voting record in brief.</strong> Each member's page carries a short note describing patterns their voting table cannot show at a glance. The arithmetic is done first: every division the member voted in is read for the bill or kind of motion and the stage, and their ayes and noes are counted per bill and per kind. The generator is handed those counts and the divisions where they voted against their own party grouping, and nothing else, so it can phrase the sums but not make them; a note that uses a number the counts do not contain is discarded. It is instructed to describe, never evaluate: no praise, no criticism, no motives, no ideology. A note is rewritten once the member has voted in ten more divisions since it was written, or at once when they vote against their party grouping.</p>",
        "<p>AI text is never part of the record. If a summary or note misstates the record, <a href=\"/about/corrections/\">request a correction</a> and it will be regenerated or removed.</p>",
        "<h2>What this site never does</h2>",
        "<p>No scoring, no ranking, no summarising of speeches, no inference of positions from votes. Where They Vote For You provides a plain-English description of a motion, it is shown with explicit attribution to them.</p>",
    )
    .to_string();
    const DESCRIPTION: &str = "How every derived figure on pollywiki is computed, and what the official record cannot tell you.";
    let mut page = Page::new(
        "Methodology",
        Some(DESCRIPTION.to_string()),
        "/about/methodology/",
        body,
    );
    page.jsonld = Some(jsonld_script(vec![
        about_page(
            &data.site_url,
            "Methodology",
            DESCRIPTION,
            "/about/methodology/",
        ),
        breadcrumb(
            &data.site_url,
            &[
                ("pollywiki", "/"),
                ("About", "/about/"),
                ("Methodology", "/about/methodology/"),
            ],
        ),
    ]));
    page
}

pub fn corrections(data: &SiteData) -> Page {
    let body = concat!(
        "<div class=\"masthead\"><h1>Corrections</h1>",
        "<p class=\"lede\">Every page here is generated from official sources, but pipelines have bugs and sources have errors. If anything on this site is wrong, incomplete or misleading, report it and it will be fixed or taken down quickly.</p></div>",
        "<h2>How to report</h2>",
        "<p><a href=\"https://github.com/logan-han/pollywiki/issues\">Open an issue on GitHub</a> with a link to the affected page and, if you can, a link to the official record that shows the correct information.</p>",
        "<h2>What happens</h2>",
        "<p>Reports about a person are treated with priority. If a claim about a person cannot be verified against the official source promptly, the content is removed while it is checked. Corrections are applied at the data pipeline level so they cannot regress on the next sync.</p>",
        "<h2>Upstream errors</h2>",
        "<p>Where the error is in the source itself (Hansard, the AEC, Wikidata or They Vote For You), it needs fixing there; this site will pick up the fix on the next sync. Reports are forwarded upstream where possible.</p>",
    )
    .to_string();
    const DESCRIPTION: &str = "How to report an error on pollywiki and what happens next.";
    let mut page = Page::new(
        "Corrections",
        Some(DESCRIPTION.to_string()),
        "/about/corrections/",
        body,
    );
    page.jsonld = Some(jsonld_script(vec![
        about_page(
            &data.site_url,
            "Corrections",
            DESCRIPTION,
            "/about/corrections/",
        ),
        breadcrumb(
            &data.site_url,
            &[
                ("pollywiki", "/"),
                ("About", "/about/"),
                ("Corrections", "/about/corrections/"),
            ],
        ),
    ]));
    page
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

    /// The sample bundles carry no party facts, so the block that renders them
    /// is driven from a fixture: a website label with the scheme, the www and
    /// the trailing slash stripped, and a Wikipedia title decoded out of its
    /// URL.
    #[test]
    fn party_facts_render_a_readable_website_and_article_title() {
        let data = crate::data::SiteData::load(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/sample/bundles"),
            "https://pollywiki.test",
        )
        .expect("sample bundles load");

        let party: Party = serde_json::from_value(serde_json::json!({
            "slug": "example-party",
            "name": "Example Party",
            "facts": {
                "founded": "1901-05-08",
                "website": "https://www.example.org.au/",
                "wikipedia": "https://en.wikipedia.org/wiki/Example_Party_(Australia)",
            },
        }))
        .expect("party fixture");
        let html = party_page(&data, &party).body;
        assert!(
            html.contains("<a href=\"https://www.example.org.au/\">example.org.au</a>"),
            "the label drops the scheme, the www and the trailing slash: {html}"
        );
        assert!(
            html.contains(
                "<a href=\"https://en.wikipedia.org/wiki/Example_Party_(Australia)\">Example Party (Australia)</a>"
            ),
            "the article title is decoded and unescaped: {html}"
        );

        // A party with no facts at all renders no table.
        let bare: Party = serde_json::from_value(serde_json::json!({
            "slug": "example-party",
            "name": "Example Party",
        }))
        .expect("party fixture");
        assert!(!party_page(&data, &bare)
            .body
            .contains("table class=\"facts\""));
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
