//! Atom feeds for the two things that change: divisions as they are recorded,
//! and bills as they move. Newest 50 each, built from the same bundles the
//! pages read.

use crate::data::{division_key, SiteData};
use crate::html::{esc, esc_attr};
use crate::pages::latest_step;
use anyhow::Result;
use pollywiki_schema::House;
use std::path::Path;

const LIMIT: usize = 50;

/// Atom wants a timestamp; the record only ever carries a date.
fn as_timestamp(date: &str) -> String {
    format!("{date}T00:00:00Z")
}

fn chamber_word(house: House) -> &'static str {
    match house {
        House::Senate => "Senate",
        House::Representatives => "House",
    }
}

struct Entry {
    title: String,
    path: String,
    updated: String,
    summary: String,
}

fn feed(origin: &str, title: &str, index_path: &str, entries: &[Entry]) -> String {
    let self_href = format!("{origin}{index_path}feed.xml");
    // An empty feed still needs an updated stamp; fall back to the newest entry.
    let updated = entries
        .first()
        .map(|e| e.updated.clone())
        .unwrap_or_else(|| as_timestamp("1970-01-01"));

    let mut out = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?><feed xmlns=\"http://www.w3.org/2005/Atom\"><title>");
    out.push_str(&esc(title));
    out.push_str("</title><link rel=\"self\" href=\"");
    out.push_str(&esc_attr(&self_href));
    out.push_str("\"/><link href=\"");
    out.push_str(&esc_attr(&format!("{origin}{index_path}")));
    out.push_str("\"/><id>");
    out.push_str(&esc(&self_href));
    out.push_str("</id><updated>");
    out.push_str(&updated);
    out.push_str("</updated><author><name>pollywiki</name></author>");
    for entry in entries {
        let url = format!("{origin}{}", entry.path);
        out.push_str("<entry><title>");
        out.push_str(&esc(&entry.title));
        out.push_str("</title><link href=\"");
        out.push_str(&esc_attr(&url));
        out.push_str("\"/><id>");
        out.push_str(&esc(&url));
        out.push_str("</id><updated>");
        out.push_str(&entry.updated);
        out.push_str("</updated><summary>");
        out.push_str(&esc(&entry.summary));
        out.push_str("</summary></entry>");
    }
    out.push_str("</feed>");
    out
}

fn write(out_dir: &Path, index_path: &str, body: &str) -> Result<()> {
    let dir = out_dir.join(index_path.trim_start_matches('/'));
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("feed.xml"), body)?;
    Ok(())
}

pub fn write_feeds(out_dir: &Path, site_url: &str, data: &SiteData) -> Result<()> {
    let origin = site_url.trim_end_matches('/');

    // Divisions already arrive newest first from the derive step.
    let divisions: Vec<Entry> = data
        .divisions
        .iter()
        .take(LIMIT)
        .map(|d| Entry {
            title: d.name.clone(),
            path: format!("/divisions/{}/{}/", d.house, division_key(d)),
            updated: as_timestamp(&d.date),
            summary: format!(
                "{} {}\u{2013}{} \u{b7} {}",
                chamber_word(d.house),
                d.ayes,
                d.noes,
                crate::components::result_word(d.result)
            ),
        })
        .collect();
    write(
        out_dir,
        "/divisions/",
        &feed(origin, "pollywiki: divisions", "/divisions/", &divisions),
    )?;

    // Bills are alphabetical in the bundle, so sort by their newest step.
    let mut moved: Vec<(&pollywiki_schema::Bill, &pollywiki_schema::TimelineStep)> = data
        .bills
        .iter()
        .filter_map(|b| latest_step(b).map(|s| (b, s)))
        .collect();
    moved.sort_by(|a, b| b.1.date.cmp(&a.1.date));
    let bills: Vec<Entry> = moved
        .into_iter()
        .take(LIMIT)
        .map(|(bill, step)| Entry {
            title: format!("{} \u{2014} {}", bill.title, step.event),
            path: format!("/bills/{}/", bill.id),
            updated: as_timestamp(&step.date),
            summary: format!("{} \u{b7} {}", step.event, bill.status),
        })
        .collect();
    write(
        out_dir,
        "/bills/",
        &feed(origin, "pollywiki: bills", "/bills/", &bills),
    )?;
    Ok(())
}
