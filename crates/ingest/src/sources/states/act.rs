//! The ACT. The Legislative Assembly's members page is one table: each
//! member's name, any office under it, their electorate and their party.
//! The site asks crawlers to wait 20 seconds between requests, and the one
//! page is all this reads.

use super::{plain_name, RollChamber, RollMember};
use crate::endpoints::Endpoints;
use crate::http::fetch_text;
use crate::store::Store;
use anyhow::Result;
use regex::Regex;
use std::sync::LazyLock;

/// A cell of the members table, by the column its headers attribute names.
fn cell(row: &str, column: u8) -> Option<&str> {
    let marker = format!("c{column}\">");
    let start = row.find(&marker)? + marker.len();
    let end = row[start..].find("</td>")?;
    Some(&row[start..start + end])
}

pub async fn read(store: &Store, endpoints: &Endpoints) -> Result<Vec<RollChamber>> {
    static LINK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?s)<a href="([^"]+)"[^>]*>(.*?)</a>"#).unwrap());
    static FAMILY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)<(?:strong|b)>(.*?)</(?:strong|b)>").unwrap());
    static OFFICE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?s)<span style="color: grey">(.*?)</span>"#).unwrap());

    let source = format!("{}/members/current", endpoints.act_parliament);
    let page = fetch_text(&source, &endpoints.opts(20_000)).await?;
    store
        .put_raw("raw/states/act/members-current.html", page.as_bytes())
        .await?;

    let body = page.split("<tbody>").nth(1).unwrap_or("");
    let mut members = Vec::new();
    for row in body.split("<tr>").skip(1) {
        let (Some(member), Some(electorate), Some(party)) =
            (cell(row, 1), cell(row, 2), cell(row, 3))
        else {
            continue;
        };
        let Some(link) = LINK.captures(member) else {
            continue;
        };
        // The family name is set in bold; what comes before it is the name
        // the member goes by. Post-nominals after it are dropped.
        let label = text(&link[2]);
        let name = match FAMILY.captures(&link[2]) {
            Some(family) => {
                let given = text(&link[2][..family.get(0).map_or(0, |m| m.start())]);
                plain_name(&format!("{given} {}", text(&family[1])))
            }
            None => plain_name(&label),
        };
        members.push(RollMember {
            name,
            // The party follows a coloured dot.
            party: text(party)
                .trim_start_matches('\u{2b24}')
                .trim()
                .to_string(),
            seat: Some(text(electorate)).filter(|s| !s.is_empty()),
            since: None,
            offices: OFFICE
                .captures_iter(member)
                .map(|o| text(&o[1]))
                .filter(|o| !o.is_empty())
                .collect(),
            profile: Some(link[1].to_string()),
        });
    }
    Ok(vec![RollChamber {
        slug: "legislative-assembly".to_string(),
        source,
        members,
    }])
}

/// A cell's words, without its tags and with entities and spacing tidied.
/// A tag can fall inside a word (the dot's span has swallowed the "I" of
/// "Independent" before now), so tags go without leaving a space.
fn text(fragment: &str) -> String {
    static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]*>").unwrap());
    let plain = TAG
        .replace_all(fragment, "")
        .replace("&nbsp;", " ")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    plain.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::LocalStore;
    use crate::test_http::{Response, TestServer};

    const PAGE: &str = r#"<table id="t1"><thead><tr><th id="t1r1c1">Member</th></tr></thead><tbody>
      <tr><td headers="t1r1c1"><a href="https://example.org/members/current/barr"><img src="x.png" alt="" />Andrew <strong>Barr</strong></a><br /><span style="color: grey">Chief Minister</span></td><td headers="t1r1c2">
         Kurrajong 
      </td><td headers="t1r1c3"><span style="color: red">⬤</span>&nbsp;Labor 
      </td><td headers="t1r1c4">contact</td></tr>
      <tr><td headers="t1r1c1"><a href="https://example.org/members/current/hanson"><img src="y.png" alt="" />Jeremy <strong>Hanson</strong> CSC</a><br /><span style="color: grey">Speaker</span></td><td headers="t1r1c2">Murrumbidgee</td><td headers="t1r1c3"><span style="color: blue">⬤</span>&nbsp;Liberals</td><td headers="t1r1c4"></td></tr>
      <tr><td headers="t1r1c1"><a href="https://example.org/members/current/vassarotti">Rebecca <b>Vassarotti</b></a></td><td headers="t1r1c2">Kurrajong</td><td headers="t1r1c3"><span style="color: green">⬤</span>&nbsp;Green</td><td headers="t1r1c4"></td></tr>
      <tr><td headers="t1r1c1"><a href="https://example.org/members/current/castley">Leanne <strong>Castley</strong></a></td><td headers="t1r1c2">Yerrabi</td><td headers="t1r1c3"><span style="color: grey">⬤ I</span>ndependent </td><td headers="t1r1c4"></td></tr>
    </tbody></table>"#;

    #[tokio::test]
    async fn the_members_table_gives_name_seat_party_and_office() {
        let server = TestServer::start(|req| match req.path.as_str() {
            "/act/members/current" => Response::html(PAGE),
            _ => Response::status(404, "unexpected path"),
        });
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/states-tests/act");
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::Local(LocalStore::new(dir));

        let chambers = read(&store, &Endpoints::at(&server.base))
            .await
            .expect("read");
        let members = &chambers[0].members;
        let rows: Vec<(&str, &str, Option<&str>)> = members
            .iter()
            .map(|m| (m.name.as_str(), m.party.as_str(), m.seat.as_deref()))
            .collect();
        assert_eq!(
            rows,
            [
                ("Andrew Barr", "Labor", Some("Kurrajong")),
                ("Jeremy Hanson", "Liberals", Some("Murrumbidgee")),
                ("Rebecca Vassarotti", "Green", Some("Kurrajong")),
                ("Leanne Castley", "Independent", Some("Yerrabi"))
            ],
            "post-nominals off, either bold tag for the family name"
        );
        assert_eq!(members[0].offices, ["Chief Minister"]);
        assert_eq!(members[1].offices, ["Speaker"]);
        assert!(members[2].offices.is_empty());
        assert_eq!(
            members[0].profile.as_deref(),
            Some("https://example.org/members/current/barr")
        );
    }
}
