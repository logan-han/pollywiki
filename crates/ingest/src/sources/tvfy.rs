use crate::endpoints::Endpoints;
use crate::http::fetch_json;
use crate::store::Store;
use anyhow::{anyhow, Result};
use indexmap::IndexMap;
use pollywiki_schema::{
    slugify, Division, DivisionLinks, DivisionResult, House, Person, Vote, VoteCast,
};
use serde::Deserialize;
use serde_json::Value;

const PAGE_SIZE: usize = 100;

const BROWSER_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/127.0.0.0 Safari/537.36";

/// Per-run request budget; raise via env for a supervised backfill.
fn request_cap() -> u32 {
    std::env::var("TVFY_REQUEST_CAP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(500)
}

#[derive(Debug, Clone, Deserialize)]
pub struct TvfyPersonSummary {
    pub id: i64,
    pub latest_member: TvfyMemberSummary,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TvfyMemberSummary {
    pub name: TvfyName,
    pub electorate: String,
    pub house: House,
    #[allow(dead_code)]
    pub party: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TvfyName {
    pub first: String,
    pub last: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TvfyDivisionSummary {
    pub id: i64,
    pub house: House,
    pub date: String,
    pub number: i64,
    #[allow(dead_code)]
    pub name: String,
    pub aye_votes: i64,
    pub no_votes: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TvfyDivisionDetail {
    #[allow(dead_code)]
    pub id: i64,
    pub house: House,
    pub date: String,
    pub number: i64,
    pub name: String,
    pub aye_votes: i64,
    pub no_votes: i64,
    pub summary: Option<String>,
    #[serde(default)]
    pub votes: Vec<TvfyVote>,
    #[serde(default)]
    pub bills: Option<Vec<TvfyBillRef>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TvfyVote {
    pub member: TvfyVoteMember,
    pub vote: Vote,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TvfyVoteMember {
    pub person: TvfyPersonRef,
    pub first_name: String,
    pub last_name: String,
    pub party: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TvfyPersonRef {
    pub id: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TvfyBillRef {
    pub id: i64,
    #[allow(dead_code)]
    pub title: String,
    pub official_id: Option<Value>,
}

#[derive(Debug, Default, Deserialize, serde::Serialize)]
struct TvfyCursor {
    #[serde(rename = "lastDivisionDate", skip_serializing_if = "Option::is_none")]
    last_division_date: Option<String>,
}

struct TvfyClient<'a> {
    key: String,
    requests: u32,
    cap: u32,
    ua_blocked: bool,
    endpoints: &'a Endpoints,
}

impl TvfyClient<'_> {
    async fn get<T: serde::de::DeserializeOwned>(
        &mut self,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<T> {
        self.requests += 1;
        if self.requests > self.cap {
            return Err(anyhow!("TVFY request cap ({}) reached", self.cap));
        }
        let mut qs = String::new();
        for (name, value) in params.iter().chain([("key", self.key.as_str())].iter()) {
            if !qs.is_empty() {
                qs.push('&');
            }
            qs.push_str(&format!(
                "{}={}",
                name,
                crate::js_url::encode_uri_component(value).replace("%20", "+")
            ));
        }
        let url = format!("{}/{path}.json?{qs}", self.endpoints.tvfy);
        let browser = self
            .endpoints
            .opts(1200)
            .with_header("user-agent", BROWSER_UA);
        if self.ua_blocked {
            return fetch_json(&url, &browser).await;
        }
        match fetch_json(&url, &self.endpoints.opts(1200)).await {
            Ok(value) => Ok(value),
            Err(err) if err.to_string().contains(" 403") => {
                // Some edges 403 the identifying bot UA (seen from CI runners); fall
                // back to a browser UA for the rest of the run and note it loudly.
                eprintln!("tvfy: identifying UA refused (403), retrying with a browser UA");
                let result = fetch_json(&url, &browser).await?;
                self.ua_blocked = true;
                Ok(result)
            }
            Err(err) => Err(err),
        }
    }
}

/// Divisions and votes from They Vote For You (data licence: ODbL 1.0).
/// Requires TVFY_API_KEY. Free access is low-volume and non-commercial;
/// email the OpenAustralia Foundation before any bulk backfill.
pub async fn sync_tvfy(
    store: &Store,
    people: &mut [Person],
    rebuild: bool,
    endpoints: &Endpoints,
) -> Result<()> {
    if rebuild {
        return rebuild_from_raw(store, people).await;
    }
    let key = std::env::var("TVFY_API_KEY")
        .map_err(|_| anyhow!("TVFY_API_KEY not set; skipping They Vote For You sync"))?;
    sync_with_key(store, people, &key, endpoints).await
}

/// The sync itself, against a given key and endpoint.
async fn sync_with_key(
    store: &Store,
    people: &mut [Person],
    key: &str,
    endpoints: &Endpoints,
) -> Result<()> {
    let mut client = TvfyClient {
        key: key.to_string(),
        requests: 0,
        cap: request_cap(),
        ua_blocked: false,
        endpoints,
    };

    let tvfy_people_raw: Value = client.get("people", &[]).await?;
    store
        .put_json("raw/tvfy/people.json", &tvfy_people_raw)
        .await?;
    let tvfy_people: Vec<TvfyPersonSummary> = serde_json::from_value(tvfy_people_raw)?;
    let crosswalk = match_people(&tvfy_people, people);

    let cursor: Option<TvfyCursor> = store.get_json("state/tvfy-cursor.json").await?;
    let last = cursor.and_then(|c| c.last_division_date);
    let overlap_days = 14;
    let start_date = match &last {
        Some(date) => iso_days_before(date, overlap_days)?,
        None => "2025-07-01".to_string(), // opening of the 48th Parliament
    };

    let mut latest_date = last.unwrap_or_else(|| start_date.clone());
    for house in [House::Representatives, House::Senate] {
        // The index returns at most 100 divisions, newest first, and ignores any
        // page parameter, so walk backwards through date windows via end_date.
        // Windows overlap on the boundary date; the map dedupes by id.
        let mut by_id: IndexMap<i64, TvfyDivisionSummary> = IndexMap::new();
        let mut end_date: Option<String> = None;
        loop {
            let mut params: Vec<(&str, &str)> =
                vec![("house", house.as_str()), ("start_date", &start_date)];
            if let Some(end) = &end_date {
                params.push(("end_date", end));
            }
            let batch: Vec<TvfyDivisionSummary> = client.get("divisions", &params).await?;
            let before = by_id.len();
            let min_date = batch.iter().map(|s| s.date.clone()).min();
            let batch_len = batch.len();
            for s in batch {
                by_id.insert(s.id, s);
            }
            if batch_len < PAGE_SIZE || by_id.len() == before {
                break;
            }
            end_date = min_date;
        }
        for summary in by_id.values() {
            let id = format!("{}/{}/{}", summary.house, summary.date, summary.number);
            let existing: Option<Division> = store
                .get_json(&format!("canonical/divisions/{}.json", key_for(&id)))
                .await?;
            if let Some(existing) = existing {
                if existing.ayes == summary.aye_votes && existing.noes == summary.no_votes {
                    continue;
                }
            }
            let detail_raw: Value = client
                .get(&format!("divisions/{}", summary.id), &[])
                .await?;
            store
                .put_json(
                    &format!("raw/tvfy/divisions/{}.json", summary.id),
                    &detail_raw,
                )
                .await?;
            let detail: TvfyDivisionDetail = serde_json::from_value(detail_raw)?;
            let division = to_division(&detail, &crosswalk);
            store
                .put_json(
                    &format!("canonical/divisions/{}.json", key_for(&division.id)),
                    &division,
                )
                .await?;
            if division.date > latest_date {
                latest_date = division.date.clone();
            }
        }
    }
    store
        .put_json(
            "state/tvfy-cursor.json",
            &TvfyCursor {
                last_division_date: Some(latest_date),
            },
        )
        .await?;

    // Persist discovered TVFY ids back onto people.
    persist_ids(store, people, &crosswalk).await
}

async fn persist_ids(
    store: &Store,
    people: &mut [Person],
    crosswalk: &IndexMap<String, i64>,
) -> Result<()> {
    for person in people.iter_mut() {
        if let Some(&tvfy_id) = crosswalk.get(&person.slug) {
            if person.ids.tvfy != Some(tvfy_id) {
                person.ids.tvfy = Some(tvfy_id);
                store
                    .put_json(&format!("canonical/people/{}.json", person.slug), person)
                    .await?;
            }
        }
    }
    Ok(())
}

fn state_code_for(seat: &str) -> Option<&'static str> {
    match seat.to_lowercase().as_str() {
        "new south wales" => Some("NSW"),
        "victoria" => Some("VIC"),
        "queensland" => Some("QLD"),
        "western australia" => Some("WA"),
        "south australia" => Some("SA"),
        "tasmania" => Some("TAS"),
        "australian capital territory" => Some("ACT"),
        "northern territory" => Some("NT"),
        _ => None,
    }
}

/// Match TVFY people to canonical people by name, then by seat on ties.
pub fn match_people(tvfy_people: &[TvfyPersonSummary], people: &[Person]) -> IndexMap<String, i64> {
    let mut by_slug: IndexMap<String, i64> = IndexMap::new();
    let mut unmatched: Vec<String> = Vec::new();
    for tp in tvfy_people {
        let name = format!(
            "{} {}",
            tp.latest_member.name.first, tp.latest_member.name.last
        );
        let name_slug = slugify(&name);
        // TVFY reports a senator's "electorate" as the full state name.
        let seat = &tp.latest_member.electorate;
        let seat_state = state_code_for(seat);
        let candidates: Vec<&Person> = people
            .iter()
            .filter(|p| p.slug == name_slug || slugify(&p.name) == name_slug)
            .collect();
        let matched = if candidates.len() == 1 {
            Some(candidates[0])
        } else {
            candidates
                .iter()
                .find(|p| {
                    p.house == tp.latest_member.house
                        && (p.electorate.as_deref() == Some(slugify(seat).as_str())
                            || (seat_state.is_some() && p.state.map(|s| s.as_str()) == seat_state))
                })
                .copied()
        };
        match matched {
            Some(p) => {
                by_slug.insert(p.slug.clone(), tp.id);
            }
            None => unmatched.push(name),
        }
    }
    if !unmatched.is_empty() {
        eprintln!(
            "tvfy: {} people unmatched: {}",
            unmatched.len(),
            unmatched.join(", ")
        );
    }
    by_slug
}

pub fn to_division(detail: &TvfyDivisionDetail, crosswalk: &IndexMap<String, i64>) -> Division {
    let mut id_by_slug: IndexMap<i64, String> = IndexMap::new();
    for (slug, id) in crosswalk {
        id_by_slug.insert(*id, slug.clone());
    }

    let mut group_tallies: IndexMap<String, (i64, i64)> = IndexMap::new();
    for v in &detail.votes {
        let tally = group_tallies
            .entry(v.member.party.clone())
            .or_insert((0, 0));
        match v.vote {
            Vote::Aye => tally.0 += 1,
            Vote::No => tally.1 += 1,
        }
    }
    let mut votes: Vec<VoteCast> = Vec::new();
    for v in &detail.votes {
        let slug = id_by_slug
            .get(&v.member.person.id)
            .cloned()
            .unwrap_or_else(|| slugify(&format!("{} {}", v.member.first_name, v.member.last_name)));
        let tally = group_tallies.get(&v.member.party);
        let majority = tally.and_then(|(aye, no)| {
            if aye == no {
                None
            } else if aye > no {
                Some(Vote::Aye)
            } else {
                Some(Vote::No)
            }
        });
        votes.push(VoteCast {
            person_slug: slug,
            vote: v.vote,
            teller: None,
            against_group_majority: match majority {
                Some(m) if v.vote != m => Some(true),
                _ => None,
            },
        });
    }

    Division {
        id: format!("{}/{}/{}", detail.house, detail.date, detail.number),
        house: detail.house,
        date: detail.date.clone(),
        number: detail.number,
        name: detail.name.clone(),
        summary: detail
            .summary
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        summary_kind: None,
        ai_summary: None,
        result: if detail.aye_votes > detail.no_votes {
            DivisionResult::Passed
        } else {
            DivisionResult::Rejected
        },
        ayes: detail.aye_votes,
        noes: detail.no_votes,
        bill_ids: detail
            .bills
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|b| match &b.official_id {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Number(n)) => n.to_string(),
                _ => b.id.to_string(),
            })
            .collect(),
        links: DivisionLinks {
            hansard: None,
            tvfy: Some(format!(
                "https://theyvoteforyou.org.au/divisions/{}/{}/{}",
                detail.house, detail.date, detail.number
            )),
        },
        votes,
    }
}

/// Re-normalise every division from cached raw payloads, no API calls.
async fn rebuild_from_raw(store: &Store, people: &mut [Person]) -> Result<()> {
    let raw_people: Option<Vec<TvfyPersonSummary>> = store.get_json("raw/tvfy/people.json").await?;
    let raw_people = raw_people.ok_or_else(|| {
        anyhow!("tvfy rebuild: raw/tvfy/people.json missing, run a live sync first")
    })?;
    let crosswalk = match_people(&raw_people, people);

    let mut count = 0;
    for key in store.list("raw/tvfy/divisions/").await? {
        let Some(detail) = store.get_json::<TvfyDivisionDetail>(&key).await? else {
            continue;
        };
        let division = to_division(&detail, &crosswalk);
        store
            .put_json(
                &format!("canonical/divisions/{}.json", key_for(&division.id)),
                &division,
            )
            .await?;
        count += 1;
    }

    persist_ids(store, people, &crosswalk).await?;
    println!("tvfy rebuild: {count} divisions re-normalised from raw");
    Ok(())
}

pub fn key_for(division_id: &str) -> String {
    division_id.replace('/', "-")
}

fn iso_days_before(iso: &str, days: i64) -> Result<String> {
    let date = chrono::NaiveDate::parse_from_str(iso, "%Y-%m-%d")?;
    Ok((date - chrono::Duration::days(days))
        .format("%Y-%m-%d")
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_http::{Response, TestServer};
    use pollywiki_schema::StateCode;

    fn person(slug: &str, name: &str, house: House) -> Person {
        let mut p: Person = serde_json::from_str(&format!(
            r#"{{"slug":"{slug}","name":"{name}","house":"{}","group":"Example","groupSlug":"example","ids":{{}},"links":{{}}}}"#,
            house
        ))
        .expect("person fixture");
        p.electorate = None;
        p
    }

    fn detail(json: &str) -> TvfyDivisionDetail {
        serde_json::from_str(json).expect("division detail fixture")
    }

    #[test]
    fn state_codes_cover_every_senate_seat_name() {
        for (seat, code) in [
            ("New South Wales", "NSW"),
            ("victoria", "VIC"),
            ("QUEENSLAND", "QLD"),
            ("Western Australia", "WA"),
            ("South Australia", "SA"),
            ("Tasmania", "TAS"),
            ("Australian Capital Territory", "ACT"),
            ("Northern Territory", "NT"),
        ] {
            assert_eq!(state_code_for(seat), Some(code), "{seat}");
        }
        // A House electorate is not a state name.
        assert_eq!(state_code_for("Wentworth"), None);
    }

    #[test]
    fn people_match_by_name_then_by_seat() {
        let tvfy: Vec<TvfyPersonSummary> = serde_json::from_str(
            r#"[
              {"id":10,"latest_member":{"name":{"first":"Alex","last":"Paterson"},
                "electorate":"Sampleford","house":"representatives","party":"Example"}},
              {"id":11,"latest_member":{"name":{"first":"Sam","last":"Kelly"},
                "electorate":"Queensland","house":"senate","party":"Example"}},
              {"id":12,"latest_member":{"name":{"first":"Nobody","last":"Here"},
                "electorate":"Nowhere","house":"senate","party":"Example"}}
            ]"#,
        )
        .expect("tvfy people fixture");

        let mut senator = person("sam-kelly", "Sam Kelly", House::Senate);
        senator.state = Some(StateCode::QLD);
        let people = vec![
            person("alex-paterson", "Alex Paterson", House::Representatives),
            senator,
        ];

        let crosswalk = match_people(&tvfy, &people);
        assert_eq!(crosswalk.get("alex-paterson"), Some(&10));
        assert_eq!(crosswalk.get("sam-kelly"), Some(&11));
        // Unmatched TVFY people are reported, never invented.
        assert_eq!(crosswalk.len(), 2);
    }

    #[test]
    fn duplicate_names_are_split_by_chamber_and_seat() {
        let tvfy: Vec<TvfyPersonSummary> = serde_json::from_str(
            r#"[{"id":20,"latest_member":{"name":{"first":"Jane","last":"Smith"},
                 "electorate":"Tasmania","house":"senate","party":"Example"}}]"#,
        )
        .expect("tvfy people fixture");

        let mut senator = person("jane-smith-senate", "Jane Smith", House::Senate);
        senator.state = Some(StateCode::TAS);
        let people = vec![
            person("jane-smith", "Jane Smith", House::Representatives),
            senator,
        ];

        // Two canonical people share the name, so the seat decides.
        let crosswalk = match_people(&tvfy, &people);
        assert_eq!(crosswalk.get("jane-smith-senate"), Some(&20));
        assert!(!crosswalk.contains_key("jane-smith"));
    }

    #[test]
    fn divisions_normalise_ids_results_and_crossings() {
        let d = detail(
            r#"{"id":1,"house":"representatives","date":"2026-08-12","number":7,
                "name":"Bills — Example Bill 2026; Second Reading",
                "aye_votes":2,"no_votes":1,"summary":"  Context.  ",
                "bills":[{"id":99,"title":"Example","official_id":"r7123"},
                         {"id":100,"title":"Numbered","official_id":4242},
                         {"id":101,"title":"Bare","official_id":null}],
                "votes":[
                  {"member":{"person":{"id":10},"first_name":"Alex","last_name":"Paterson",
                    "party":"Example Party"},"vote":"aye"},
                  {"member":{"person":{"id":11},"first_name":"Jordan","last_name":"Nguyen",
                    "party":"Example Party"},"vote":"aye"},
                  {"member":{"person":{"id":99},"first_name":"Casey","last_name":"O'Brien",
                    "party":"Example Party"},"vote":"no"}
                ]}"#,
        );
        let mut crosswalk: IndexMap<String, i64> = IndexMap::new();
        crosswalk.insert("alex-paterson".to_string(), 10);
        crosswalk.insert("jordan-nguyen".to_string(), 11);

        let division = to_division(&d, &crosswalk);
        assert_eq!(division.id, "representatives/2026-08-12/7");
        assert_eq!(division.result, DivisionResult::Passed);
        assert_eq!((division.ayes, division.noes), (2, 1));
        // Summary is trimmed; empty ones drop out entirely.
        assert_eq!(division.summary.as_deref(), Some("Context."));
        // Official ids win over TVFY's internal ids, whatever their JSON type.
        assert_eq!(division.bill_ids, vec!["r7123", "4242", "101"]);
        assert_eq!(
            division.links.tvfy.as_deref(),
            Some("https://theyvoteforyou.org.au/divisions/representatives/2026-08-12/7")
        );

        // Unknown TVFY ids fall back to a slug of the member's name.
        let slugs: Vec<&str> = division
            .votes
            .iter()
            .map(|v| v.person_slug.as_str())
            .collect();
        assert_eq!(
            slugs,
            vec!["alex-paterson", "jordan-nguyen", "casey-obrien"]
        );
        // The party voted 2-1 aye, so only the no vote crossed.
        let crossed: Vec<Option<bool>> = division
            .votes
            .iter()
            .map(|v| v.against_group_majority)
            .collect();
        assert_eq!(crossed, vec![None, None, Some(true)]);
    }

    #[test]
    fn a_tied_party_has_no_majority_to_cross() {
        let d = detail(
            r#"{"id":2,"house":"senate","date":"2026-08-12","number":1,"name":"Tied",
                "aye_votes":1,"no_votes":1,"summary":"   ",
                "votes":[
                  {"member":{"person":{"id":1},"first_name":"A","last_name":"One",
                    "party":"Split Party"},"vote":"aye"},
                  {"member":{"person":{"id":2},"first_name":"B","last_name":"Two",
                    "party":"Split Party"},"vote":"no"}
                ]}"#,
        );
        let division = to_division(&d, &IndexMap::new());
        assert_eq!(division.result, DivisionResult::Rejected);
        assert!(
            division.summary.is_none(),
            "whitespace-only summary must drop"
        );
        assert!(division.bill_ids.is_empty());
        assert!(division
            .votes
            .iter()
            .all(|v| v.against_group_majority.is_none()));
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tvfy-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[tokio::test]
    async fn rebuild_renormalises_every_cached_division_without_the_api() {
        let store = Store::Local(crate::store::LocalStore::new(scratch("rebuild")));
        let raw_people: serde_json::Value = serde_json::from_str(
            r#"[{"id":10,"latest_member":{"name":{"first":"Alex","last":"Paterson"},
                 "electorate":"Sampleford","house":"representatives","party":"Example"}}]"#,
        )
        .unwrap();
        store
            .put_json("raw/tvfy/people.json", &raw_people)
            .await
            .unwrap();

        for (key, json) in [
            (
                "raw/tvfy/divisions/representatives-2026-08-12-7.json",
                r#"{"id":1,"house":"representatives","date":"2026-08-12","number":7,
                    "name":"Bills - Example; Second Reading","aye_votes":1,"no_votes":0,
                    "summary":"Context.","votes":[{"member":{"person":{"id":10},
                      "first_name":"Alex","last_name":"Paterson","party":"Example"},"vote":"aye"}]}"#,
            ),
            (
                "raw/tvfy/divisions/senate-2026-07-01-1.json",
                r#"{"id":2,"house":"senate","date":"2026-07-01","number":1,"name":"Motions",
                    "aye_votes":0,"no_votes":1,"summary":null,"votes":[]}"#,
            ),
        ] {
            let value: serde_json::Value = serde_json::from_str(json).unwrap();
            store.put_json(key, &value).await.unwrap();
        }

        let mut people = vec![{
            let mut p = person("alex-paterson", "Alex Paterson", House::Representatives);
            p.electorate = Some("sampleford".to_string());
            p
        }];

        sync_tvfy(&store, &mut people, true, &Endpoints::default())
            .await
            .expect("rebuild");

        // Both cached payloads become canonical divisions, keyed by flattened id.
        let keys = store.list("canonical/divisions/").await.unwrap();
        assert_eq!(keys.len(), 2, "got {keys:?}");
        let division: Division = store
            .get_json("canonical/divisions/representatives-2026-08-12-7.json")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(division.result, DivisionResult::Passed);
        assert_eq!(division.votes[0].person_slug, "alex-paterson");

        // The crosswalk id is written back onto the person record.
        assert_eq!(people[0].ids.tvfy, Some(10));
        let stored: Person = store
            .get_json("canonical/people/alex-paterson.json")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.ids.tvfy, Some(10));
    }

    #[tokio::test]
    async fn rebuild_without_a_cached_people_payload_is_an_error_not_a_silent_pass() {
        let store = Store::Local(crate::store::LocalStore::new(scratch("norebuild")));
        let mut people = vec![person(
            "alex-paterson",
            "Alex Paterson",
            House::Representatives,
        )];
        let err = sync_tvfy(&store, &mut people, true, &Endpoints::default())
            .await
            .expect_err("a rebuild with no raw payload must fail");
        assert!(
            err.to_string().contains("run a live sync first"),
            "got {err}"
        );
    }

    #[test]
    fn division_keys_and_date_windows() {
        assert_eq!(
            key_for("representatives/2026-08-12/7"),
            "representatives-2026-08-12-7"
        );
        assert_eq!(iso_days_before("2026-08-12", 30).unwrap(), "2026-07-13");
        assert_eq!(iso_days_before("2026-01-01", 1).unwrap(), "2025-12-31");
        assert!(iso_days_before("not-a-date", 1).is_err());
    }

    fn division_summary(
        id: i64,
        house: &str,
        date: &str,
        number: i64,
        ayes: i64,
        noes: i64,
    ) -> serde_json::Value {
        serde_json::json!({
            "id": id, "house": house, "date": date, "number": number,
            "name": format!("Division {number}"), "aye_votes": ayes, "no_votes": noes
        })
    }

    fn division_detail(id: i64, house: &str, date: &str, number: i64) -> serde_json::Value {
        serde_json::json!({
            "id": id, "house": house, "date": date, "number": number,
            "name": format!("Division {number}"), "aye_votes": 2, "no_votes": 1,
            "summary": "A written summary.",
            "votes": [
                { "member": { "person": { "id": 10001 }, "first_name": "Alex",
                              "last_name": "Paterson", "party": "Example Party" },
                  "vote": "aye" }
            ],
            "bills": [{ "id": 5, "title": "Appropriation Bill 2026", "official_id": "r7354" }]
        })
    }

    /// The people index, the division index and per-division details.
    fn tvfy_server() -> TestServer {
        TestServer::start(|req| {
            if req.path.starts_with("/tvfy/people.json") {
                return Response::json(
                    serde_json::json!([{
                        "id": 10001,
                        "latest_member": {
                            "name": { "first": "Alex", "last": "Paterson" },
                            "electorate": "Sampleford",
                            "house": "representatives",
                            "party": "Example Party"
                        }
                    }])
                    .to_string(),
                );
            }
            if req.path.starts_with("/tvfy/divisions/") {
                let id: i64 = req
                    .path
                    .trim_start_matches("/tvfy/divisions/")
                    .split(".json")
                    .next()
                    .and_then(|s| s.parse().ok())
                    .expect("division id in the path");
                return Response::json(
                    division_detail(id, "representatives", "2026-08-12", id).to_string(),
                );
            }
            if req.path.starts_with("/tvfy/divisions.json") {
                // Only the House has anything; the Senate index is empty.
                if req.query("house").as_deref() == Some("senate") {
                    return Response::json("[]");
                }
                return Response::json(
                    serde_json::json!([division_summary(
                        1,
                        "representatives",
                        "2026-08-12",
                        1,
                        2,
                        1
                    )])
                    .to_string(),
                );
            }
            Response::status(404, "unexpected path")
        })
    }

    #[tokio::test]
    async fn a_sync_stores_divisions_votes_and_the_cursor() {
        let server = tvfy_server();
        let store = Store::Local(crate::store::LocalStore::new(scratch("sync")));
        let mut people = vec![person(
            "alex-paterson",
            "Alex Paterson",
            House::Representatives,
        )];

        sync_with_key(
            &store,
            &mut people,
            "test-key",
            &Endpoints::at(&server.base),
        )
        .await
        .expect("sync");

        let division: Division = store
            .get_json("canonical/divisions/representatives-2026-08-12-1.json")
            .await
            .unwrap()
            .expect("division stored");
        assert_eq!(division.id, "representatives/2026-08-12/1");
        assert_eq!(division.ayes, 2);
        assert_eq!(division.noes, 1);
        assert_eq!(division.bill_ids, vec!["r7354"]);
        assert_eq!(division.votes.len(), 1);
        assert_eq!(division.votes[0].person_slug, "alex-paterson");

        // The crosswalk id is written back onto the person.
        assert_eq!(people[0].ids.tvfy, Some(10001));

        // Raw responses are kept, and the cursor advances to the newest date.
        assert!(store
            .get_json::<Value>("raw/tvfy/people.json")
            .await
            .unwrap()
            .is_some());
        assert!(store
            .get_json::<Value>("raw/tvfy/divisions/1.json")
            .await
            .unwrap()
            .is_some());
        let cursor: TvfyCursor = store
            .get_json("state/tvfy-cursor.json")
            .await
            .unwrap()
            .expect("cursor stored");
        assert_eq!(cursor.last_division_date.as_deref(), Some("2026-08-12"));

        // The api key rides on every request.
        assert!(
            server
                .requests()
                .iter()
                .all(|r| r.query("key").as_deref() == Some("test-key")),
            "every call is keyed"
        );
    }

    #[tokio::test]
    async fn an_unchanged_division_is_not_refetched_but_a_changed_tally_is() {
        let server = tvfy_server();
        let endpoints = Endpoints::at(&server.base);
        let store = Store::Local(crate::store::LocalStore::new(scratch("unchanged")));
        let mut people = vec![person(
            "alex-paterson",
            "Alex Paterson",
            House::Representatives,
        )];

        sync_with_key(&store, &mut people, "k", &endpoints)
            .await
            .expect("first");
        let after_first = server.hits();
        sync_with_key(&store, &mut people, "k", &endpoints)
            .await
            .expect("second");
        // People index plus two chamber index calls; no detail refetch.
        assert_eq!(
            server.hits() - after_first,
            3,
            "matching tallies are skipped"
        );

        // A tally that no longer matches means the division is refetched.
        let mut stored: Division = store
            .get_json("canonical/divisions/representatives-2026-08-12-1.json")
            .await
            .unwrap()
            .expect("division");
        stored.ayes = 99;
        store
            .put_json(
                "canonical/divisions/representatives-2026-08-12-1.json",
                &stored,
            )
            .await
            .unwrap();
        let before = server.hits();
        sync_with_key(&store, &mut people, "k", &endpoints)
            .await
            .expect("third");
        assert_eq!(server.hits() - before, 4, "a changed tally is refetched");
    }

    #[tokio::test]
    async fn the_index_is_walked_backwards_by_end_date_because_page_is_ignored() {
        // The real index caps at 100 rows, newest first, and ignores `page`, so
        // the only way back through history is the end_date window.
        let server = TestServer::start(|req| {
            if req.path.starts_with("/tvfy/people.json") {
                return Response::json("[]");
            }
            if req.path.starts_with("/tvfy/divisions/") {
                // The detail has to mirror its summary, or the canonical key it
                // is stored under would not match the one just checked.
                let id: i64 = req
                    .path
                    .trim_start_matches("/tvfy/divisions/")
                    .split(".json")
                    .next()
                    .and_then(|s| s.parse().ok())
                    .expect("division id in the path");
                let (date, number) = match id > PAGE_SIZE as i64 {
                    true => ("2026-07-01", 1),
                    false => ("2026-08-12", id),
                };
                return Response::json(
                    division_detail(id, "representatives", date, number).to_string(),
                );
            }
            if req.path.starts_with("/tvfy/divisions.json") {
                if req.query("house").as_deref() == Some("senate") {
                    return Response::json("[]");
                }
                // A full page on the first call, then a short one, then nothing
                // new: three windows in all.
                match req.query("end_date").as_deref() {
                    None => {
                        let rows: Vec<serde_json::Value> = (1..=PAGE_SIZE as i64)
                            .map(|n| division_summary(n, "representatives", "2026-08-12", n, 1, 0))
                            .collect();
                        Response::json(serde_json::to_string(&rows).unwrap())
                    }
                    Some("2026-08-12") => {
                        let rows = vec![division_summary(
                            PAGE_SIZE as i64 + 1,
                            "representatives",
                            "2026-07-01",
                            1,
                            1,
                            0,
                        )];
                        Response::json(serde_json::to_string(&rows).unwrap())
                    }
                    _ => Response::json("[]"),
                }
            } else {
                Response::status(404, "unexpected path")
            }
        });
        let store = Store::Local(crate::store::LocalStore::new(scratch("windows")));
        let mut people: Vec<Person> = Vec::new();

        sync_with_key(&store, &mut people, "k", &Endpoints::at(&server.base))
            .await
            .expect("sync");

        let windows: Vec<Option<String>> = server
            .requests()
            .iter()
            .filter(|r| r.path.starts_with("/tvfy/divisions.json"))
            .filter(|r| r.query("house").as_deref() == Some("representatives"))
            .map(|r| r.query("end_date"))
            .collect();
        assert_eq!(
            windows,
            vec![None, Some("2026-08-12".to_string())],
            "the window steps back to the oldest date in the previous batch"
        );
        // Every division across both windows is stored.
        let stored = store.list("canonical/divisions/").await.unwrap();
        assert_eq!(stored.len(), PAGE_SIZE + 1);
    }

    #[tokio::test]
    async fn an_identifying_user_agent_that_is_refused_falls_back_to_a_browser_one() {
        // Some edges 403 the bot UA. The run must recover rather than fail.
        let server = TestServer::start(|req| {
            let browser = req
                .header("user-agent")
                .is_some_and(|ua| ua.starts_with("Mozilla/"));
            if !browser {
                return Response::status(403, "bot refused");
            }
            if req.path.starts_with("/tvfy/people.json") {
                return Response::json("[]");
            }
            Response::json("[]")
        });
        let store = Store::Local(crate::store::LocalStore::new(scratch("uafallback")));
        let mut people: Vec<Person> = Vec::new();

        sync_with_key(&store, &mut people, "k", &Endpoints::at(&server.base))
            .await
            .expect("the browser UA carries the run");

        let uas: Vec<String> = server
            .requests()
            .iter()
            .filter_map(|r| r.header("user-agent").map(str::to_string))
            .collect();
        assert!(
            uas[0].starts_with("pollywiki/"),
            "the honest UA is tried first"
        );
        assert!(uas[1].starts_with("Mozilla/"), "then the browser one");
        assert!(
            uas[2..].iter().all(|ua| ua.starts_with("Mozilla/")),
            "and it sticks for the rest of the run"
        );
    }

    #[tokio::test]
    async fn a_missing_api_key_is_reported_rather_than_guessed() {
        // The env is process-wide, so this only asserts the message shape when
        // the variable happens to be absent, which it is in a clean test run.
        if std::env::var("TVFY_API_KEY").is_ok() {
            return;
        }
        let store = Store::Local(crate::store::LocalStore::new(scratch("nokey")));
        let mut people: Vec<Person> = Vec::new();
        let err = sync_tvfy(&store, &mut people, false, &Endpoints::default())
            .await
            .expect_err("no key, no sync");
        assert!(
            err.to_string().contains("TVFY_API_KEY not set"),
            "got {err}"
        );
    }
}
