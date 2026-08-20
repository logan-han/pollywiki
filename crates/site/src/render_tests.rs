//! Whole-template tests over the committed sample bundles.
//!
//! Every page type is rendered through the real `layout::render`, then checked
//! for the invariants that matter: absolute canonical and share URLs, valid
//! JSON-LD, the accessibility scaffolding, and the layout constraints that real
//! APH data has broken before (long questions, long step descriptions). The
//! sample bundles deliberately carry worst-case titles, events and statuses, so
//! a regression shows up here rather than in a deploy.

use crate::components::{bill_dots, bill_stage, ledger_month, month_label, seat_bar};
use crate::data::{division_key, SiteData};
use crate::feeds;
use crate::layout::{self, Page};
use crate::og;
use crate::pages;
use crate::procedures::procedure_for;
use pollywiki_schema::{DivisionResult, House};
use std::path::PathBuf;

const SITE_URL: &str = "https://pollywiki.test";
const CSS_HREF: &str = "/_assets/site.deadbeef.css";

fn bundles() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/sample/bundles")
        .canonicalize()
        .expect("sample bundles are committed next to the crate")
}

fn sample_data() -> SiteData {
    SiteData::load(&bundles(), SITE_URL).expect("sample bundles load")
}

/// Every page the build emits, in build order.
fn all_pages(data: &SiteData) -> Vec<Page> {
    let mut list = vec![
        pages::home(data),
        pages::people_index(data),
        pages::divisions_index(data),
        pages::bills_index(data),
        pages::electorates_index(data),
        pages::parties_index(data),
        pages::search_page(),
        pages::about_index(),
        pages::data_sources(data),
        pages::methodology(),
        pages::corrections(),
        pages::not_found(),
    ];
    for person in &data.people {
        list.push(pages::person_page(data, person));
    }
    for division in &data.divisions {
        list.push(pages::division_page(data, division));
    }
    for bill in &data.bills {
        list.push(pages::bill_page(data, bill));
    }
    for electorate in &data.electorates {
        list.push(pages::electorate_page(data, electorate));
    }
    for party in &data.parties {
        list.push(pages::party_page(data, party));
    }
    list
}

fn render(data: &SiteData, page: &Page) -> String {
    layout::render(data, SITE_URL, CSS_HREF, page)
}

/// The single JSON-LD block on a page, parsed.
fn jsonld(html: &str) -> Option<serde_json::Value> {
    let open = "<script type=\"application/ld+json\">";
    let start = html.find(open)? + open.len();
    let end = start + html[start..].find("</script>")?;
    Some(serde_json::from_str(&html[start..end]).expect("json-ld parses"))
}

/// Flattens a JSON-LD payload to the list of nodes it declares.
fn nodes(value: &serde_json::Value) -> Vec<&serde_json::Value> {
    match value.as_array() {
        Some(items) => items.iter().collect(),
        None => vec![value],
    }
}

fn types(value: &serde_json::Value) -> Vec<String> {
    nodes(value)
        .iter()
        .filter_map(|n| n["@type"].as_str().map(str::to_string))
        .collect()
}

#[test]
fn sample_bundles_carry_worst_case_shapes() {
    let data = sample_data();
    assert!(!data.people.is_empty());
    assert!(!data.divisions.is_empty());
    assert!(!data.bills.is_empty());
    assert_eq!(data.site_url, SITE_URL);

    // These are what broke the layout when the samples were all short: a
    // question past 100 characters and a step description past 50.
    assert!(
        data.divisions.iter().any(|d| d.name.chars().count() > 100),
        "sample divisions need a long question to exercise the tables"
    );
    assert!(
        data.bills
            .iter()
            .flat_map(|b| &b.timeline)
            .any(|s| s.event.chars().count() > 50),
        "sample bills need a long step description with a chamber suffix"
    );
    // Every pill bucket has at least one bill behind it.
    let statuses: Vec<&str> = data.bills.iter().map(|b| b.status.as_str()).collect();
    assert!(statuses.iter().any(|s| s.starts_with("Before")));
    assert!(statuses.iter().any(|s| *s == "Act" || *s == "Assent"));
    assert!(statuses
        .iter()
        .any(|s| !s.starts_with("Before") && *s != "Act" && *s != "Assent"));
}

#[test]
fn every_page_carries_the_shared_head_and_landmarks() {
    let data = sample_data();
    for page in all_pages(&data) {
        let html = render(&data, &page);
        let where_ = &page.path;

        assert!(
            html.starts_with("<!DOCTYPE html><html lang=\"en-AU\">"),
            "{where_}"
        );
        assert!(html.ends_with("</body></html>"), "{where_}");
        assert!(html.contains("<title>"), "{where_}");

        // Canonical, og:url and og:image are absolute and agree with the path.
        let canonical = format!("<link rel=\"canonical\" href=\"{SITE_URL}{}\">", page.path);
        assert!(html.contains(&canonical), "canonical wrong on {where_}");
        assert!(
            html.contains(&format!(
                "<meta property=\"og:url\" content=\"{SITE_URL}{}\">",
                page.path
            )),
            "og:url wrong on {where_}"
        );
        let expected_image = page.og_image.as_deref().unwrap_or(layout::DEFAULT_OG_IMAGE);
        assert!(
            html.contains(&format!(
                "<meta property=\"og:image\" content=\"{SITE_URL}{expected_image}\">"
            )),
            "og:image wrong on {where_}"
        );
        assert!(
            html.contains("twitter:card\" content=\"summary_large_image"),
            "{where_}"
        );

        // Accessibility scaffolding from finding 09, and the combobox from 04.
        assert!(
            html.contains("<body><a class=\"skip\" href=\"#main\">Skip to content</a>"),
            "skip link must be first in the tab order on {where_}"
        );
        assert!(
            html.contains("<main class=\"wrap\" id=\"main\">"),
            "{where_}"
        );
        assert!(html.contains("role=\"combobox\""), "{where_}");
        assert!(
            html.contains("<ul id=\"quick-search-results\" role=\"listbox\""),
            "{where_}"
        );
        assert!(!html.contains("<th>"), "every th needs a scope on {where_}");

        // Both theme colours, the feed links and the font preloads.
        assert!(
            html.contains("name=\"theme-color\" content=\"#fafaf7\""),
            "{where_}"
        );
        assert!(
            html.contains("content=\"#191d1b\" media=\"(prefers-color-scheme: dark)\""),
            "{where_}"
        );
        assert!(html.contains("href=\"/divisions/feed.xml\""), "{where_}");
        assert!(html.contains("rel=\"preload\""), "{where_}");
        assert!(
            html.contains("property=\"og:site_name\" content=\"pollywiki\""),
            "{where_}"
        );
        assert!(
            html.contains("property=\"og:locale\" content=\"en_AU\""),
            "{where_}"
        );

        if let Some(value) = jsonld(&html) {
            assert!(
                !types(&value).is_empty(),
                "json-ld with no @type on {where_}"
            );
        }
    }
}

#[test]
fn the_sample_banner_follows_the_meta_flag() {
    let mut data = sample_data();
    let page = pages::about_index();

    data.meta.sample = true;
    assert!(render(&data, &page).contains("class=\"sample-banner\""));

    data.meta.sample = false;
    assert!(!render(&data, &page).contains("class=\"sample-banner\""));
}

#[test]
fn home_lists_bill_activity_newest_first() {
    let data = sample_data();
    let page = pages::home(&data);
    let html = render(&data, &page);

    assert!(html.contains("Latest bill activity"));
    assert!(html.contains("class=\"bill-list activity\""));
    assert!(html.contains("class=\"dots-legend\""));

    // Rows are ordered by their newest step, descending.
    let dates: Vec<&str> = html
        .match_indices("<time datetime=\"")
        .map(|(i, m)| {
            let start = i + m.len();
            &html[start..start + 10]
        })
        .collect();
    assert!(dates.len() >= 2, "expected several activity rows");
    let mut sorted = dates.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(dates, sorted, "activity rows are not newest first");

    // The chamber suffix is dropped from the visible text but kept in the title.
    assert!(html.contains(
        "Referred to Federation Chamber \u{b7} <time datetime=\"2025-08-04\">4 Aug</time>"
    ));
    assert!(html.contains("title=\"Referred to Federation Chamber (House of Representatives)"));

    // WebSite + SearchAction, per finding 11.
    let value = jsonld(&html).expect("home carries json-ld");
    assert_eq!(types(&value), vec!["WebSite"]);
    assert_eq!(
        value["potentialAction"]["target"]["urlTemplate"],
        format!("{SITE_URL}/search/?q={{search_term_string}}")
    );
}

#[test]
fn divisions_index_groups_into_months_that_match_their_runs() {
    let data = sample_data();
    let html = render(&data, &pages::divisions_index(&data));

    assert!(html.contains("id=\"division-filter-text\""));
    assert!(html.contains("id=\"filter-count\""));
    assert!(html.contains("id=\"filter-clear\""));

    // Every month divider reports the number of rows that follow it.
    let mut months: Vec<(String, usize)> = Vec::new();
    for chunk in html.split("<li class=\"ledger-month\"").skip(1) {
        let key_start = chunk.find("data-month=\"").expect("month key") + 12;
        let key = chunk[key_start..].split('"').next().expect("month key end");
        let claimed: usize = chunk
            .split("class=\"n\">")
            .nth(1)
            .and_then(|s| s.split(' ').next())
            .and_then(|n| n.parse().ok())
            .expect("month count");
        let rows = chunk
            .split("<li class=\"ledger-month\"")
            .next()
            .unwrap_or(chunk)
            .matches("<li data-house=")
            .count();
        assert_eq!(claimed, rows, "month {key} claims {claimed} but has {rows}");
        months.push((key.to_string(), rows));
    }
    assert!(months.len() >= 2, "sample data should span several months");
    let keys: Vec<&String> = months.iter().map(|(k, _)| k).collect();
    let mut sorted = keys.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(keys, sorted, "months are not newest first");
    assert_eq!(
        months.iter().map(|(_, n)| n).sum::<usize>(),
        data.divisions.len()
    );
}

#[test]
fn bills_index_reads_newest_activity_first_under_month_dividers() {
    let data = sample_data();
    let html = render(&data, &pages::bills_index(&data));

    // Every month divider reports the number of bill rows that follow it.
    let mut months: Vec<(String, usize)> = Vec::new();
    for chunk in html.split("<li class=\"ledger-month\"").skip(1) {
        let key_start = chunk.find("data-month=\"").expect("month key") + 12;
        let key = chunk[key_start..].split('"').next().expect("month key end");
        let claimed: usize = chunk
            .split("class=\"n\">")
            .nth(1)
            .and_then(|s| s.split(' ').next())
            .and_then(|n| n.parse().ok())
            .expect("month count");
        let rows = chunk
            .split("<li class=\"ledger-month\"")
            .next()
            .unwrap_or(chunk)
            .matches("<li data-status=")
            .count();
        assert_eq!(claimed, rows, "month {key} claims {claimed} but has {rows}");
        months.push((key.to_string(), rows));
    }
    assert!(months.len() >= 2, "sample data should span several months");
    let keys: Vec<&String> = months.iter().map(|(k, _)| k).collect();
    let mut sorted = keys.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(keys, sorted, "months are not newest first");
    assert_eq!(
        months.iter().map(|(_, n)| n).sum::<usize>(),
        data.bills.len()
    );

    // Rows carry the last recorded step, newest first down the whole list.
    let dates: Vec<&str> = html
        .match_indices("<span class=\"when\" title=")
        .filter_map(|(i, _)| html[i..].split_once("<time datetime=\""))
        .map(|(_, rest)| rest.split('"').next().expect("date end"))
        .collect();
    assert_eq!(dates.len(), data.bills.len(), "every row dated");
    let mut newest_first = dates.clone();
    newest_first.sort_by(|a, b| b.cmp(a));
    assert_eq!(dates, newest_first, "bills are not newest first");
}

#[test]
fn bills_index_pills_cover_every_row() {
    let data = sample_data();
    let html = render(&data, &pages::bills_index(&data));

    for bucket in ["", "open", "act", "other"] {
        assert!(
            html.contains(&format!("data-status=\"{bucket}\"")),
            "{bucket} pill"
        );
    }
    // Each row declares a bucket the pills can actually select.
    let row_buckets: Vec<&str> = html
        .match_indices("<li data-status=\"")
        .map(|(i, m)| html[i + m.len()..].split('"').next().expect("bucket"))
        .collect();
    assert_eq!(row_buckets.len(), data.bills.len());
    for bucket in &row_buckets {
        assert!(
            matches!(*bucket, "open" | "act" | "other"),
            "bad bucket {bucket}"
        );
    }
    assert!(html.contains("class=\"dots-legend\""));
}

#[test]
fn division_pages_state_the_outcome_and_link_their_card() {
    let data = sample_data();
    for division in &data.divisions {
        let page = pages::division_page(&data, division);
        let html = render(&data, &page);
        let expected = match division.result {
            DivisionResult::Passed => "Carried",
            DivisionResult::Rejected => "Negatived",
        };
        assert!(
            html.contains(&format!("<strong>{expected}</strong>")),
            "outcome missing on {}",
            page.path
        );
        assert!(html.contains("og:type\" content=\"article"));
        assert_eq!(page.lastmod.as_deref(), Some(division.date.as_str()));
        assert_eq!(
            types(&jsonld(&html).expect("breadcrumbs")),
            vec!["BreadcrumbList"]
        );
        assert_eq!(
            og::card_path(division),
            format!(
                "/og/divisions/{}/{}.png",
                division.house,
                division_key(division)
            )
        );
    }
}

#[test]
fn bill_pages_show_progress_and_legislation_data() {
    let data = sample_data();
    for bill in &data.bills {
        let page = pages::bill_page(&data, bill);
        let html = render(&data, &page);
        assert!(
            html.contains("class=\"bill-dots\""),
            "dots missing on {}",
            page.path
        );
        assert!(html.contains("aria-label=\"Stage "), "{}", page.path);
        assert!(html.contains("og:type\" content=\"article"));
        let value = jsonld(&html).expect("bill json-ld");
        assert_eq!(types(&value), vec!["Legislation", "BreadcrumbList"]);
        let legislation = &nodes(&value)[0];
        assert_eq!(legislation["name"], bill.title.as_str());
        assert_eq!(legislation["legislationStatus"], bill.status.as_str());
        // lastmod tracks the newest recorded step, or is absent without one.
        assert_eq!(
            page.lastmod.is_some(),
            !bill.timeline.is_empty(),
            "lastmod wrong on {}",
            page.path
        );
    }
}

#[test]
fn person_pages_are_profiles_with_a_result_column() {
    let data = sample_data();
    for person in &data.people {
        let page = pages::person_page(&data, person);
        let html = render(&data, &page);
        assert!(html.contains("og:type\" content=\"profile"));
        let value = jsonld(&html).expect("person json-ld");
        assert_eq!(types(&value), vec!["Person", "BreadcrumbList"]);
        assert_eq!(nodes(&value)[0]["name"], person.name.as_str());

        if !data.votes_for_person(&person.slug).is_empty() {
            assert!(
                html.contains("<th scope=\"col\">Result</th>"),
                "{}",
                page.path
            );
            assert!(html.contains("class=\"result-chip"), "{}", page.path);
        }
    }
}

#[test]
fn machine_written_context_is_always_labelled() {
    let data = sample_data();
    let mut labelled = 0;
    for page in all_pages(&data) {
        let html = render(&data, &page);
        for (i, _) in html.match_indices("context-body") {
            // Every AI body sits in a box whose header carries the tag.
            let box_start = html[..i]
                .rfind("<aside class=\"context-box\"")
                .expect("aside");
            let card = &html[box_start..i];
            if card.contains("ai-tag") {
                labelled += 1;
                assert!(
                    html[i..].contains("Written by AI") || html[i..].contains("Written by AI to"),
                    "an AI card on {} carries no provenance credit",
                    page.path
                );
            }
        }
    }
    assert!(labelled >= 2, "sample data should carry AI notes to check");
}

#[test]
fn a_transcript_summary_is_never_shown_as_written_context() {
    let data = sample_data();
    let transcript = data
        .divisions
        .iter()
        .find(|d| d.summary_kind == Some(pollywiki_schema::SummaryKind::Transcript))
        .expect("sample data has a transcript division");
    let html = render(&data, &pages::division_page(&data, transcript));

    // The Hansard excerpt itself must not be reproduced as TVFY context.
    assert!(!html.contains("The question is that the bill be read a first time"));
    assert!(!html.contains("Context written by"));
    // The machine-written replacement takes its place, labelled.
    assert!(html.contains("ai-tag"));
    assert!(html.contains("Machine-written context explaining"));
    // Both official links are offered in the footer note.
    assert!(html.contains("theyvoteforyou.org.au/divisions/senate/2025-08-05/3"));
    assert!(html.contains("Hansard"));
}

#[test]
fn bill_summaries_render_in_all_three_grammars() {
    let data = sample_data();
    let html_for = |id: &str| {
        let bill = data.bills.iter().find(|b| b.id == id).expect("sample bill");
        render(&data, &pages::bill_page(&data, bill))
    };

    // Multi-act: items nest under an act heading.
    let grouped = html_for("sample-1");
    assert!(grouped.contains("<p>Amends:</p>"));
    assert!(grouped.contains("Corporations Act 2001"));
    assert!(grouped.contains("<li>require one thing</li>"));
    assert!(grouped.contains("Privacy Act 1988"));

    // Single act, several items: a lead line then a flat list.
    let listed = html_for("sample-2");
    assert!(listed.contains("<li>do the first thing</li>"));
    // The last item keeps the summary's closing full stop.
    assert!(listed.contains("<li>do the fourth thing.</li>"));

    // Short summary: plain prose, no list.
    let prose = html_for("sample-3");
    assert!(prose.contains("Makes minor technical amendments to review legislation."));
    assert!(!prose.contains("<li>Makes minor"));

    // Every one credits the official summary rather than implying authorship.
    for html in [&grouped, &listed, &prose] {
        assert!(html.contains("Summary from the official bill homepage."));
    }
}

#[test]
fn profiles_render_background_photos_and_election_history() {
    let data = sample_data();
    let person = data
        .people
        .iter()
        .find(|p| p.photo.is_some())
        .expect("sample data has a portrait");
    let html = render(&data, &pages::person_page(&data, person));

    // Portrait, with its licence and attribution.
    assert!(html.contains("alt=\"Portrait of Alex Paterson\""));
    assert!(html.contains("CC BY-SA 4.0"));
    assert!(html.contains("Sample Photographer"));

    // Background facts, each as a row header.
    for label in ["Born", "Entered parliament", "Parliaments", "Honours"] {
        assert!(
            html.contains(&format!("<th class=\"label\" scope=\"row\">{label}</th>")),
            "{label} row missing"
        );
    }
    assert!(html.contains("BA (Hons)"), "qualifications missing");

    // Positions, bills raised and election history tables.
    assert!(html.contains("Positions held"));
    assert!(html.contains("Bills raised"));
    assert!(html.contains("Election history"));
    assert!(html.contains("Sample election"));
    assert!(html.contains("+1.2"), "a positive swing is signed");

    // The Person node picks up the image and the Wikipedia sameAs.
    let value = jsonld(&html).expect("person json-ld");
    let person_node = &nodes(&value)[0];
    assert!(person_node["image"].is_string());
    assert_eq!(
        person_node["sameAs"],
        "https://en.wikipedia.org/wiki/Sample"
    );
}

#[test]
fn electorate_pages_show_both_result_tables() {
    let data = sample_data();
    let with_tcp = data
        .electorates
        .iter()
        .find(|e| {
            data.election_for_electorate(&e.slug)
                .is_some_and(|r| r.tcp.len() == 2)
        })
        .expect("sample data has a two-candidate-preferred result");
    let html = render(&data, &pages::electorate_page(&data, with_tcp));

    assert!(html.contains("two-candidate preferred"));
    assert!(html.contains("first preferences"));
    assert!(html.contains("52,000"), "vote counts are grouped");
    assert!(html.contains("\u{2713}"), "the elected candidate is marked");
    assert!(html.contains("CC BY 4.0"), "AEC attribution missing");
}

#[test]
fn index_pages_ship_their_filter_script_and_feedback() {
    let data = sample_data();
    for page in [
        pages::people_index(&data),
        pages::divisions_index(&data),
        pages::bills_index(&data),
        pages::electorates_index(&data),
    ] {
        let html = render(&data, &page);
        assert!(
            page.page_script.is_some(),
            "no filter script on {}",
            page.path
        );
        assert!(html.contains("aria-live=\"polite\""), "{}", page.path);
        assert!(html.contains("id=\"filter-clear\""), "{}", page.path);
        assert!(html.contains("<script type=\"module\">"), "{}", page.path);
    }
}

#[test]
fn seat_bars_mark_the_majority_and_link_each_party() {
    let data = sample_data();
    for house in [House::Representatives, House::Senate] {
        let bar = seat_bar(&data, house);
        assert!(bar.contains("class=\"majority\">majority "), "{house}");
        assert!(bar.contains("class=\"tick\""), "{house}");
        assert!(bar.contains("class=\"tick-label\""), "{house}");
        // Every segment is a party link with its own accessible name.
        let segments = bar.matches("<a href=\"/parties/").count();
        assert!(segments >= 2, "expected linked segments for {house}");
        assert_eq!(bar.matches("aria-label=\"").count(), 1 + segments / 2);
    }
}

#[test]
fn feeds_are_well_formed_and_newest_first() {
    let data = sample_data();
    let out = tempdir();
    feeds::write_feeds(&out, SITE_URL, &data).expect("feeds write");

    for (rel, expected_title) in [
        ("divisions/feed.xml", "pollywiki: divisions"),
        ("bills/feed.xml", "pollywiki: bills"),
    ] {
        let xml = std::fs::read_to_string(out.join(rel)).expect(rel);
        assert!(
            xml.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>"),
            "{rel}"
        );
        assert!(
            xml.contains(&format!("<title>{expected_title}</title>")),
            "{rel}"
        );
        assert!(xml.contains(&format!("href=\"{SITE_URL}/{rel}\"")), "{rel}");
        assert!(xml.ends_with("</feed>"), "{rel}");
        // Tags balance, and entries run newest first.
        assert_eq!(
            xml.matches("<entry>").count(),
            xml.matches("</entry>").count()
        );
        let stamps: Vec<&str> = xml
            .match_indices("<updated>")
            .map(|(i, m)| xml[i + m.len()..].split('<').next().expect("stamp"))
            .skip(1) // the feed's own stamp
            .collect();
        assert!(!stamps.is_empty(), "{rel} has no entries");
        let mut sorted = stamps.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(stamps, sorted, "{rel} entries are not newest first");
        for stamp in &stamps {
            assert!(stamp.ends_with("T00:00:00Z"), "bad stamp {stamp} in {rel}");
        }
    }

    // Division summaries carry the outcome in the site's vocabulary.
    let divisions = std::fs::read_to_string(out.join("divisions/feed.xml")).unwrap();
    assert!(divisions.contains("\u{b7} Carried") || divisions.contains("\u{b7} Negatived"));
    std::fs::remove_dir_all(&out).ok();
}

#[test]
fn division_cards_render_for_every_sample_division() {
    let data = sample_data();
    let cards = og::Cards::load().expect("vendored fonts decode");
    let out = tempdir();
    let written = cards.write_all(&out, &data).expect("cards write");

    assert_eq!(written.len(), data.divisions.len());
    for division in &data.divisions {
        let rel = written.get(&division.id).expect("card for every division");
        assert_eq!(rel, &og::card_path(division));
        let bytes = std::fs::read(out.join(rel.trim_start_matches('/'))).expect("card on disk");
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"), "not a png: {rel}");
    }
    std::fs::remove_dir_all(&out).ok();
}

#[test]
fn leadership_lists_one_row_per_person_and_role() {
    let data = sample_data();
    for party in &data.parties {
        let html = render(&data, &pages::party_page(&data, party));
        let Some(section) = html.split("Parliamentary leadership").nth(1) else {
            continue;
        };
        let table = section.split("</table>").next().expect("leadership table");
        // The Handbook records a continuing role once per ministry; the table
        // names roles, not ministries, so each pair may appear only once.
        let mut seen: Vec<&str> = Vec::new();
        for row in table.split("<tr>").skip(1) {
            let cells = row.split("</td>").next().unwrap_or("");
            if let Some(role) = cells.split("<td>").nth(1) {
                assert!(
                    !seen.contains(&role),
                    "duplicate leadership row {role} on /parties/{}/",
                    party.slug
                );
                seen.push(role);
            }
        }
    }

    // The sample data records Prime Minister under two ministries; the row must
    // collapse to one, dated from the earlier of them.
    let alp = data
        .parties
        .iter()
        .find(|p| p.slug == "example-party")
        .expect("sample party");
    let html = render(&data, &pages::party_page(&data, alp));
    assert_eq!(html.matches("<td>Prime Minister</td>").count(), 1);
    assert!(
        html.contains("3 May 2025"),
        "expected the earliest start date"
    );
    assert!(!html.contains("20 May 2025"));
}

#[test]
fn occupation_rows_fill_their_columns_or_span_them() {
    let data = sample_data();
    let person = data
        .people
        .iter()
        .find(|p| {
            p.background
                .as_ref()
                .is_some_and(|b| !b.occupations.is_empty())
        })
        .expect("sample data has occupations");
    let html = render(&data, &pages::person_page(&data, person));
    let table = html
        .split("Occupations before parliament")
        .nth(1)
        .and_then(|s| s.split("</table>").next())
        .expect("occupations table");
    // "CEO of the ..." used to fall through to the verbatim row.
    assert!(table.contains("<td>CEO</td><td>Sample Business Network</td>"));
    assert!(table.contains("<td>Policy Analyst</td><td>Example Treasury</td>"));
    // Anything unparseable still spans the row rather than sitting under Role.
    assert!(table.contains("<td colspan=\"3\">Grazier and small business owner</td>"));
}

#[test]
fn procedure_notes_match_the_motions_they_explain() {
    let label = |name: &str| procedure_for(name).map(|p| p.label);
    // Every wording of the suspension motion that appears on the live index.
    assert_eq!(
        label("Business \u{2014} Suspension of Standing and Sessional Orders"),
        Some("Suspension of standing orders")
    );
    assert_eq!(
        label("Motions - Telecommunications - Suspend the usual procedural rules"),
        Some("Suspension of standing orders")
    );
    assert_eq!(
        label("Motions - National Security - Suspend the usual rules"),
        Some("Suspension of standing orders")
    );
    assert_eq!(
        label("Bills \u{2014} Example Bill 2026; Second Reading"),
        Some("Second reading")
    );
    assert_eq!(
        label("Documents - Order for the Production of Documents"),
        Some("Order for the production of documents")
    );
    assert!(label("Matters of Urgency \u{2014} Senior Australians").is_none());
}

#[test]
fn dot_strips_agree_with_the_stage_they_report() {
    let data = sample_data();
    for bill in &data.bills {
        let stage = bill_stage(bill);
        let dots = bill_dots(bill);
        assert!(stage <= 4);
        assert_eq!(dots.matches("class=\"on\"").count(), stage as usize);
        assert_eq!(dots.matches("class=\"off\"").count(), 4 - stage as usize);
        assert!(dots.contains(&format!("Stage {stage} of 4")));
        // The hover text mirrors the accessible name, per the review feedback.
        assert_eq!(dots.matches(&format!("Stage {stage} of 4")).count(), 2);
    }
    // An Act must be fully filled; a bill still before its own chamber must not.
    let act = data
        .bills
        .iter()
        .find(|b| b.status == "Act")
        .expect("sample data has an Act");
    assert_eq!(bill_stage(act), 4);
}

#[test]
fn month_dividers_label_and_pluralise() {
    assert_eq!(month_label("2026-08"), "August 2026");
    assert!(ledger_month("2026-08", 1, "division").contains("1 division<"));
    assert!(ledger_month("2026-08", 12, "division").contains("12 divisions<"));
}

#[test]
fn a_full_build_emits_every_artefact() {
    let out = tempdir().join("site");
    crate::build_site(&out, &bundles(), SITE_URL).expect("build");
    let data = sample_data();

    // One directory index per page, plus the out-of-sitemap 404.
    for page in all_pages(&data) {
        if page.path == "/404/" {
            continue;
        }
        let file = out
            .join(page.path.trim_start_matches('/'))
            .join("index.html");
        assert!(file.is_file(), "missing {}", file.display());
    }
    assert!(out.join("404.html").is_file());

    // Hashed stylesheet, vendored fonts, public files and the search index.
    let css: Vec<PathBuf> = std::fs::read_dir(out.join("_assets"))
        .expect("_assets")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "css"))
        .collect();
    assert_eq!(css.len(), 1, "expected exactly one hashed stylesheet");
    let stylesheet = std::fs::read_to_string(&css[0]).expect("stylesheet");
    assert!(
        stylesheet.contains("--faint:"),
        "tokens missing from the bundle"
    );
    assert!(
        stylesheet.contains("@font-face"),
        "fonts.css not concatenated"
    );
    assert!(out
        .join("_assets/fonts/newsreader-latin-wght-italic.woff2")
        .is_file());
    assert!(out.join("favicon.svg").is_file());
    assert!(out.join("robots.txt").is_file());
    assert!(out.join("og-default.png").is_file());
    assert!(out.join("quick-search.json").is_file());
    assert!(out.join("pagefind").is_dir(), "pagefind index missing");

    // Feeds, sitemaps and one card per division.
    assert!(out.join("divisions/feed.xml").is_file());
    assert!(out.join("bills/feed.xml").is_file());
    assert!(out.join("sitemap-index.xml").is_file());
    for division in &data.divisions {
        assert!(
            out.join(og::card_path(division).trim_start_matches('/'))
                .is_file(),
            "card missing for {}",
            division.id
        );
    }

    // The pages reference the stylesheet that was actually written.
    let name = css[0]
        .file_name()
        .and_then(|n| n.to_str())
        .expect("css name");
    let home = std::fs::read_to_string(out.join("index.html")).expect("home");
    assert!(home.contains(&format!("href=\"/_assets/{name}\"")));

    std::fs::remove_dir_all(&out).ok();
}

#[test]
fn the_sitemap_sorts_naturally_and_dates_what_it_can() {
    let out = tempdir().join("sitemap");
    crate::build_site(&out, &bundles(), SITE_URL).expect("build");
    let xml = std::fs::read_to_string(out.join("sitemap-0.xml")).expect("sitemap");
    let data = sample_data();

    let locs: Vec<&str> = xml
        .match_indices("<loc>")
        .map(|(i, m)| xml[i + m.len()..].split('<').next().expect("loc"))
        .collect();
    assert_eq!(
        locs.len(),
        all_pages(&data).len() - 2,
        "the 404 and the search page are not in the sitemap"
    );
    assert!(locs.iter().all(|l| l.starts_with(SITE_URL)));
    assert!(!xml.contains("/404/"));
    assert!(!xml.contains("/search/"), "noindex pages stay out");

    // Digit runs compare numerically, so s996 precedes s1138.
    let index: Vec<usize> = ["/bills/sample-1/", "/bills/sample-2/", "/bills/sample-3/"]
        .iter()
        .map(|p| {
            locs.iter()
                .position(|l| l.ends_with(p))
                .expect("bill in sitemap")
        })
        .collect();
    let mut sorted = index.clone();
    sorted.sort_unstable();
    assert_eq!(index, sorted, "bill urls are not in natural order");

    // Divisions and bills carry lastmod; the static pages do not.
    for division in &data.divisions {
        let url = format!(
            "{SITE_URL}/divisions/{}/{}/",
            division.house,
            division_key(division)
        );
        assert!(
            xml.contains(&format!(
                "<loc>{url}</loc><lastmod>{}</lastmod>",
                division.date
            )),
            "no lastmod for {url}"
        );
    }
    assert!(xml.contains(&format!("<loc>{SITE_URL}/about/</loc></url>")));

    // Every member with a recorded vote is dated by their newest division.
    for person in &data.people {
        let url = format!("{SITE_URL}/people/{}/", person.slug);
        let newest = data
            .votes_for_person(&person.slug)
            .iter()
            .map(|v| v.division.date.clone())
            .max();
        match newest {
            Some(date) => assert!(
                xml.contains(&format!("<loc>{url}</loc><lastmod>{date}</lastmod>")),
                "no lastmod for {url}"
            ),
            None => assert!(xml.contains(&format!("<loc>{url}</loc></url>"))),
        }
    }

    // The index points at the child sitemap and dates it from the newest page,
    // which is whichever moved last: a division or a bill step.
    let index = std::fs::read_to_string(out.join("sitemap-index.xml")).expect("index");
    let newest = xml
        .match_indices("<lastmod>")
        .map(|(i, m)| xml[i + m.len()..].split('<').next().expect("lastmod"))
        .max()
        .expect("sample pages carry dates");
    assert!(index.contains(&format!("{SITE_URL}/sitemap-0.xml</loc><lastmod>{newest}")));
    // The front page turns over with them.
    assert!(xml.contains(&format!(
        "<loc>{SITE_URL}/</loc><lastmod>{newest}</lastmod>"
    )));

    std::fs::remove_dir_all(&out).ok();
}

#[test]
fn navigation_pages_ask_not_to_be_indexed() {
    let data = sample_data();
    let robots = |page: &Page| {
        crate::layout::render(&data, SITE_URL, "/site.css", page)
            .contains("<meta name=\"robots\" content=\"noindex, follow\">")
    };
    assert!(robots(&pages::search_page()), "search page needs noindex");
    assert!(robots(&pages::not_found()), "404 needs noindex");
    assert!(
        !robots(&pages::home(&data)),
        "the record itself is indexable"
    );
}

#[test]
fn the_dev_server_routes_directories_assets_and_misses() {
    use std::io::{BufRead, BufReader, Write};

    let out = tempdir().join("served");
    crate::build_site(&out, &bundles(), SITE_URL).expect("build");

    // Bind an ephemeral port so parallel tests never collide, then serve on a
    // background thread; serve() runs until the process ends.
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("probe bind");
    let port = probe.local_addr().expect("addr").port();
    drop(probe);
    let serve_dir = out.clone();
    std::thread::spawn(move || {
        let _ = crate::serve(&serve_dir, port);
    });

    let get = |path: &str| -> (String, String) {
        // The server is coming up on another thread; retry the connect briefly.
        let mut stream = None;
        for _ in 0..100 {
            match std::net::TcpStream::connect(("127.0.0.1", port)) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(_) => std::thread::yield_now(),
            }
        }
        let mut stream = stream.expect("server accepted a connection");
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        )
        .expect("request");
        let mut reader = BufReader::new(&stream);
        let mut status = String::new();
        reader.read_line(&mut status).expect("status line");
        let mut content_type = String::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).expect("header") == 0 || line == "\r\n" {
                break;
            }
            if line.to_ascii_lowercase().starts_with("content-type:") {
                content_type = line.trim().to_string();
            }
        }
        (status.trim().to_string(), content_type)
    };

    // A directory path resolves to its index.html.
    let (status, content_type) = get("/divisions/");
    assert!(status.contains("200"), "got {status}");
    assert!(content_type.contains("text/html"), "got {content_type}");

    // Query strings are ignored when resolving the file.
    assert!(get("/bills/?q=example").0.contains("200"));

    // Static assets are served with their own content types.
    assert!(get("/divisions/feed.xml").1.contains("application/xml"));
    assert!(get("/quick-search.json").1.contains("application/json"));
    assert!(get("/favicon.svg").1.contains("image/svg+xml"));

    // A miss falls back to the 404 page rather than an empty response.
    let (status, content_type) = get("/nothing/here/");
    assert!(
        status.contains("200"),
        "the 404 body is served, got {status}"
    );
    assert!(content_type.contains("text/html"));

    std::fs::remove_dir_all(&out).ok();
}

#[test]
fn the_freshness_line_labels_every_source_and_flags_stale_ones() {
    let data = sample_data();
    assert!(
        !data.meta.sources.is_empty(),
        "sample meta should carry sources"
    );
    let html = render(&data, &pages::home(&data));

    // Each source id is rendered under its human label.
    for label in [
        "Wikidata",
        "AEC",
        "APH bills",
        "They Vote For You",
        "Parliamentary Handbook",
        "AEC profiles",
    ] {
        assert!(
            html.contains(label),
            "{label} missing from the freshness line"
        );
    }
    // A failed sync is marked stale rather than quietly shown as current.
    assert!(html.contains("class=\"stale\""));
    assert!(html.contains("class=\"ok\""));
    assert!(html.contains("built "));

    // The data-sources page reports the same syncs.
    let sources_page = render(&data, &pages::data_sources(&data));
    assert!(sources_page.contains("Wikidata"));
}

#[test]
fn command_line_arguments_parse_to_the_documented_defaults() {
    let args = |list: &[&str]| {
        crate::parse_args(&list.iter().map(|s| s.to_string()).collect::<Vec<String>>())
    };

    let bare = args(&[]);
    assert!(!bare.help);
    assert_eq!(bare.out_dir, PathBuf::from("dist"));
    assert!(bare.serve_port.is_none(), "no --serve means build and exit");

    let served = args(&["--out", "public", "--serve", "8080"]);
    assert_eq!(served.out_dir, PathBuf::from("public"));
    assert_eq!(served.serve_port, Some(8080));

    // --serve with no port, or an unparseable one, uses the default port.
    assert_eq!(args(&["--serve"]).serve_port, Some(4321));
    assert_eq!(args(&["--serve", "not-a-port"]).serve_port, Some(4321));
    // --out with no value keeps the default rather than emptying the path.
    assert_eq!(args(&["--out"]).out_dir, PathBuf::from("dist"));

    assert!(args(&["--help"]).help);
    assert!(args(&["-h"]).help);
}

/// A unique scratch directory under the target dir, so tests never collide.
fn tempdir() -> PathBuf {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/render-tests");
    // Thread ids are unique within a test binary and need no clock or rng.
    let unique = format!("{:?}", std::thread::current().id())
        .replace(|c: char| !c.is_ascii_alphanumeric(), "");
    let dir = base.join(unique);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}
