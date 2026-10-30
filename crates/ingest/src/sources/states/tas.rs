//! Tasmania. Each house's member listing links to a profile per member,
//! which gives the party and the division in one line ("Liberal member for
//! Braddon") and any office the Parliament records there. Where a profile
//! names no party, the member's entry in the Parliament's biographical
//! database does.

use super::{plain_name, RollChamber, RollMember};
use crate::endpoints::Endpoints;
use crate::http::fetch_text;
use crate::store::Store;
use anyhow::{anyhow, Result};
use regex::Regex;
use std::sync::LazyLock;

/// A listing's member cards: the profile link around the styled name.
static CARD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<a[^>]*href="([^"]+)"[^>]*>\s*<h2[^>]*>(.*?)</h2>"#).unwrap()
});

/// "Liberal member for <a ...>Braddon</a>"; the party can be missing.
static SEAT_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<p>\s*([^<]*?)\s*member for\s*<a[^>]*>(.*?)</a>").unwrap());

/// Offices, where a profile lists any: <li><strong>Premier</strong>.
static OFFICE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<li>\s*<strong>(.*?)</strong>").unwrap());

static BIO_LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"<a href="([^"]+/historyindex/members/[^"]+)">Member Bio Page</a>"#).unwrap()
});

static BIO_PARTY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<strong>Party:</strong>\s*([^<]*)</p>").unwrap());

pub async fn read(store: &Store, endpoints: &Endpoints) -> Result<Vec<RollChamber>> {
    let mut chambers = Vec::new();
    for (path, slug) in [
        ("house-of-assembly/currentmembers", "house-of-assembly"),
        ("legislative-council/members", "legislative-council"),
    ] {
        let source = format!("{}/{path}", endpoints.tas_parliament);
        let listing = fetch_text(&source, &endpoints.opts(1500)).await?;
        store
            .put_raw(&format!("raw/states/tas/{slug}.html"), listing.as_bytes())
            .await?;
        // The page's menus link every member too; only the cards in its
        // body carry a heading, and each member once.
        let mut cards: Vec<(String, String)> = Vec::new();
        for card in CARD.captures_iter(&listing) {
            let profile = card[1].to_string();
            if profile.contains(&format!("/{path}/")) && !cards.iter().any(|(p, _)| *p == profile) {
                cards.push((profile, plain_name(&html_text(&card[2]))));
            }
        }
        let mut members = Vec::new();
        for (profile, name) in cards {
            let page = fetch_text(&profile, &endpoints.opts(1500)).await?;
            let line = SEAT_LINE
                .captures(&page)
                .ok_or_else(|| anyhow!("no \"member for\" line on {profile}"))?;
            let mut party = html_text(&line[1]);
            if party.is_empty() {
                party = bio_party(&page, endpoints).await?;
            }
            members.push(RollMember {
                name,
                party,
                seat: Some(html_text(&line[2])).filter(|s| !s.is_empty()),
                since: None,
                offices: OFFICE
                    .captures_iter(&page)
                    .map(|o| html_text(&o[1]))
                    .filter(|o| !o.is_empty())
                    .collect(),
                profile: Some(profile),
            });
        }
        chambers.push(RollChamber {
            slug: slug.to_string(),
            source,
            members,
        });
    }
    Ok(chambers)
}

/// The party the biographical database gives for a member whose profile
/// names none. It lists every party a member has sat for, the current one
/// last: "Tasmanian Greens (2024 - 2026), Independent (2026 - )".
async fn bio_party(profile: &str, endpoints: &Endpoints) -> Result<String> {
    let link = BIO_LINK
        .captures(profile)
        .ok_or_else(|| anyhow!("a profile with no party and no biography"))?;
    let bio = fetch_text(&link[1], &endpoints.opts(1500)).await?;
    let listed = BIO_PARTY
        .captures(&bio)
        .map(|p| html_text(&p[1]))
        .unwrap_or_default();
    let party = current_party(&listed);
    if party.is_empty() {
        return Err(anyhow!("no party for the member at {}", &link[1]));
    }
    Ok(party)
}

/// The last party in a biography's list, without its years or numbering.
fn current_party(listed: &str) -> String {
    static NOISE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\(\d+\)|\(?\b\d{4}\s*-\s*(?:\d{4})?\s*\)?|[()]").unwrap());
    let last = listed.rsplit(',').next().unwrap_or("");
    let last = NOISE.replace_all(last, " ");
    last.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Text inside a tag: entities decoded, tags and runs of space collapsed.
fn html_text(fragment: &str) -> String {
    static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]*>").unwrap());
    let text = TAG
        .replace_all(fragment, " ")
        .replace("&nbsp;", " ")
        .replace("&#39;", "'")
        .replace("&rsquo;", "\u{2019}")
        .replace("&amp;", "&");
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::LocalStore;
    use crate::test_http::{Response, TestServer};

    #[test]
    fn a_biography_s_current_party_is_its_last() {
        assert_eq!(current_party("Independent"), "Independent");
        assert_eq!(
            current_party("Tasmanian Greens (2024 - 2026), Independent (2026 - )"),
            "Independent"
        );
        assert_eq!(
            current_party("Australian Labor Party 2010 - 2022, Independent 2022 -"),
            "Independent"
        );
        assert_eq!(
            current_party(
                "(1) Australian Labor Party (2) Independent (2019 - 2021), Liberal (2021 - )"
            ),
            "Liberal"
        );
        assert_eq!(current_party(""), "");
    }

    #[tokio::test]
    async fn listings_lead_to_profiles_and_a_partyless_one_to_its_biography() {
        let server = TestServer::start(|req| {
            let base = req
                .header("host")
                .map(|h| format!("http://{h}"))
                .unwrap_or_default();
            let card = |path: &str, name: &str| {
                format!(
                    r#"<div class="card"><a href="{base}/tas/{path}"><h2>{name}</h2></a></div>"#
                )
            };
            let menu = |path: &str, name: &str| {
                format!(r#"<li><a href="{base}/tas/{path}" class="menu">{name}</a></li>"#)
            };
            let body = match req.path.as_str() {
                "/tas/house-of-assembly/currentmembers" => format!(
                    "{}{}{}",
                    menu(
                        "house-of-assembly/currentmembers/eric-abetz",
                        "The Honourable Eric Abetz"
                    ),
                    card(
                        "house-of-assembly/currentmembers/eric-abetz",
                        "The Honourable Eric Abetz"
                    ),
                    card(
                        "house-of-assembly/currentmembers/tabatha-badger",
                        "Tabatha Badger"
                    ),
                ),
                "/tas/house-of-assembly/currentmembers/eric-abetz" => {
                    r#"<h1>The Honourable Eric Abetz MP</h1>
                    <p>Liberal member for <a href="x#Franklin">Franklin</a></p>
                    <ul><li><strong>Premier</strong></li><li><strong>Treasurer</strong></li></ul>"#
                        .to_string()
                }
                "/tas/house-of-assembly/currentmembers/tabatha-badger" => r#"<p>
                    Green member for <a href="x#Lyons">Lyons</a></p>"#
                    .to_string(),
                "/tas/legislative-council/members" => card(
                    "legislative-council/members/casey-hiscutt",
                    "The Honourable Casey Hiscutt",
                ),
                "/tas/legislative-council/members/casey-hiscutt" => format!(
                    r#"<p>member for <a href="x#Montgomery">Montgomery</a></p>
                       <p><a href="{base}/tas/visit-and-learn/historyindex/members/casey-hiscutt">Member Bio Page</a></p>"#
                ),
                "/tas/visit-and-learn/historyindex/members/casey-hiscutt" => {
                    "<p><strong>Party:</strong> Independent</p>".to_string()
                }
                _ => return Response::status(404, "unexpected path"),
            };
            Response::html(body)
        });
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/states-tests/tas");
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::Local(LocalStore::new(dir));

        let chambers = read(&store, &Endpoints::at(&server.base))
            .await
            .expect("read");
        let assembly = &chambers[0].members;
        assert_eq!(
            assembly.len(),
            2,
            "a member linked from the menu is still one member"
        );
        assert_eq!(assembly[0].name, "Eric Abetz");
        assert_eq!(assembly[0].party, "Liberal");
        assert_eq!(assembly[0].seat.as_deref(), Some("Franklin"));
        assert_eq!(assembly[0].offices, ["Premier", "Treasurer"]);
        assert_eq!(assembly[1].party, "Green");
        assert!(assembly[1].offices.is_empty());

        let council = &chambers[1].members;
        assert_eq!(council[0].name, "Casey Hiscutt");
        assert_eq!(council[0].party, "Independent", "from the biography");
        assert_eq!(council[0].seat.as_deref(), Some("Montgomery"));
    }
}
