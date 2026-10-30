//! South Australia. The Parliament's public contact API lists each house's
//! members with their current party and, in the House of Assembly, their
//! district; its members app adds the names they go by, the day each began
//! their current service, and the one office it lists them in. The two are
//! joined on the Parliament's own member id.

use super::{RollChamber, RollMember};
use crate::endpoints::Endpoints;
use crate::http::fetch_json;
use crate::store::Store;
use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

const PROFILE: &str = "https://www.parliament.sa.gov.au/Search/Member?type=member&id=";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContactFeed {
    member_contacts: Vec<Contact>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Contact {
    pm_id: i64,
    first_name: String,
    last_name: String,
    preferred_name: Option<String>,
    electorate_name: Option<String>,
    political_party_name: Option<String>,
}

#[derive(Deserialize)]
struct AppMember {
    #[serde(rename = "pm_Id")]
    id: i64,
    #[serde(rename = "pm_FirstName")]
    first_name: Option<String>,
    #[serde(rename = "pm_Position")]
    position: Option<String>,
    #[serde(rename = "mb_ElectedDate")]
    elected: Option<String>,
}

pub async fn read(store: &Store, endpoints: &Endpoints) -> Result<Vec<RollChamber>> {
    let opts = endpoints.opts(1000);
    let assembly_url = format!("{}/HAMembersDetails", endpoints.sa_contacts);
    let council_url = format!("{}/LCMembersDetails", endpoints.sa_contacts);
    let assembly: Value = fetch_json(&assembly_url, &opts).await?;
    let council: Value = fetch_json(&council_url, &opts).await?;
    let mut post = endpoints.opts(1000);
    post.post_json = Some(r#"{"memberType":"current"}"#.to_string());
    let app: Value = fetch_json(&format!("{}/members", endpoints.sa_members), &post).await?;
    store
        .put_json("raw/states/sa/ha-contacts.json", &assembly)
        .await?;
    store
        .put_json("raw/states/sa/lc-contacts.json", &council)
        .await?;
    store.put_json("raw/states/sa/members.json", &app).await?;

    let app: Vec<AppMember> = serde_json::from_value(app)?;
    let app: HashMap<i64, AppMember> = app.into_iter().map(|m| (m.id, m)).collect();
    Ok(vec![
        chamber(
            "house-of-assembly",
            &assembly_url,
            serde_json::from_value(assembly)?,
            &app,
        ),
        chamber(
            "legislative-council",
            &council_url,
            serde_json::from_value(council)?,
            &app,
        ),
    ])
}

fn chamber(
    slug: &str,
    source: &str,
    feed: ContactFeed,
    app: &HashMap<i64, AppMember>,
) -> RollChamber {
    let members = feed
        .member_contacts
        .into_iter()
        .map(|c| {
            let listed = app.get(&c.pm_id);
            // The app has the name a member goes by; the contact feed only
            // for some members of the Assembly, and otherwise the legal one.
            let first = listed
                .and_then(|m| m.first_name.as_deref())
                .or(c.preferred_name.as_deref())
                .unwrap_or(&c.first_name);
            RollMember {
                name: format!("{} {}", first.trim(), c.last_name.trim()),
                party: c
                    .political_party_name
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or("")
                    .to_string(),
                seat: c
                    .electorate_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
                since: listed.and_then(|m| m.elected.as_deref()).and_then(app_date),
                offices: listed
                    .and_then(|m| m.position.as_deref())
                    .map(str::trim)
                    .filter(|p| !p.is_empty() && *p != "Member")
                    .map(|p| vec![p.to_string()])
                    .unwrap_or_default(),
                profile: Some(format!("{PROFILE}{}", c.pm_id)),
            }
        })
        .collect();
    RollChamber {
        slug: slug.to_string(),
        source: source.to_string(),
        members,
    }
}

/// The app's dates read "Mar 17 2018 12:00AM", a single-digit day padded
/// with a second space ("Jul  2 2022").
fn app_date(text: &str) -> Option<String> {
    let words: Vec<&str> = text.split_whitespace().take(3).collect();
    chrono::NaiveDate::parse_from_str(&words.join(" "), "%b %d %Y")
        .ok()
        .map(|d| d.format("%Y-%m-%d").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::LocalStore;
    use crate::test_http::{Response, TestServer};

    #[test]
    fn app_dates_read_with_either_padding() {
        assert_eq!(
            app_date("Mar 17 2018 12:00AM").as_deref(),
            Some("2018-03-17")
        );
        assert_eq!(
            app_date("Jul  2 2022 12:00AM").as_deref(),
            Some("2022-07-02")
        );
        assert_eq!(app_date("not a date"), None);
    }

    #[tokio::test]
    async fn both_houses_are_read_and_joined_on_the_member_id() {
        let server = TestServer::start(|req| {
            if req.path == "/sa-contacts/HAMembersDetails" {
                return Response::json(
                    r#"{"parliamentNumber":56,"memberContacts":[
                      {"pmId":1,"title":"Hon","firstName":"Natalie","lastName":"Example",
                       "preferredName":null,"positions":[],"electorateName":"Sampleford",
                       "politicalPartyName":"ALP"},
                      {"pmId":2,"title":"Mr","firstName":"Jordan","lastName":"Nguyen",
                       "preferredName":"Jordy","positions":[],"electorateName":"Placeholder Bay",
                       "politicalPartyName":"Independent"}]}"#,
                );
            }
            if req.path == "/sa-contacts/LCMembersDetails" {
                return Response::json(
                    r#"{"parliamentNumber":56,"memberContacts":[
                      {"pmId":3,"title":"Hon","firstName":"Casey ","lastName":"Doe",
                       "positions":[],"politicalPartyName":"Greens SA"}]}"#,
                );
            }
            if req.path == "/sa-members/members" && req.method == "POST" {
                assert!(
                    req.body.contains(r#""memberType":"current""#),
                    "{}",
                    req.body
                );
                return Response::json(
                    r#"[{"pm_Id":1,"pm_FirstName":"Nat","pm_Position":"Premier",
                         "mb_ElectedDate":"Mar 17 2018 12:00AM","electorate":"Unknown"},
                        {"pm_Id":3,"pm_FirstName":"Casey ","pm_Position":"Member",
                         "mb_ElectedDate":"Jul  2 2022 12:00AM"}]"#,
                );
            }
            Response::status(404, "unexpected path")
        });
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/states-tests/sa");
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::Local(LocalStore::new(dir));

        let chambers = read(&store, &Endpoints::at(&server.base))
            .await
            .expect("read");
        let slugs: Vec<&str> = chambers.iter().map(|c| c.slug.as_str()).collect();
        assert_eq!(slugs, ["house-of-assembly", "legislative-council"]);

        let premier = &chambers[0].members[0];
        assert_eq!(
            premier.name, "Nat Example",
            "the name the app says they go by"
        );
        assert_eq!(premier.party, "ALP");
        assert_eq!(premier.seat.as_deref(), Some("Sampleford"));
        assert_eq!(premier.since.as_deref(), Some("2018-03-17"));
        assert_eq!(premier.offices, ["Premier"]);
        assert_eq!(
            premier.profile.as_deref(),
            Some("https://www.parliament.sa.gov.au/Search/Member?type=member&id=1")
        );

        // Missing from the app: the contact feed's preferred name stands in,
        // and nothing is invented for a start date or an office.
        let independent = &chambers[0].members[1];
        assert_eq!(independent.name, "Jordy Nguyen");
        assert!(independent.since.is_none() && independent.offices.is_empty());

        // A councillor is elected state-wide, and "Member" is no office.
        let councillor = &chambers[1].members[0];
        assert_eq!(councillor.name, "Casey Doe", "stray spaces trimmed");
        assert!(councillor.seat.is_none());
        assert!(councillor.offices.is_empty());
        assert_eq!(councillor.since.as_deref(), Some("2022-07-02"));
        assert!(chambers[1]
            .source
            .ends_with("/sa-contacts/LCMembersDetails"));

        // What was read is kept for replay.
        assert!(store
            .get_json::<Value>("raw/states/sa/members.json")
            .await
            .unwrap()
            .is_some());
    }
}
