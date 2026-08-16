use crate::http::{fetch_json, FetchOpts};
use crate::store::Store;
use anyhow::{anyhow, Result};
use indexmap::IndexMap;
use pollywiki_schema::{
    slugify, Division, DivisionLinks, DivisionResult, House, Person, Vote, VoteCast,
};
use serde::Deserialize;
use serde_json::Value;

const BASE: &str = "https://theyvoteforyou.org.au/api/v1";
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

struct TvfyClient {
    key: String,
    requests: u32,
    cap: u32,
    ua_blocked: bool,
}

impl TvfyClient {
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
        let url = format!("{BASE}/{path}.json?{qs}");
        let browser = FetchOpts::min_interval(1200).with_header("user-agent", BROWSER_UA);
        if self.ua_blocked {
            return fetch_json(&url, &browser).await;
        }
        match fetch_json(&url, &FetchOpts::min_interval(1200)).await {
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
pub async fn sync_tvfy(store: &Store, people: &mut [Person], rebuild: bool) -> Result<()> {
    if rebuild {
        return rebuild_from_raw(store, people).await;
    }
    let key = std::env::var("TVFY_API_KEY")
        .map_err(|_| anyhow!("TVFY_API_KEY not set; skipping They Vote For You sync"))?;
    let mut client = TvfyClient {
        key,
        requests: 0,
        cap: request_cap(),
        ua_blocked: false,
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
