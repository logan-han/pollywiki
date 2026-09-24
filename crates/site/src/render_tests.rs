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
use std::collections::HashSet;
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
        pages::about_index(data),
        pages::data_sources(data),
        pages::methodology(data),
        pages::corrections(data),
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
    // A first-preference table closes on the AEC's informal ballots.
    assert!(
        data.elections.iter().any(|r| r.informal().is_some()),
        "sample results need an informal row to exercise the table foot"
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
    let page = pages::about_index(&data);

    data.meta.sample = true;
    assert!(render(&data, &page).contains("class=\"sample-banner\""));

    data.meta.sample = false;
    assert!(!render(&data, &page).contains("class=\"sample-banner\""));
}

#[test]
fn the_quick_search_placeholder_fits_a_phone_but_the_name_stays_whole() {
    let data = sample_data();
    let html = render(&data, &pages::about_index(&data));
    // At the 16px phone size the header field holds about 26 characters; the
    // placeholder is shortened to fit and the accessible name keeps the verb.
    assert!(html.contains("placeholder=\"Bill, person or electorate\""));
    assert!(html.contains("aria-label=\"Find a bill, person or electorate\""));
}

#[test]
fn the_quick_search_is_a_labelled_search_form_that_works_without_script() {
    let data = sample_data();
    for page in [pages::home(&data), pages::search_page()] {
        let html = render(&data, &page);
        let where_ = &page.path;
        // A search landmark with its own name, beside Pagefind's on /search/,
        // and a real form: Enter goes to /search/?q= with no script at all.
        assert!(
            html.contains("<form class=\"quick-search\" role=\"search\" aria-label=\"Quick find\" action=\"/search/\">"),
            "{where_}"
        );
        assert_eq!(html.matches("aria-label=\"Quick find\"").count(), 1);
        let form = &html[html.find("<form class=\"quick-search\"").unwrap()..];
        let form = &form[..form.find("</form>").expect("the form is closed")];
        assert!(
            form.contains("<input type=\"search\" id=\"quick-search-input\" name=\"q\""),
            "{where_}"
        );
        // The list scrolls, so without tabindex=-1 Chrome makes it a tab stop
        // of its own, where the arrow keys do nothing.
        assert!(
            form.contains("role=\"listbox\" aria-label=\"Suggestions\" tabindex=\"-1\" hidden>"),
            "{where_}"
        );
        // The script says how many suggestions there are here.
        assert!(
            form.contains(
                "<p id=\"quick-search-status\" class=\"visually-hidden\" role=\"status\"></p>"
            ),
            "{where_}"
        );
        assert!(!html.contains("<div class=\"quick-search\""), "{where_}");
    }
}

#[test]
fn a_nav_link_is_the_current_page_only_on_its_own_index() {
    let data = sample_data();
    let nav = |html: &str| -> String {
        let start = html.find("<nav class=\"site-nav\"").expect("primary nav");
        html[start..start + html[start..].find("</nav>").unwrap()].to_string()
    };

    let index = nav(&render(&data, &pages::people_index(&data)));
    assert!(index.contains("<a href=\"/people/\" aria-current=\"page\">People</a>"));
    assert_eq!(index.matches("aria-current").count(), 1);

    // On a profile the link marks the section, not the page itself.
    let person = &data.people[0];
    let profile = nav(&render(&data, &pages::person_page(&data, person)));
    assert!(profile.contains("<a href=\"/people/\" aria-current=\"true\">People</a>"));
    assert!(!profile.contains("aria-current=\"page\""));
    assert_eq!(profile.matches("aria-current").count(), 1);

    // Pages outside the six sections mark nothing.
    let about = nav(&render(&data, &pages::about_index(&data)));
    assert!(!about.contains("aria-current"));

    assert_eq!(layout::nav_state("/bills/", "/bills/"), Some("page"));
    assert_eq!(
        layout::nav_state("/bills/sample-1/", "/bills/"),
        Some("true")
    );
    assert_eq!(layout::nav_state("/", "/bills/"), None);
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

    // The date leads the row, as on the bills index; the event closes it. The
    // chamber suffix is dropped from the visible text but kept in the title.
    assert!(
        html.contains("<li><span class=\"when\"><time datetime=\"2025-08-04\">4 Aug</time></span>")
    );
    assert!(html.contains(">Referred to Federation Chamber</span></li>"));
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
fn home_folds_a_days_divisions_on_one_matter_into_a_series() {
    let data = sample_data();
    let html = render(&data, &pages::home(&data));
    let section = html
        .split("Latest divisions")
        .nth(1)
        .and_then(|s| s.split("Latest bill activity").next())
        .expect("latest divisions section");

    // Three Senate divisions on one bill in one day become one entry whose
    // steps run in the order the chamber took them, each labelled by stage.
    let senate = section
        .split("<li class=\"ledger-series\" data-house=\"senate\"")
        .nth(1)
        .and_then(|s| s.split("</details></li>").next())
        .expect("senate series");
    assert!(senate.contains(
        "<span class=\"matter\">Bills — Sex Discrimination Amendment (Restoring Common Sense and Recognising Biological Sex) Bill 2026</span>"
    ));
    assert!(senate.contains("3 divisions · 1 carried · 2 negatived"));
    let order: Vec<usize> = ["Division 3<", "Division 4<", "Division 5<"]
        .iter()
        .map(|needle| senate.find(needle).expect(needle))
        .collect();
    assert!(
        order.windows(2).all(|w| w[0] < w[1]),
        "steps are not in chamber order"
    );
    assert!(senate.contains("/divisions/senate/2025-08-05-4/\">Second Reading</a>"));
    // The note under a stage is the first sentence of the machine-written
    // context, marked as such.
    assert!(senate.contains("<span class=\"q\">The Senate considered a second reading amendment moved by Senator Morgan Rossi.<span class=\"ai-mark\" title=\"AI-generated\">AI</span></span>"));
    // Outcomes in order, readable without colour.
    assert!(senate.contains("aria-label=\"In order: Negatived, Negatived, Carried\""));
    assert!(senate.contains(
        "<i class=\"negatived\"></i><i class=\"negatived\"></i><i class=\"carried\"></i>"
    ));
    // Machine-written labels carry the tag and the credit.
    assert!(senate.contains("ai-tag"));
    assert!(senate.contains("How this works."));

    // Two House divisions with identical names: the opened row links to the
    // bill under the bill's own title, They Vote For You's written context
    // labels the step that has it, and the machine-written note labels the
    // other, marked.
    let house = section
        .split("<li class=\"ledger-series\" data-house=\"representatives\"")
        .nth(1)
        .and_then(|s| s.split("</details></li>").next())
        .expect("house series");
    assert!(house.contains(
        "</summary><p class=\"series-bill\">Bill: <a href=\"/bills/sample-1/\">Demonstration Data Bill 2025</a></p><ol class=\"series-steps\">"
    ));
    // The summary is a pure toggle: no control nested inside it.
    for summary in [&house, &senate] {
        let summary = summary.split("</summary>").next().expect("summary");
        assert!(!summary.contains("<a "), "a link inside a summary");
    }
    assert!(house
        .contains("<span class=\"matter\">Demonstration Data Bill 2025 - Second Reading</span>"));
    assert!(house.contains(
        "/divisions/representatives/2025-08-01-2/\">The majority voted in favour of a sample motion to demonstrate how context summaries render, which means it passed.</a>"
    ));
    assert!(house.contains(
        "/divisions/representatives/2025-08-01-3/\">The House considered an amendment moved by Jordan Nguyen to the second reading motion.</a><span class=\"ai-mark\" title=\"AI-generated\">AI</span>"
    ));
    assert!(house.contains("They Vote For You</a> volunteers (ODbL)"));
    assert!(
        !house.contains("Division 2</a>"),
        "a described step is not linked by number"
    );

    // With nothing to describe a step, the number itself carries the link
    // rather than being repeated as the label.
    let bare: Vec<pollywiki_schema::Division> = data
        .divisions
        .iter()
        .filter(|d| d.house == pollywiki_schema::House::Representatives && d.date == "2025-08-01")
        .map(|d| pollywiki_schema::Division {
            summary: None,
            summary_kind: None,
            ai_summary: None,
            ..d.clone()
        })
        .collect();
    let series = crate::data::DivisionSeries {
        house: pollywiki_schema::House::Representatives,
        date: "2025-08-01",
        matter: "Demonstration Data Bill 2025 - Second Reading",
        divisions: bare.iter().collect(),
    };
    let row = crate::components::series_row(&data, &series);
    assert!(row.contains(
        "<span class=\"n\"><a href=\"/divisions/representatives/2025-08-01-3/\">Division 3</a></span><span class=\"what\"></span>"
    ));
    assert!(!row.contains("series-credit"), "no descriptions, no credit");

    // A matter divided on once stays a plain ledger row.
    assert!(section.contains("<li data-house=\"representatives\""));
    assert!(section.contains("Sample Motion - That the example be noted"));
    assert_eq!(section.matches("<li class=\"ledger-series\"").count(), 2);
}

#[test]
fn divisions_index_groups_into_months_that_match_their_runs() {
    let data = sample_data();
    let html = render(&data, &pages::divisions_index(&data));

    assert!(html.contains("id=\"division-filter-text\""));
    assert!(html.contains("id=\"filter-count\""));
    assert!(html.contains("id=\"filter-clear\""));

    // Every month divider reports the number of divisions that follow it: a
    // plain row is one, and a folded series is as many as its data-count.
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
        let run = chunk.split("</ul>").next().unwrap_or(chunk);
        let plain = run.matches("<li data-house=").count();
        let folded: usize = run
            .split("<li class=\"ledger-series\"")
            .skip(1)
            .map(|entry| {
                let start = entry.find("data-count=\"").expect("series count") + 12;
                entry[start..]
                    .split('"')
                    .next()
                    .and_then(|n| n.parse::<usize>().ok())
                    .expect("series count value")
            })
            .sum();
        let rows = plain + folded;
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
fn divisions_index_folds_a_days_divisions_on_one_matter_into_a_series() {
    let data = sample_data();
    let html = render(&data, &pages::divisions_index(&data));
    let list = html
        .split("<ul class=\"ledger\" id=\"division-list\">")
        .nth(1)
        .and_then(|s| s.split("</ul>").next())
        .expect("division list");

    // As on home, one chamber's divisions on one matter in one sitting day
    // fold into one entry that says how many divisions it stands for, dated
    // like the plain rows around it: no year under the month divider.
    assert!(list.contains(
        "<li class=\"ledger-series\" data-house=\"senate\" data-count=\"3\"><details><summary><span class=\"when\"><time datetime=\"2025-08-05\">5 Aug</time></span>"
    ));
    assert!(list.contains(
        "<li class=\"ledger-series\" data-house=\"representatives\" data-count=\"2\"><details><summary><span class=\"when\"><time datetime=\"2025-08-01\">1 Aug</time></span>"
    ));
    // A matter divided on once stays a plain row.
    assert_eq!(list.matches("<li data-house=").count(), 1);

    // Folding hides no division: each is linked exactly once, from its step
    // or its row.
    for d in &data.divisions {
        let href = format!("href=\"/divisions/{}/{}/\"", d.house, division_key(d));
        assert_eq!(list.matches(&href).count(), 1, "{href}");
    }

    // The filter weighs each entry by the divisions it holds, so its counts
    // stay in divisions.
    assert!(html.contains("Number(row.dataset.count ?? 1)"));

    // A series answers to its divisions' names and nothing more: the matter,
    // and the stage that ends each name. A stage link is marked so the filter
    // can find it; a step labelled by a written description is not, so a
    // folded matter never matches on words a plain row would not.
    assert!(html.contains("row.querySelectorAll('.series-steps a.stage')"));
    let senate = list
        .split("<li class=\"ledger-series\" data-house=\"senate\"")
        .nth(1)
        .and_then(|s| s.split("</details></li>").next())
        .expect("senate series");
    assert!(senate.contains(
        "<span class=\"what\"><a class=\"stage\" href=\"/divisions/senate/2025-08-05-3/\">First Reading</a>"
    ));
    assert_eq!(senate.matches("<a class=\"stage\"").count(), 3);
    let house = list
        .split("<li class=\"ledger-series\" data-house=\"representatives\"")
        .nth(1)
        .and_then(|s| s.split("</details></li>").next())
        .expect("house series");
    assert!(house
        .contains("<span class=\"matter\">Demonstration Data Bill 2025 - Second Reading</span>"));
    assert!(
        !house.contains("class=\"stage\""),
        "a description marked as a name"
    );
}

#[test]
fn a_month_strip_links_every_divider_with_the_same_count() {
    let data = sample_data();
    for (page, noun) in [
        (pages::divisions_index(&data), "division"),
        (pages::bills_index(&data), "bill"),
    ] {
        let html = render(&data, &page);
        let strip = html
            .split("<nav class=\"month-jump\" id=\"month-jump\" aria-label=\"Jump to month\">")
            .nth(1)
            .and_then(|s| s.split("</nav>").next())
            .expect("month strip");
        // The strip comes after the filter's feedback and before the list.
        let at = |needle: &str| html.find(needle).expect(needle);
        assert!(at("id=\"filter-empty\"") < at("class=\"month-jump\""));
        assert!(at("class=\"month-jump\"") < at("<li class=\"ledger-month\""));

        let count_after = |text: &str| -> usize {
            text.split("<span class=\"n\">")
                .nth(1)
                .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|n| n.parse().ok())
                .expect("a count")
        };
        let links: Vec<(String, usize)> = strip
            .split("<a href=\"#m-")
            .skip(1)
            .map(|a| {
                let key = a.split('"').next().expect("link key");
                assert!(a.contains(&format!("data-month=\"{key}\"")), "{key}");
                (key.to_string(), count_after(a))
            })
            .collect();
        let dividers: Vec<(String, usize)> = html
            .split("<li class=\"ledger-month\" id=\"m-")
            .skip(1)
            .map(|li| {
                let key = li.split('"').next().expect("divider key");
                assert!(li.contains(&format!("data-month=\"{key}\"")), "{key}");
                (key.to_string(), count_after(li))
            })
            .collect();
        assert!(dividers.len() >= 2, "{}", page.path);
        assert_eq!(
            html.matches("<li class=\"ledger-month\"").count(),
            dividers.len(),
            "a divider with no id on {}",
            page.path
        );
        assert_eq!(links, dividers, "{}", page.path);

        // Short month names keep the strip to a few lines on a phone; the
        // noun is there for a screen reader.
        assert!(strip.contains(&format!("<span class=\"visually-hidden\"> {noun}")));
    }

    let divisions = render(&data, &pages::divisions_index(&data));
    assert!(divisions.contains(
        "<a href=\"#m-2025-08\" data-month=\"2025-08\">Aug 2025 <span class=\"n\">5</span><span class=\"visually-hidden\"> divisions</span></a>"
    ));
    assert!(divisions.contains(
        "<a href=\"#m-2025-07\" data-month=\"2025-07\">Jul 2025 <span class=\"n\">1</span><span class=\"visually-hidden\"> division</span></a>"
    ));
}

#[test]
fn the_people_filter_reaches_former_members_and_whole_state_names() {
    let data = sample_data();
    let html = render(&data, &pages::people_index(&data));

    // A sitting card answers to its state spelt out as well as its code.
    assert!(html.contains(
        "<div class=\"person-cell\" data-name=\"morgan rossi  tas tasmania\" data-house=\"senate\" data-group=\"independent\">"
    ));

    // Former members sit in their own section, each card wrapped for the
    // text filter but with no chamber or party to match.
    let former = html
        .split("<section id=\"former-members\">")
        .nth(1)
        .and_then(|s| s.split("</section>").next())
        .expect("former members section");
    assert!(former.starts_with("<h2>Former members</h2>"));
    assert!(former.contains(
        "<div class=\"person-cell\" data-name=\"casey o'brien oldbridge vic victoria\"><a class=\"person-card\""
    ));
    assert_eq!(
        former.matches("<div class=\"person-cell\"").count(),
        data.former().count()
    );
    assert!(!former.contains("data-house="));
    assert!(!former.contains("data-group="));

    // The script scopes the sitting grid and the former section apart, and
    // greys out a party pill that could only empty the grid.
    assert!(html.contains("#person-grid .person-cell"));
    assert!(html.contains("#former-members .person-cell"));
    assert!(html.contains("button.disabled = !possible"));
}

#[test]
fn index_filters_keep_their_state_in_the_url_and_match_words_in_any_order() {
    let data = sample_data();
    for (page, params) in [
        (
            pages::people_index(&data),
            ["q", "house", "party"].as_slice(),
        ),
        (pages::divisions_index(&data), ["q", "house"].as_slice()),
        (pages::bills_index(&data), ["q", "status"].as_slice()),
        (pages::electorates_index(&data), ["q"].as_slice()),
    ] {
        let html = render(&data, &page);
        let path = &page.path;
        // The phone keyboard offers Done, which puts it away.
        assert_eq!(html.matches("enterkeyhint=\"done\"").count(), 1, "{path}");
        assert!(html.contains("matchMedia('(pointer: coarse)')"), "{path}");

        // Every filter reads its state from the URL on load and writes it
        // back, leaving any hash where it is.
        assert!(
            html.contains("new URLSearchParams(location.search)"),
            "{path}"
        );
        assert!(html.contains("history.replaceState("), "{path}");
        assert!(html.contains("${location.hash}"), "{path}");
        for param in params {
            assert!(
                html.contains(&format!("params.get('{param}')")),
                "{path} ?{param}="
            );
        }

        // Folded terms in any order, split where the quick search splits a
        // name, and a count that settles before it is announced.
        assert!(html.contains(".normalize('NFD')"), "{path}");
        assert!(html.contains(r".split(/[^\p{L}\p{N}]+/u)"), "{path}");
        assert!(
            html.contains("terms.every((term) => hay.includes(term))"),
            "{path}"
        );
        assert!(html.contains("settleCount(count,"), "{path}");
        assert!(!html.contains("count.textContent = filtered"), "{path}");
    }

    // The electorate box names everything it matches.
    let electorates = render(&data, &pages::electorates_index(&data));
    assert!(electorates.contains("placeholder=\"Filter by name, state or member\""));
}

#[test]
fn long_lists_can_be_walked_by_heading() {
    let data = sample_data();

    // Each month divider on both dated indexes is a heading under the h1.
    for (html, months) in [
        (
            render(&data, &pages::divisions_index(&data)),
            ["August 2025", "July 2025"].as_slice(),
        ),
        (
            render(&data, &pages::bills_index(&data)),
            ["July 2026", "August 2025"].as_slice(),
        ),
    ] {
        let dividers = html.matches("<li class=\"ledger-month\"").count();
        assert_eq!(html.matches("<h2 class=\"m\">").count(), dividers);
        for month in months {
            assert!(
                html.contains(&format!("<h2 class=\"m\">{month}</h2>")),
                "{month}"
            );
        }
    }

    // On a division page the aye and no columns each head their list, under
    // the section's h2.
    let division = data.divisions.first().expect("a division");
    let html = render(&data, &pages::division_page(&data, division));
    let votes = html
        .split("<h2>Every vote</h2>")
        .nth(1)
        .expect("every vote");
    for column in ["AYE", "NO"] {
        assert!(
            votes.contains(&format!("<div><h3 class=\"col-head\">{column} (")),
            "{column}"
        );
    }
    assert!(!votes.contains("<div class=\"col-head\">"));
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
fn ledger_rows_split_the_tally_and_drop_the_year_only_under_a_month() {
    let data = sample_data();
    let index = render(&data, &pages::divisions_index(&data));

    // The tally is four pieces the grid lines up as columns, with a space
    // between the words for anyone who hears or copies the row as text.
    assert!(index.contains(
        "<span class=\"tally\"><span class=\"result-chip negatived\">Negatived</span> <span class=\"ch\">House</span> <span class=\"fig\">2\u{2013}2</span><span class=\"vote-bar\""
    ));

    // The filter reads each title from the row itself, so the 990 rows carry
    // no lower-cased copy of it.
    assert!(!index.contains("data-text="));
    assert!(index.contains("row.querySelector('.what')"));

    // Under a month divider every date drops its year, a folded series's
    // as well as a plain row's; the <time> keeps it.
    let whens: Vec<&str> = index
        .split("<span class=\"when\">")
        .skip(1)
        .map(|s| s.split("</span>").next().expect("when end"))
        .collect();
    let entries = index.matches("<li data-house=").count()
        + index.matches("<li class=\"ledger-series\"").count();
    assert_eq!(whens.len(), entries);
    assert!(entries < data.divisions.len(), "nothing was folded");
    assert!(whens.contains(&"<time datetime=\"2025-08-05\">5 Aug</time>"));
    for when in &whens {
        let shown = when
            .split_once("\">")
            .map(|(_, rest)| rest.trim_end_matches("</time>"))
            .expect("a <time> element");
        let last = shown.rsplit(' ').next().unwrap_or(shown);
        assert!(
            !(last.len() == 4 && last.bytes().all(|b| b.is_ascii_digit())),
            "a year under a month divider: {when}"
        );
    }

    // A division's sitting day shares one date, so its rows are labelled by
    // number instead; the tally keeps its columns.
    let division = data
        .divisions
        .iter()
        .find(|d| d.house == House::Senate && division_key(d) == "2025-08-05-4")
        .expect("senate division");
    let page = render(&data, &pages::division_page(&data, division));
    assert!(page.contains("<span class=\"when\">Division 5</span>"));
    assert!(!page.contains("<span class=\"when\">5 Aug 2025</span>"));
    assert!(page.contains("<span class=\"fig\">2\u{2013}0</span>"));
    // With no divider above, a row keeps its full date: the latest divisions
    // on home.
    let home = render(&data, &pages::home(&data));
    assert!(home.contains("<span class=\"when\">5 Aug 2025</span>"));
    // A series names its chamber after the strip that stands in for the
    // outcome, so the two fall in the row tallies' columns.
    assert!(home.contains("</span> <span class=\"ch\">Senate</span></span></summary>"));
}

#[test]
fn a_first_column_date_takes_a_date_cell_not_a_figure_cell() {
    let data = sample_data();
    let bill = data
        .bills
        .iter()
        .find(|b| b.id == "sample-1")
        .expect("sample bill");
    let progress = render(&data, &pages::bill_page(&data, bill));
    assert!(progress.contains("<h2>Progress</h2>"));
    assert!(progress.contains("<tr><td class=\"date\">28 Jul 2025</td>"));

    let person = data
        .people
        .iter()
        .find(|p| p.slug == "jordan-nguyen")
        .expect("sample member");
    let record = render(&data, &pages::person_page(&data, person));
    let votes = record
        .split("<h2 id=\"voting-record\">")
        .nth(1)
        .expect("voting record");
    assert!(votes.contains("<th scope=\"col\">Date</th>"));
    // Under its month's header the date drops the year; the <time> keeps it.
    assert!(
        votes.contains("<tr><td class=\"date\"><time datetime=\"2025-08-01\">1 Aug</time></td>")
    );
    assert!(!votes.contains("<td class=\"num\">"));
}

#[test]
fn bills_index_labels_its_columns_and_keys_the_dots_before_the_rows() {
    let data = sample_data();
    let html = render(&data, &pages::bills_index(&data));

    // The key comes before the list, and shows each stage filled as far as a
    // bill at that stage has got.
    let legend = html.find("class=\"dots-legend\"").expect("legend");
    let list = html.find("id=\"bill-rows\"").expect("list");
    assert!(legend < list, "the key should precede the rows");
    let key = html[legend..list].split("</p>").next().expect("legend end");
    for (label, filled) in [
        ("Introduced", 1),
        ("Passed 1st house", 2),
        ("Passed 2nd house", 3),
        ("Assent", 4),
    ] {
        let entry = key
            .split("<span><span class=\"bill-dots\"")
            .find(|entry| entry.contains(&format!("</span>{label}</span>")))
            .expect(label);
        assert_eq!(entry.matches("class=\"on\"").count(), filled, "{label}");
        assert_eq!(
            entry.matches("class=\"off\"").count(),
            4 - filled,
            "{label}"
        );
    }

    // The header row opens the list. It carries neither data-month nor
    // data-status, so the filter script skips it, and it is hidden from
    // assistive tech, which reads each row's own labels instead.
    assert!(html.contains(
        "<ul class=\"bill-list\" id=\"bill-rows\"><li class=\"bill-head\" aria-hidden=\"true\"><span>Moved</span><span>Bill</span><span>Origin</span><span>Stage</span><span>Status</span></li><li class=\"ledger-month\""
    ));

    // Every row names its title as the ledger does, so a phone can lift it
    // above the meta line.
    assert_eq!(
        html.matches("<span class=\"what\"><a href=\"/bills/")
            .count(),
        data.bills.len()
    );
    // The home page keeps its key under the activity list.
    let home = render(&data, &pages::home(&data));
    let activity = home.find("class=\"bill-list activity\"").expect("activity");
    assert!(activity < home.find("class=\"dots-legend\"").expect("home legend"));
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
fn bill_progress_reads_in_chamber_phases_under_a_labelled_head() {
    let data = sample_data();
    // Senate, then House, then assent, which the record places in neither.
    let bill = data.bill_by_id("sample-4").expect("sample bill");
    let html = render(&data, &pages::bill_page(&data, bill));
    let (_, progress) = html.split_once("<h2>Progress</h2>").expect("progress");
    let table = &progress[..progress.find("</table>").expect("table ends")];
    let phase = |chip: &str| {
        format!("<tr class=\"phase\"><th colspan=\"2\" scope=\"rowgroup\">{chip}</th></tr>")
    };
    let expected = format!(
        "<table class=\"progress\"><thead><tr><th scope=\"col\">Date</th><th scope=\"col\">Step</th></tr></thead>\
         <tbody>{}<tr><td class=\"date\">2 Nov 2024</td><td>Introduced</td></tr>\
         <tr><td class=\"date\">1 Dec 2024</td><td>Third reading agreed to</td></tr></tbody>\
         <tbody>{}<tr><td class=\"date\">14 Feb 2025</td><td>Third reading agreed to</td></tr></tbody>\
         <tbody><tr><td class=\"date\">3 Mar 2025</td><td>Assent</td></tr></tbody>",
        phase("<span class=\"chip senate\">Senate</span>"),
        phase("<span class=\"chip house\">House</span>"),
    );
    assert!(table.ends_with(&expected), "progress table was {table}");
    // The chamber moves into the phase header; no step repeats it.
    assert!(!table.contains("(Senate)") && !table.contains("(House of"));
}

#[test]
fn a_bill_page_names_its_divisions_by_stage_unless_they_decided_more() {
    let mut data = sample_data();
    let bill = data.bill_by_id("sample-1").expect("sample bill").clone();
    let id = bill.division_ids[0].clone();
    let rename = |data: &mut SiteData, name: &str| {
        let i = data
            .divisions
            .iter()
            .position(|d| d.id == id)
            .expect("division on the bill");
        data.divisions[i].name = name.to_string();
    };
    let ledger = |data: &SiteData| -> String {
        let html = render(data, &pages::bill_page(data, &bill));
        let (_, list) = html
            .split_once("<h2>Divisions on this bill</h2>")
            .expect("divisions list");
        list[..list.find("</ul>").expect("list ends")].to_string()
    };
    let link = |text: &str| format!("/\">{text}</a></span>");

    // The bill's own heading already names it: the row says the stage.
    rename(
        &mut data,
        "Bills \u{2014} Demonstration Data Bill 2025; Second Reading",
    );
    let own = ledger(&data);
    assert!(own.contains(&link("Second Reading")), "{own}");
    assert!(own.contains("<span class=\"when\">1 Aug 2025</span>"));

    // A cognate debate's division decided other bills too; the full name
    // says so.
    let cognate =
        "Bills \u{2014} Demonstration Data Bill 2025, Placeholder Amendment Bill 2025; Second Reading";
    rename(&mut data, cognate);
    assert!(ledger(&data).contains(&link(cognate)));

    // A name in any other shape is left whole.
    rename(&mut data, "Demonstration Data Bill 2025 - Second Reading");
    assert!(ledger(&data).contains(&link("Demonstration Data Bill 2025 - Second Reading")));
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
    let transcripts: Vec<&pollywiki_schema::Division> = data
        .divisions
        .iter()
        .filter(|d| d.summary_kind == Some(pollywiki_schema::SummaryKind::Transcript))
        .collect();
    assert!(
        !transcripts.is_empty(),
        "sample data has a transcript division"
    );
    for transcript in transcripts {
        let html = render(&data, &pages::division_page(&data, transcript));
        // The Hansard excerpt itself must not be reproduced as TVFY context.
        let excerpt = transcript.summary.as_deref().expect("transcript text");
        assert!(
            !html.contains(excerpt),
            "{} reproduces its excerpt",
            transcript.id
        );
        assert!(!html.contains("Context written by"));
        // The machine-written replacement takes its place, labelled.
        let note = transcript
            .ai_summary
            .as_ref()
            .expect("sample transcripts carry notes");
        assert!(html.contains("ai-tag"));
        assert!(
            html.contains(&pages_text(&note.text)),
            "{} lacks its note",
            transcript.id
        );
        // Both official links are offered in the footer note.
        if let Some(tvfy) = &transcript.links.tvfy {
            assert!(html.contains(tvfy.as_str()));
        }
        if transcript.links.hansard.is_some() {
            assert!(html.contains("Hansard"));
        }
    }
}

/// The first clause of a note as the page shows it: bill titles inside it are
/// linked, so only the text up to the first bill title is safe to search for.
fn pages_text(note: &str) -> String {
    note.split(" Bill ").next().unwrap_or(note).to_string()
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
    // Shares and swings to the AEC's two places, as on the electorate page.
    assert!(
        html.contains("<td class=\"num\">52.00</td><td class=\"num\">+1.20</td>"),
        "a positive swing is signed"
    );

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

    // Both tables share one labelled grid: the deciding count has its
    // column headers too, and a share reads as a bare figure under "%".
    let (_, tcp) = html
        .split_once("two-candidate preferred</h2>")
        .expect("tcp table");
    let (tcp, first) = tcp.split_once("first preferences</h2>").expect("both");
    let head = "<colgroup><col class=\"cand\"><col class=\"party\"><col class=\"votes\"><col class=\"pct\"><col class=\"swing\"></colgroup><thead";
    for table in [tcp, first] {
        assert!(table.contains(&format!("<table class=\"results\">{head}")));
        assert!(table.contains("<th class=\"num\" scope=\"col\">%</th>"));
        assert!(table.contains("<th class=\"num\" scope=\"col\">Swing</th>"));
    }
    assert!(tcp.contains("<td class=\"num\">53.00</td>"), "TCP share");
    assert!(!tcp.contains("%</td>"), "the header carries the unit");
}

#[test]
fn first_preferences_are_shares_of_formal_votes_and_informal_ballots_close_the_table() {
    let mut data = sample_data();
    let slug = data
        .elections
        .iter()
        .find(|r| r.informal().is_some())
        .expect("sample data has informal ballots")
        .electorate_slug
        .clone();
    let electorate = data.electorate_by_slug(&slug).expect("seat").clone();
    let first_prefs = |html: &str| -> String {
        let (_, first) = html
            .split_once("first preferences</h2>")
            .expect("first preferences table");
        first[..first.find("</table>").expect("table ends")].to_string()
    };

    let html = render(&data, &pages::electorate_page(&data, &electorate));
    let table = first_prefs(&html);
    let (rows, foot) = table.split_once("</tbody>").expect("body then foot");
    // Candidates only in the body, each share of the 100,000 formal votes.
    assert!(
        !rows.contains("Informal"),
        "informal is not a candidate row"
    );
    assert!(rows.contains(
        "<td class=\"num\">52,000</td><td class=\"num\">52.00</td><td class=\"num\">+1.20</td>"
    ));
    assert!(rows.contains("<td class=\"num\">48.00</td><td class=\"num\">-1.20</td>"));
    // The informal ballots close it: their count, their share of all
    // 105,000 ballots cast, and the AEC's swing in informality.
    assert_eq!(
        foot,
        "<tfoot><tr><td colspan=\"2\">Informal ballots</td><td class=\"num\">5,000</td><td class=\"num\">4.76</td><td class=\"num\">+0.40</td></tr></tfoot>"
    );
    assert!(html.contains("<p class=\"note\">Candidate percentages are of formal votes; the informal share is of all ballots cast.</p>"));

    // A result stored before the ingest set the informal ballots apart took
    // each share over every ballot. The page works the share out from the
    // votes, so it reads the same either way.
    for c in &mut data
        .elections
        .iter_mut()
        .find(|r| r.electorate_slug == slug)
        .expect("result")
        .first_prefs
    {
        c.pct = pollywiki_schema::JsNum(c.votes as f64 / 105_000.0 * 100.0);
        if c.is_informal() {
            c.name = "Informal Informal".to_string();
        }
    }
    let stale = render(&data, &pages::electorate_page(&data, &electorate));
    assert_eq!(first_prefs(&stale), table);

    // With no informal row there is no foot and no note on the bases.
    data.elections
        .iter_mut()
        .find(|r| r.electorate_slug == slug)
        .expect("result")
        .first_prefs
        .retain(|c| !c.is_informal());
    let formal_only = render(&data, &pages::electorate_page(&data, &electorate));
    assert!(!first_prefs(&formal_only).contains("<tfoot>"));
    assert!(!formal_only.contains("informal share"));
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
        // The majority is already in the label; the figure over the tick is
        // drawn for the eye and kept out of the reading order.
        assert!(
            bar.contains("class=\"tick-label\"") && bar.contains("aria-hidden=\"true\">"),
            "{house}"
        );

        // The bar is one picture with one text alternative and nothing inside
        // it to tab to: a control inside role="img" has no name.
        let (_, picture) = bar.split_once("<div class=\"bar\"").expect("bar");
        let picture = picture.split("</div>").next().unwrap_or("");
        assert!(picture.contains("role=\"img\""), "{house}");
        assert!(
            !picture.contains("<a "),
            "segments must not be links on {house}"
        );
        assert_eq!(bar.matches("aria-label=\"").count(), 1, "{house}");

        // The key links every party holding a seat in the chamber, once each,
        // with no separator glyph to strand at the start of a wrapped line.
        let seated = data
            .parties
            .iter()
            .filter(|p| p.seats.as_ref().is_some_and(|s| s.get(house) > 0))
            .count();
        assert!(seated >= 2, "expected several parties in {house}");
        let (_, key) = bar.split_once("<div class=\"key\">").expect("key");
        assert_eq!(
            key.matches("<a href=\"/parties/").count(),
            seated,
            "{house}"
        );
        assert_eq!(picture.matches("<span ").count(), seated, "{house}");
        assert!(!key.contains('\u{b7}'), "{house}");
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
fn leadership_roles_are_the_offices_themselves() {
    for role in [
        "Prime Minister",
        "Deputy Prime Minister",
        "Leader of the Opposition",
        "Deputy Leader of the Opposition in the Senate",
        "Leader of the House",
        "Manager of Opposition Business in the Senate",
        "President of the Senate",
        "Deputy President and Chairman of Committees",
        "Speaker of the House of Representatives",
        "Deputy Speaker",
        "Chief Government Whip",
        "Opposition Whip in the Senate",
    ] {
        assert!(pages::is_leadership_role(role), "{role} should be listed");
    }
    // Junior posts named after an office are not the office.
    for role in [
        "Assistant Minister to the Prime Minister",
        "Minister Assisting the Prime Minister for the Public Service",
        "Parliamentary Secretary to the Prime Minister",
        "Cabinet Minister",
        "Shadow Minister for Examples",
    ] {
        assert!(
            !pages::is_leadership_role(role),
            "{role} should be left out"
        );
    }
}

#[test]
fn a_leadership_row_with_no_recorded_start_says_so() {
    let mut data = sample_data();
    let speaker = data
        .people
        .iter_mut()
        .find(|p| p.slug == "jordan-nguyen")
        .expect("sample speaker");
    for position in speaker.positions.iter_mut().flatten() {
        position.from = None;
    }
    let party = data
        .parties
        .iter()
        .find(|p| p.slug == "placeholder-alliance")
        .expect("sample party");
    let html = render(&data, &pages::party_page(&data, party));
    let (_, section) = html
        .split_once("Parliamentary leadership")
        .expect("leadership table");
    let table = section.split("</table>").next().unwrap_or("");
    assert!(table.contains(
        "<td class=\"num zero\"><span aria-hidden=\"true\">\u{2014}</span><span class=\"visually-hidden\">Not recorded</span></td>"
    ));
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
fn occupation_tables_show_only_the_columns_their_rows_fill() {
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
    let table = |person: &pollywiki_schema::Person| -> String {
        let html = render(&data, &pages::person_page(&data, person));
        html.split("<h3>Occupations before parliament</h3>")
            .nth(1)
            .and_then(|s| s.split("</table>").next())
            .expect("occupations table")
            .to_string()
    };

    // Dated placements fill all three columns; a bare trade sits in the role
    // column with the others empty, never spanning them.
    let full = table(person);
    assert!(full.contains("<th scope=\"col\">Organisation</th>"));
    assert!(full.contains("<th class=\"num\" scope=\"col\">Period</th>"));
    assert!(full.contains("<td>CEO</td><td>Sample Business Network</td>"));
    assert!(full.contains("<td>Policy Analyst</td><td>Example Treasury</td>"));
    assert!(full.contains("<td>Grazier and small business owner</td><td></td>"));
    assert!(!full.contains("colspan"));

    // A career of titles alone gets a one-column table: "Head of Partnerships"
    // used to render as the role "Head" at the organisation "Partnerships",
    // above rows with two empty cells.
    let mut titles_only = person.clone();
    let background = titles_only.background.as_mut().expect("background");
    background.occupations = vec![
        "Head of Partnerships".to_string(),
        "Senior Manager".to_string(),
        "National Sales Manager".to_string(),
    ];
    let bare = table(&titles_only);
    assert!(bare.contains("<th scope=\"col\">Role</th></tr>"));
    assert!(!bare.contains("Organisation"));
    assert!(!bare.contains("Period"));
    assert!(bare.contains("<tr><td>Head of Partnerships</td></tr>"));
    assert!(bare.contains("<tr><td>Senior Manager</td></tr>"));
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
fn every_other_page_asks_for_the_large_image_preview() {
    let data = sample_data();
    for page in all_pages(&data) {
        let html = render(&data, &page);
        let where_ = &page.path;
        let expected = page.robots.unwrap_or(layout::INDEXED_ROBOTS);
        assert!(
            html.contains(&format!("<meta name=\"robots\" content=\"{expected}\">")),
            "robots wrong on {where_}"
        );
        // Exactly one directive, so a page cannot say both things at once.
        assert_eq!(html.matches("<meta name=\"robots\"").count(), 1, "{where_}");
        if page.robots.is_none() {
            assert!(
                html.contains("max-image-preview:large"),
                "an indexable page wants its share card in the result: {where_}"
            );
        }
    }
}

#[test]
fn every_page_says_what_its_share_card_shows() {
    let data = sample_data();
    for page in all_pages(&data) {
        let html = render(&data, &page);
        let where_ = &page.path;
        // A page on the site-wide card describes the site; one with a card of
        // its own names itself, which is what the card actually renders.
        let expected = match &page.og_image {
            None => layout::DEFAULT_OG_IMAGE_ALT.to_string(),
            Some(_) => format!("pollywiki share card: {}", page.title),
        };
        assert!(
            html.contains(&format!(
                "<meta property=\"og:image:alt\" content=\"{}\">",
                crate::html::esc_attr(&expected)
            )),
            "og:image:alt wrong on {where_}"
        );
    }

    // A division carries its own card, so its alt names the division.
    let division = data.divisions.first().expect("a sample division");
    let mut page = pages::division_page(&data, division);
    page.og_image = Some(og::card_path(division));
    assert!(render(&data, &page).contains(&format!(
        "og:image:alt\" content=\"pollywiki share card: {}\"",
        crate::html::esc_attr(&division.name)
    )));
}

#[test]
fn the_analytics_origin_is_opened_early() {
    let data = sample_data();
    let html = render(&data, &pages::home(&data));
    let preconnect =
        "<link rel=\"preconnect\" href=\"https://www.googletagmanager.com\" crossorigin>";
    assert!(html.contains(preconnect));
    // It has to come before the tag that uses it, or it buys nothing.
    assert!(
        html.find(preconnect) < html.find("googletagmanager.com/gtag/js"),
        "the preconnect must precede the script"
    );
}

#[test]
fn index_pages_declare_what_they_collect() {
    let data = sample_data();
    let cases: Vec<(Page, &str, usize)> = vec![
        (pages::people_index(&data), "People", data.sitting().count()),
        (
            pages::divisions_index(&data),
            "Divisions",
            data.divisions.len(),
        ),
        (pages::bills_index(&data), "Bills", data.bills.len()),
        (
            pages::electorates_index(&data),
            "Electorates",
            data.electorates.len(),
        ),
        (pages::parties_index(&data), "Parties", data.parties.len()),
    ];
    for (page, name, count) in cases {
        let where_ = page.path.clone();
        let value = jsonld(&render(&data, &page)).unwrap_or_else(|| panic!("json-ld on {where_}"));
        assert_eq!(
            types(&value),
            vec!["CollectionPage", "BreadcrumbList"],
            "{where_}"
        );
        let collection = nodes(&value)[0];
        assert_eq!(collection["name"], name, "{where_}");
        assert_eq!(collection["url"], format!("{SITE_URL}{where_}"), "{where_}");
        assert_eq!(collection["inLanguage"], "en-AU", "{where_}");
        assert_eq!(collection["isPartOf"]["url"], format!("{SITE_URL}/"));
        assert_eq!(
            collection["mainEntity"]["numberOfItems"], count,
            "the count must be what the page lists on {where_}"
        );
        assert!(count > 0, "the sample bundles should fill {where_}");

        // Two steps home from an index, and the last one is the page itself.
        let trail = nodes(&value)[1]["itemListElement"]
            .as_array()
            .expect("trail")
            .clone();
        assert_eq!(trail.len(), 2, "{where_}");
        assert_eq!(trail[1]["item"], format!("{SITE_URL}{where_}"), "{where_}");
    }
}

#[test]
fn the_about_pages_are_typed_and_sit_under_about() {
    let data = sample_data();
    let leaves = [
        pages::data_sources(&data),
        pages::methodology(&data),
        pages::corrections(&data),
    ];
    for page in leaves {
        let where_ = page.path.clone();
        let value = jsonld(&render(&data, &page)).unwrap_or_else(|| panic!("json-ld on {where_}"));
        assert_eq!(
            types(&value),
            vec!["AboutPage", "BreadcrumbList"],
            "{where_}"
        );
        let trail = nodes(&value)[1]["itemListElement"]
            .as_array()
            .expect("trail")
            .clone();
        assert_eq!(trail.len(), 3, "a leaf sits below /about/ on {where_}");
        assert_eq!(trail[1]["item"], format!("{SITE_URL}/about/"), "{where_}");
        assert_eq!(trail[2]["item"], format!("{SITE_URL}{where_}"), "{where_}");
    }

    // The section front page is one step from home.
    let index = jsonld(&render(&data, &pages::about_index(&data))).expect("json-ld");
    assert_eq!(types(&index), vec!["AboutPage", "BreadcrumbList"]);
    assert_eq!(nodes(&index)[0]["url"], format!("{SITE_URL}/about/"));
}

#[test]
fn the_home_page_declares_the_site_its_language_and_its_search() {
    let data = sample_data();
    let value = jsonld(&render(&data, &pages::home(&data))).expect("json-ld");
    assert_eq!(types(&value), vec!["WebSite"]);
    assert_eq!(value["url"], format!("{SITE_URL}/"));
    assert_eq!(value["inLanguage"], "en-AU");
    assert_eq!(
        value["potentialAction"]["target"]["urlTemplate"],
        format!("{SITE_URL}/search/?q={{search_term_string}}")
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
                // A bare yield can spin through every attempt before the
                // server thread has bound its port.
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
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
fn every_page_opens_with_a_masthead_or_a_record_head() {
    let data = sample_data();
    for page in all_pages(&data) {
        let html = render(&data, &page);
        let (_, main) = html
            .split_once("<main class=\"wrap\" id=\"main\">")
            .expect("main");
        let top = main
            .strip_prefix("<article data-pagefind-body>")
            .unwrap_or(main);
        // The page-top space lives in these three openers; a bare h1 would
        // sit against the header rule.
        assert!(
            [
                "<div class=\"masthead\"><h1",
                "<div class=\"profile-head\">",
                "<div class=\"meta-row\">"
            ]
            .iter()
            .any(|opener| top.starts_with(opener)),
            "{} opens with {}",
            page.path,
            &top[..top.len().min(60)]
        );
    }
}

#[test]
fn inline_styles_carry_only_values_from_the_data() {
    let data = sample_data();
    for page in all_pages(&data) {
        let html = render(&data, &page);
        // Layout belongs in the stylesheet, where print, the dark scheme and
        // forced colours can reach it. What stays inline is per-record: a
        // party's colour, a segment's share, the majority tick's position.
        for value in html.split(" style=\"").skip(1) {
            let value = value.split('"').next().unwrap_or("");
            for declaration in value.split(';').filter(|d| !d.is_empty()) {
                let property = declaration.split(':').next().unwrap_or("").trim();
                assert!(
                    ["background", "width", "left"].contains(&property),
                    "fixed inline style {value:?} on {}",
                    page.path
                );
            }
        }
    }
}

#[test]
fn party_chips_link_wherever_the_party_has_a_page() {
    let data = sample_data();

    // The party page names the party in its h1 chip, which is not a link to
    // itself, and the seat line spaces its separator.
    let party = data
        .parties
        .iter()
        .find(|p| p.slug == "example-party")
        .expect("sample party");
    let html = render(&data, &pages::party_page(&data, party));
    assert!(html.contains(
        "<h1 data-pagefind-meta=\"title\"><span class=\"group-chip\"><span class=\"dot\""
    ));
    let motto = html
        .split("<p class=\"motto\">")
        .nth(1)
        .and_then(|m| m.split("</p>").next())
        .expect("motto");
    assert!(
        motto.contains(" House seat") && motto.contains(" \u{b7} "),
        "{motto}"
    );

    // The parties index and every division's By party table link each group
    // that has a page.
    let index = render(&data, &pages::parties_index(&data));
    for p in &data.parties {
        assert!(
            index.contains(&format!(
                "<a class=\"group-chip\" href=\"/parties/{}/\">",
                p.slug
            )),
            "{} unlinked on the index",
            p.slug
        );
    }
    let mut linked = 0;
    for division in &data.divisions {
        let html = render(&data, &pages::division_page(&data, division));
        let (_, by_party) = html.split_once("<h2>By party</h2>").expect("By party");
        let table = by_party.split("</table>").next().unwrap_or("");
        linked += table
            .matches("<a class=\"group-chip\" href=\"/parties/")
            .count();
    }
    assert!(linked > 0, "By party rows should link to their party pages");
}

#[test]
fn card_portraits_are_decorative_and_the_profile_portrait_is_described() {
    let data = sample_data();
    let person = data
        .people
        .iter()
        .find(|p| p.photo.is_some())
        .expect("sample data has a portrait");
    // On a card the name follows the portrait inside the same link, so a
    // described image would read the name twice.
    let index = render(&data, &pages::people_index(&data));
    let card = index
        .split(&format!(
            "<a class=\"person-card\" href=\"/people/{}/\">",
            person.slug
        ))
        .nth(1)
        .and_then(|c| c.split("</a>").next())
        .expect("card");
    assert!(card.contains("<img class=\"avatar\""), "{card}");
    assert!(card.contains("alt=\"\""), "{card}");
    assert!(!index.contains("alt=\"Portrait of"));

    let profile = render(&data, &pages::person_page(&data, person));
    assert!(profile.contains(&format!("alt=\"Portrait of {}\"", person.name)));
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
    // A failed sync is marked stale rather than quietly shown as current, and
    // says so in words, not by the colour of its mark alone.
    assert!(html.contains("class=\"stale\">AEC profiles · 1 Aug 2026 · sync failed</span>"));
    assert!(html.contains("class=\"ok\""));
    assert_eq!(html.matches("sync failed").count(), 1);
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

/// Records outlive the people and the seats they name: a member leaves, a seat
/// is abolished at a redistribution, a replacement is sworn in before the
/// people source has caught up. Every one of those used to leave pages linking
/// at a URL the build never wrote.
#[test]
fn no_internal_link_points_at_a_page_the_build_does_not_emit() {
    let data = sample_data();
    let pages = all_pages(&data);
    let emitted: HashSet<&str> = pages.iter().map(|p| p.path.as_str()).collect();

    let href = regex::Regex::new("href=\"(/[^\"#?]*)\"").expect("literal pattern");
    let mut broken: Vec<String> = Vec::new();
    for page in &pages {
        let html = render(&data, page);
        for capture in href.captures_iter(&html) {
            let target = &capture[1];
            // Assets, feeds and the search index are files, not pages.
            if !target.ends_with('/') || emitted.contains(target) {
                continue;
            }
            broken.push(format!("{} -> {target}", page.path));
        }
    }
    broken.sort();
    broken.dedup();
    assert!(broken.is_empty(), "links into nothing: {broken:#?}");
}

#[test]
fn a_former_member_keeps_a_page_and_stays_out_of_the_sitting_counts() {
    let data = sample_data();
    let former = data
        .people
        .iter()
        .find(|p| p.is_former())
        .expect("sample bundles carry a member who has left");

    let html = render(&data, &pages::person_page(&data, former));
    assert!(html.contains("<strong>Former member:</strong>"));
    assert!(html.contains("Former Member for Oldbridge"));
    assert!(html.contains("21 May 2022 to 14 Mar 2026"));
    // The seat was abolished, so the contest is named but not linked.
    assert!(html.contains("<td>Oldbridge</td>"));
    assert!(!html.contains("/electorates/oldbridge/"));
    // Years served stop at the end of the term instead of running on.
    assert!(html.contains("<span class=\"n\">3.8</span>"));

    let index = render(&data, &pages::people_index(&data));
    assert!(index.contains("<h2>Former members</h2>"));
    assert!(index.contains(&format!(
        "{} sitting parliamentarians",
        data.sitting().count()
    )));
    assert!(
        !index.contains(&format!("{} sitting parliamentarians", data.people.len())),
        "the lede counts seats held, not pages published"
    );

    // Nothing that describes the parliament as it stands includes them.
    for party in &data.parties {
        assert!(
            !data
                .members_of_party(&party.slug)
                .iter()
                .any(|p| p.is_former()),
            "a former member must not fill a seat on {}",
            party.slug
        );
    }
}

#[test]
fn a_vote_names_its_member_even_with_no_page_to_link_to() {
    let data = sample_data();
    let division = data
        .divisions
        .iter()
        .find(|d| {
            d.votes
                .iter()
                .any(|v| data.person_by_slug(&v.person_slug).is_none())
        })
        .expect("sample bundles carry a voter the people bundle does not");
    let stray = division
        .votes
        .iter()
        .find(|v| data.person_by_slug(&v.person_slug).is_none())
        .expect("the vote the division was chosen for");

    let html = render(&data, &pages::division_page(&data, division));
    assert!(
        html.contains(&format!("<li>{}</li>", stray.name)),
        "the name on the record should render as plain text"
    );
    assert!(
        !html.contains(&format!("/people/{}/", stray.person_slug)),
        "no link into a page the build does not write"
    );
    assert!(
        !html.contains(&stray.person_slug.replace('-', " ")),
        "the slug fallback used to leak lowercase names into the vote list"
    );
}

/// The Every vote section of a rendered division page, up to the list's end.
fn every_vote(html: &str) -> &str {
    let from = html.find("<h2>Every vote</h2>").expect("every vote");
    let rest = &html[from..];
    let to = rest.find("</div></div>").expect("vote columns end");
    &rest[..to]
}

/// Each column's groups as (label, names), in page order.
fn vote_groups(column: &str) -> Vec<(String, Vec<String>)> {
    column
        .split("<li class=\"vote-group\">")
        .skip(1)
        .map(|group| {
            let label = group
                .split("aria-hidden=\"true\"></span>")
                .nth(1)
                .and_then(|s| s.split("</span></span>").next())
                .expect("group label")
                .replace("<span class=\"n\">", "");
            let names = group
                .split("<ul>")
                .nth(1)
                .expect("a group's own list")
                .split("<li>")
                .skip(1)
                .map(|li| {
                    let li = li.split("</li>").next().unwrap_or(li);
                    let li = li.split("<span class=\"note\">").next().unwrap_or(li);
                    match li.split_once("\">") {
                        Some((_, linked)) => linked.trim_end_matches("</a>").to_string(),
                        None => li.to_string(),
                    }
                })
                .collect();
            (label, names)
        })
        .collect()
}

#[test]
fn every_vote_groups_names_as_the_by_party_table_counts_them() {
    let data = sample_data();
    for division in &data.divisions {
        let html = render(&data, &pages::division_page(&data, division));
        let section = every_vote(&html);
        let breakdown = data.group_breakdown(division);
        let columns: Vec<&str> = section.split("<h3 class=\"col-head\">").skip(1).collect();
        assert_eq!(columns.len(), 2, "{}", division.id);
        for (column, aye) in columns.iter().zip([true, false]) {
            // The groups follow the table's order and carry its counts.
            let expected: Vec<String> = breakdown
                .iter()
                .map(|row| {
                    let label = row
                        .party
                        .map(|p| p.code.as_deref().unwrap_or(&p.name))
                        .unwrap_or(&row.group);
                    (label, if aye { row.aye } else { row.no })
                })
                .filter(|(_, n)| *n > 0)
                .map(|(label, n)| format!("{label} {n}"))
                .collect();
            let groups = vote_groups(column);
            let labels: Vec<String> = groups.iter().map(|(label, _)| label.clone()).collect();
            assert_eq!(labels, expected, "{}", division.id);
            for (label, names) in &groups {
                assert_eq!(
                    label.rsplit(' ').next(),
                    Some(names.len().to_string().as_str()),
                    "{label} on {}",
                    division.id
                );
            }
        }
    }

    // The sample's widest division: a crossed vote, a group with no party
    // page and a voter with no page at all, each under its own label.
    let division = data
        .divisions
        .iter()
        .find(|d| d.id == "representatives/2025-07-30/1")
        .expect("sample division");
    let html = render(&data, &pages::division_page(&data, division));
    let section = every_vote(&html);
    let (ayes, noes) = section
        .split_once("<h3 class=\"col-head\">NO")
        .expect("two columns");
    assert_eq!(
        vote_groups(ayes),
        vec![
            ("PLA 1".to_string(), vec!["Jordan Nguyen".to_string()]),
            (
                "Retired Party 1".to_string(),
                vec!["Casey O&#39;Brien".to_string()]
            ),
        ]
    );
    assert_eq!(
        vote_groups(noes),
        vec![
            ("EXP 1".to_string(), vec!["Alex Paterson".to_string()]),
            ("Unknown 1".to_string(), vec!["Chris Newcomer".to_string()]),
        ]
    );
    // A label heads its group's own list; it is never an item among the names.
    assert!(!section.contains("</li><li class=\"vote-group\"><a"));
    assert!(section.contains("<li><a href=\"/people/alex-paterson/\">Alex Paterson</a><span class=\"note\"> · crossed</span></li>"));
}

#[test]
fn every_vote_orders_groups_by_size_and_names_as_the_people_index_does() {
    let data = sample_data();
    let mut division = data
        .divisions
        .iter()
        .find(|d| d.id == "representatives/2025-07-30/1")
        .expect("sample division")
        .clone();
    let vote = |slug: &str, name: &str| -> pollywiki_schema::VoteCast {
        serde_json::from_value(serde_json::json!({
            "personSlug": slug, "name": name, "vote": "aye"
        }))
        .expect("vote fixture")
    };
    // In source order the smaller group comes first and every group's names
    // run backwards.
    division.votes = vec![
        vote("jordan-nguyen", "Jordan Nguyen"),
        vote("zed-arrival", "Zed Arrival"),
        vote("sam-kelly", "Sam Kelly"),
        vote("abe-arrival", "Abe Arrival"),
        vote("alex-paterson", "Alex Paterson"),
        vote("mia-arrival", "Mia Arrival"),
    ];
    division.ayes = 6;
    division.noes = 0;
    let html = render(&data, &pages::division_page(&data, &division));
    let section = every_vote(&html);
    let ayes = section
        .split("<h3 class=\"col-head\">NO")
        .next()
        .expect("aye column");
    assert_eq!(
        vote_groups(ayes),
        vec![
            (
                "Unknown 3".to_string(),
                vec![
                    "Abe Arrival".to_string(),
                    "Mia Arrival".to_string(),
                    "Zed Arrival".to_string()
                ]
            ),
            (
                "EXP 2".to_string(),
                vec!["Alex Paterson".to_string(), "Sam Kelly".to_string()]
            ),
            ("PLA 1".to_string(), vec!["Jordan Nguyen".to_string()]),
        ]
    );
    // The By party table above lists the groups in the same order.
    let table = html.split("<h2>By party</h2>").nth(1).expect("by party");
    let unknown = table.find(">Unknown</span>").expect("unknown row");
    let exp = table.find(">Example Party</a>").expect("party row");
    let pla = table.find(">Placeholder Alliance</a>").expect("party row");
    assert!(unknown < exp && exp < pla);
}

#[test]
fn the_crossed_note_sits_beside_the_list_that_uses_it() {
    let data = sample_data();
    for division in &data.divisions {
        let page = pages::division_page(&data, division);
        let html = render(&data, &page);
        let crossed = division
            .votes
            .iter()
            .any(|v| v.against_group_majority == Some(true));
        assert_eq!(
            html.contains("<h2>Every vote</h2><p class=\"note\">\u{201c}Crossed\u{201d} marks a vote against the majority of the member's own party in this division.</p><div class=\"vote-columns\">"),
            crossed,
            "{}",
            page.path
        );
        assert_eq!(
            html.matches("Crossed\u{201d} marks").count(),
            usize::from(crossed)
        );
        if let Some(note) = &page.footer_note {
            assert!(!note.contains("rossed"), "{}", page.path);
            assert!(!note.ends_with(" </p>"), "{}", page.path);
        }
    }
}

#[test]
fn a_sitting_day_lists_every_division_with_this_one_marked() {
    let data = sample_data();
    let division = data
        .divisions
        .iter()
        .find(|d| d.id == "senate/2025-08-05/4")
        .expect("sample division");
    let html = render(&data, &pages::division_page(&data, division));
    let day = html
        .split("<h2>Divisions this sitting day</h2>")
        .nth(1)
        .and_then(|s| s.split("</ul>").next())
        .expect("sitting day");
    let whens: Vec<&str> = day
        .split("<span class=\"when\">")
        .skip(1)
        .map(|s| s.split("</span>").next().expect("when end"))
        .collect();
    assert_eq!(whens, ["Division 3", "Division 4", "Division 5"]);
    // This division is in its place, marked current and not linked to itself;
    // the others link to their pages.
    assert_eq!(day.matches("aria-current=\"true\"").count(), 1);
    assert!(day.contains(&format!(
        "<li data-house=\"senate\" aria-current=\"true\"><span class=\"when\">Division 4</span><span class=\"what\">{}</span>",
        division.name
    )));
    assert!(!day.contains("/divisions/senate/2025-08-05-4/"));
    assert!(day.contains("/divisions/senate/2025-08-05-3/"));
    assert!(day.contains("/divisions/senate/2025-08-05-5/"));

    // A division alone on its day has no list to place it in.
    let alone = data
        .divisions
        .iter()
        .find(|d| d.id == "representatives/2025-07-30/1")
        .expect("sample division");
    assert_eq!(data.sitting_day(alone).len(), 1);
    let html = render(&data, &pages::division_page(&data, alone));
    assert!(!html.contains("this sitting day"));
}

#[test]
fn table_wrappers_are_named_regions_unique_on_their_page() {
    let data = sample_data();
    let mut wrapped = 0;
    for page in all_pages(&data) {
        let html = render(&data, &page);
        let mut labels: Vec<&str> = Vec::new();
        for rest in html.split("class=\"table-scroll\"").skip(1) {
            let label = rest
                .strip_prefix(" role=\"region\" aria-label=\"")
                .and_then(|s| s.split('"').next())
                .unwrap_or_else(|| panic!("an unnamed table wrapper on {}", page.path));
            assert!(!label.is_empty(), "{}", page.path);
            assert!(
                !labels.contains(&label),
                "two regions named {label} on {}",
                page.path
            );
            labels.push(label);
        }
        wrapped += labels.len();
        // The script that gives an overflowing wrapper its tab stop ships
        // only where there is a wrapper to give one to.
        assert_eq!(
            html.contains("new ResizeObserver"),
            !labels.is_empty(),
            "{}",
            page.path
        );
    }
    assert!(wrapped > 20, "the sample pages hold tables");
}

#[test]
fn a_profile_links_its_record_and_its_sections() {
    let data = sample_data();
    let person = data
        .people
        .iter()
        .find(|p| p.slug == "alex-paterson")
        .expect("sample member");
    let html = render(&data, &pages::person_page(&data, person));

    // The divisions figure opens the record it counts.
    assert!(html.contains("<span class=\"n\"><a href=\"#voting-record\">2 / 2</a></span>"));
    assert!(html.contains(
        "<h2 id=\"voting-record\">Voting record <span class=\"count\">3 divisions</span></h2>"
    ));

    // The contents line follows the figures' caveat and lists every section
    // in page order, each pointing at a heading below it.
    let nav = html.find("<nav class=\"on-page\"").expect("on-page nav");
    assert!(
        html.find("How these figures are computed.")
            .expect("caveat")
            < nav
    );
    let links = html[nav..].split("</nav>").next().expect("nav end");
    let targets: Vec<&str> = links
        .split("<a href=\"#")
        .skip(1)
        .map(|s| s.split('"').next().expect("target"))
        .collect();
    assert_eq!(
        targets,
        [
            "background",
            "positions",
            "elections",
            "bills-raised",
            "voting-record"
        ]
    );
    let mut last = nav;
    for id in &targets {
        let at = html
            .find(&format!("<h2 id=\"{id}\">"))
            .unwrap_or_else(|| panic!("no heading for #{id}"));
        assert!(at > last, "#{id} is out of page order");
        last = at;
    }

    // The record runs under month headers, one row group per month.
    let record = &html[html
        .find("<table class=\"vote-record\">")
        .expect("vote record")..];
    let record = record.split("</table>").next().expect("record end");
    assert_eq!(record.matches("<tbody>").count(), 2);
    assert!(record.contains(
        "<tbody><tr class=\"month\"><th colspan=\"4\" scope=\"rowgroup\">August 2025</th></tr>"
    ));
    assert!(record.contains(
        "<tbody><tr class=\"month\"><th colspan=\"4\" scope=\"rowgroup\">July 2025</th></tr>"
    ));

    // A profile that is only its record needs no contents line.
    let record_only = data
        .people
        .iter()
        .find(|p| p.slug == "sam-kelly")
        .expect("sample senator");
    let html = render(&data, &pages::person_page(&data, record_only));
    assert!(html.contains("<h2 id=\"voting-record\">"));
    assert!(!html.contains("class=\"on-page\""));
}

#[test]
fn positions_list_each_office_and_term_once() {
    let data = sample_data();
    let mut person = data
        .people
        .iter()
        .find(|p| p.slug == "alex-paterson")
        .expect("sample member")
        .clone();
    person.positions = Some(
        serde_json::from_value(serde_json::json!([
            {"role": "Shadow Minister for Examples", "ministry": "Old Shadow Ministry", "kind": "shadow", "from": "2019-06-01", "to": "2022-05-23"},
            {"role": "Manager of Opposition Business", "ministry": "Old Shadow Ministry", "kind": "shadow", "from": "2019-06-01", "to": "2022-05-23"},
            {"role": "Cabinet Minister", "ministry": "First Ministry", "kind": "ministry", "from": "2013-09-18", "to": "2019-05-29"},
            {"role": "Cabinet Minister", "ministry": "Second Ministry", "kind": "ministry", "from": "2013-09-18", "to": "2019-05-29"},
            {"role": "Cabinet Minister", "ministry": "Second Ministry", "kind": "ministry", "from": "2013-09-18", "to": "2019-05-29"},
            {"role": "Cabinet Minister", "ministry": "Third Ministry", "kind": "ministry", "from": "2019-05-29", "to": "2022-05-23"},
            {"role": "Member of the Speaker's Panel", "kind": "position", "from": "2022-07-26"}
        ]))
        .expect("positions fixture"),
    );
    let html = render(&data, &pages::person_page(&data, &person));
    let table = html
        .split("<h2 id=\"positions\">Positions held</h2>")
        .nth(1)
        .and_then(|s| s.split("</table>").next())
        .expect("positions table");
    let rows: Vec<&str> = table.split("<tr>").skip(2).collect();
    assert_eq!(rows.len(), 5, "one row per office and term");
    // The office still held leads; the rest keep the Handbook's order.
    assert!(rows[0].starts_with("<td>Member of the Speaker&#39;s Panel</td><td></td>"));
    assert!(rows[0].contains("<td class=\"num\">current</td>"));
    // One term under two ministries is one row naming both, each once; a new
    // term under a third ministry is a row of its own.
    assert!(table.contains(
        "<td>Cabinet Minister</td><td>First Ministry; Second Ministry</td><td class=\"num\">18 Sep 2013</td>"
    ));
    assert!(table.contains("<td>Cabinet Minister</td><td>Third Ministry</td>"));
    // A shadow office says so once, in its title or in the label.
    assert!(table.contains("<td>Shadow Minister for Examples</td>"));
    assert!(!table.contains("(shadow) (shadow)") && !table.contains("Examples (shadow)"));
    assert!(table.contains("<td>Manager of Opposition Business (shadow)</td>"));
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
