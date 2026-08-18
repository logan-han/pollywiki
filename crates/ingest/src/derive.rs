use crate::manifest::read_manifest;
use crate::sources::aec_profiles::ElectorateProfile;
use crate::sources::handbook::HandbookProfile;
use crate::store::Store;
use crate::summarise::{ai_key, bill_note_key, is_transcript, note_key, AiPersonNote, AiSummary};
use anyhow::Result;
use indexmap::IndexMap;
use pollywiki_schema::{
    js_compare, slugify, AiText, Bill, Division, ElectionContest, Electorate, ElectorateResult,
    House, Meta, Party, PartyFacts, PartySeats, Person, PersonStats, QuickSearchEntry, SummaryKind,
    BUNDLE_BILLS, BUNDLE_DIVISIONS, BUNDLE_ELECTIONS, BUNDLE_ELECTORATES, BUNDLE_PARTIES,
    BUNDLE_PEOPLE,
};
use serde::Serialize;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Default, serde::Deserialize)]
struct PartyReferenceEntry {
    name: Option<String>,
    code: Option<String>,
    colour: Option<String>,
}

/// Turns canonical entities into the precomputed bundles the site build reads.
/// Everything expensive happens here so page templates only render.
pub async fn derive(store: &Store) -> Result<()> {
    let mut people = load_all::<Person>(store, "canonical/people/").await?;
    let mut electorates = load_all::<Electorate>(store, "canonical/electorates/").await?;
    let mut divisions = load_all::<Division>(store, "canonical/divisions/").await?;
    let mut bills = load_all::<Bill>(store, "canonical/bills/").await?;
    let elections = load_all::<ElectorateResult>(store, "canonical/elections/").await?;

    let electorate_index: HashMap<String, usize> = electorates
        .iter()
        .enumerate()
        .map(|(i, e)| (e.slug.clone(), i))
        .collect();
    for person in &mut people {
        if let Some(slug) = &person.electorate {
            if let Some(&i) = electorate_index.get(slug) {
                person.state = Some(electorates[i].state);
                electorates[i].member_slug = Some(person.slug.clone());
            }
        }
    }

    let member_names: HashSet<String> = people.iter().map(|p| p.name.clone()).collect();
    for division in &mut divisions {
        if let Some(summary) = &division.summary {
            division.summary_kind = Some(if is_transcript(summary, &member_names) {
                SummaryKind::Transcript
            } else {
                SummaryKind::Summary
            });
            if division.summary_kind == Some(SummaryKind::Transcript) {
                if let Some(ai) = store.get_json::<AiSummary>(&ai_key(&division.id)).await? {
                    division.ai_summary = Some(AiText {
                        text: ai.text,
                        model: ai.model,
                        generated_at: ai.generated_at,
                    });
                }
            }
        }
    }

    for person in &mut people {
        if let Some(note) = store
            .get_json::<AiPersonNote>(&note_key(&person.slug))
            .await?
        {
            person.ai_note = Some(AiText {
                text: note.text,
                model: note.model,
                generated_at: note.generated_at,
            });
        }
        if let Some(profile) = store
            .get_json::<HandbookProfile>(&format!("canonical/handbook/{}.json", person.slug))
            .await?
        {
            person.background = Some(profile.background);
            if !profile.positions.is_empty() {
                person.positions = Some(profile.positions);
            }
        }
    }

    let people_slugs: HashSet<String> = people.iter().map(|p| p.slug.clone()).collect();
    let phid_to_slug: HashMap<String, String> = people
        .iter()
        .filter_map(|p| {
            p.ids
                .aph
                .as_ref()
                .map(|phid| (phid.to_lowercase(), p.slug.clone()))
        })
        .collect();
    for bill in &mut bills {
        // Empty text records the model's judgement that the official summary
        // is already plain enough; no box renders for those.
        if let Some(note) = store
            .get_json::<AiSummary>(&bill_note_key(&bill.id))
            .await?
        {
            if !note.text.is_empty() {
                bill.ai_summary = Some(AiText {
                    text: note.text,
                    model: note.model,
                    generated_at: note.generated_at,
                });
            }
        }
        for raiser in bill.sponsors.iter_mut().chain(bill.movers.iter_mut()) {
            raiser.slug = raiser
                .phid
                .as_ref()
                .and_then(|phid| phid_to_slug.get(&phid.to_lowercase()).cloned())
                .or_else(|| {
                    let slug = slugify(&raiser.name);
                    people_slugs.contains(&slug).then_some(slug)
                });
        }
    }

    // Election history per person: every contest across every ingested event,
    // matched by candidate name (current members only).
    let slug_to_index: HashMap<String, usize> = people
        .iter()
        .enumerate()
        .map(|(i, p)| (p.slug.clone(), i))
        .collect();
    for result in &elections {
        for candidate in &result.first_prefs {
            let Some(&i) = slug_to_index.get(&slugify(&candidate.name)) else {
                continue;
            };
            let person = &mut people[i];
            person
                .elections
                .get_or_insert_with(Vec::new)
                .push(ElectionContest {
                    event: result.event_id.clone(),
                    event_name: result.event_name.clone(),
                    electorate_slug: result.electorate_slug.clone(),
                    electorate_name: result.electorate_name.clone(),
                    party: candidate.party.clone(),
                    votes: candidate.votes,
                    pct: candidate.pct,
                    swing: candidate.swing,
                    elected: candidate.elected,
                });
        }
    }
    for person in &mut people {
        if let Some(elections) = &mut person.elections {
            elections.sort_by(|a, b| js_compare(&b.event, &a.event));
        }
    }

    for electorate in &mut electorates {
        if let Some(profile) = store
            .get_json::<ElectorateProfile>(&format!(
                "canonical/electorate-profiles/{}.json",
                electorate.slug
            ))
            .await?
        {
            electorate.profile = Some(profile.profile);
            electorate.enrolment = profile.enrolment;
        }
    }

    compute_vote_stats(&mut people, &divisions);
    link_bills(&mut bills, &divisions);
    let mut parties = build_parties(&people);
    for party in &mut parties {
        if let Some(facts) = store
            .get_json::<PartyFacts>(&format!("canonical/party-facts/{}.json", party.slug))
            .await?
        {
            party.facts = Some(facts);
        }
    }

    // Each current electorate shows its own most recent contest, so a seat
    // decided at a by-election displays that result, not the older general.
    let current_slugs: HashSet<&str> = electorates.iter().map(|e| e.slug.as_str()).collect();
    let mut latest_by_electorate: IndexMap<String, &ElectorateResult> = IndexMap::new();
    for result in &elections {
        if !current_slugs.contains(result.electorate_slug.as_str()) {
            continue;
        }
        match latest_by_electorate.get(&result.electorate_slug) {
            Some(current) if result.event_id <= current.event_id => {}
            _ => {
                latest_by_electorate.insert(result.electorate_slug.clone(), result);
            }
        }
    }
    let current_elections: Vec<&ElectorateResult> =
        latest_by_electorate.values().copied().collect();

    write_bundle(
        store,
        BUNDLE_PEOPLE,
        &sorted_by(&people, |p| p.slug.clone()),
    )
    .await?;
    write_bundle(
        store,
        BUNDLE_PARTIES,
        &sorted_by(&parties, |p| p.slug.clone()),
    )
    .await?;
    write_bundle(
        store,
        BUNDLE_ELECTORATES,
        &sorted_by(&electorates, |e| e.slug.clone()),
    )
    .await?;
    let mut divisions_sorted = sorted_by(&divisions, |d| {
        format!("{}-{:0>4}-{}", d.date, d.number.to_string(), d.house)
    });
    divisions_sorted.reverse();
    write_bundle(store, BUNDLE_DIVISIONS, &divisions_sorted).await?;
    write_bundle(store, BUNDLE_BILLS, &sorted_by(&bills, |b| b.title.clone())).await?;
    write_bundle(
        store,
        BUNDLE_ELECTIONS,
        &sorted_by(&current_elections, |e| e.electorate_slug.clone()),
    )
    .await?;

    let manifest = read_manifest(store).await?;
    let meta = Meta {
        generated_at: crate::now_iso(),
        sample: false,
        sources: manifest.sources,
    };
    store.put_json("bundles/meta.json", &meta).await?;

    let mut quick_search: Vec<QuickSearchEntry> = Vec::new();
    // Bills first: they are the most-searched entity, and the site's dropdown
    // groups by type anyway. A few hundred entries of the current parliament.
    for b in &bills {
        quick_search.push(QuickSearchEntry {
            t: "bill".to_string(),
            slug: b.id.clone(),
            name: b.title.clone(),
            sub: b.status.clone(),
        });
    }
    for p in &people {
        quick_search.push(QuickSearchEntry {
            t: "person".to_string(),
            slug: p.slug.clone(),
            name: p.name.clone(),
            sub: match p.house {
                House::Senate => format!(
                    "Senator \u{b7} {}",
                    p.state.map(|s| s.as_str()).unwrap_or("")
                ),
                House::Representatives => {
                    format!("MP \u{b7} {}", title_from_slug(p.electorate.as_deref()))
                }
            },
        });
    }
    for e in &electorates {
        quick_search.push(QuickSearchEntry {
            t: "electorate".to_string(),
            slug: e.slug.clone(),
            name: e.name.clone(),
            sub: format!("Electorate \u{b7} {}", e.state),
        });
    }
    store
        .put_json("bundles/quick-search.json", &quick_search)
        .await?;

    println!(
        "derive: {} people, {} parties, {} electorates, {} divisions, {} bills, {} electorate results",
        people.len(),
        parties.len(),
        electorates.len(),
        divisions.len(),
        bills.len(),
        current_elections.len()
    );
    Ok(())
}

fn compute_vote_stats(people: &mut [Person], divisions: &[Division]) {
    struct Tally {
        voted: i64,
        against: i64,
    }
    let mut stats: HashMap<&str, Tally> = HashMap::new();
    for division in divisions {
        for vote in &division.votes {
            let s = stats.entry(&vote.person_slug).or_insert(Tally {
                voted: 0,
                against: 0,
            });
            s.voted += 1;
            if vote.against_group_majority == Some(true) {
                s.against += 1;
            }
        }
    }
    let per_house = |house: House| divisions.iter().filter(|d| d.house == house).count() as i64;
    let divisions_per_house = (per_house(House::Representatives), per_house(House::Senate));
    for person in people {
        let eligible = match person.house {
            House::Representatives => divisions_per_house.0,
            House::Senate => divisions_per_house.1,
        };
        if eligible == 0 {
            continue;
        }
        let s = stats.get(person.slug.as_str());
        person.stats = Some(PersonStats {
            divisions_eligible: eligible,
            divisions_voted: s.map(|s| s.voted).unwrap_or(0),
            against_group_majority: s.map(|s| s.against).unwrap_or(0),
        });
    }
}

fn link_bills(bills: &mut [Bill], divisions: &[Division]) {
    let by_id: HashMap<String, usize> = bills
        .iter()
        .enumerate()
        .map(|(i, b)| (b.id.clone(), i))
        .collect();
    for division in divisions {
        for bill_id in &division.bill_ids {
            if let Some(&i) = by_id.get(bill_id) {
                if !bills[i].division_ids.contains(&division.id) {
                    bills[i].division_ids.push(division.id.clone());
                }
            }
        }
    }
}

fn build_parties(people: &[Person]) -> Vec<Party> {
    let reference: IndexMap<String, PartyReferenceEntry> = {
        let path = crate::reference_path("parties.json");
        match std::fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => {
                eprintln!("derive: data/reference/parties.json not found, using defaults");
                IndexMap::new()
            }
        }
    };

    let mut groups: IndexMap<String, Party> = IndexMap::new();
    for person in people {
        let entry = groups.entry(person.group_slug.clone()).or_insert_with(|| {
            let default = PartyReferenceEntry::default();
            let reference_entry = reference.get(&person.group_slug).unwrap_or(&default);
            Party {
                slug: person.group_slug.clone(),
                name: reference_entry
                    .name
                    .clone()
                    .unwrap_or_else(|| person.group.clone()),
                code: reference_entry.code.clone(),
                colour: reference_entry.colour.clone(),
                seats: Some(PartySeats {
                    representatives: 0,
                    senate: 0,
                }),
                facts: None,
            }
        });
        if let Some(seats) = &mut entry.seats {
            *seats.get_mut(person.house) += 1;
        }
    }
    groups.into_values().collect()
}

async fn load_all<T: serde::de::DeserializeOwned>(store: &Store, prefix: &str) -> Result<Vec<T>> {
    let mut out = Vec::new();
    for key in store.list(prefix).await? {
        if !key.ends_with(".json") {
            continue;
        }
        if let Some(value) = store.get_json::<T>(&key).await? {
            out.push(value);
        }
    }
    Ok(out)
}

async fn write_bundle<T: Serialize>(store: &Store, file: &str, records: &[T]) -> Result<()> {
    let mut jsonl = String::new();
    for record in records {
        jsonl.push_str(&serde_json::to_string(record)?);
        jsonl.push('\n');
    }
    store
        .put_raw(&format!("bundles/{file}"), jsonl.as_bytes())
        .await
}

fn sorted_by<T: Clone>(items: &[T], key: impl Fn(&T) -> String) -> Vec<T> {
    let mut keyed: Vec<(String, T)> = items.iter().map(|i| (key(i), i.clone())).collect();
    keyed.sort_by(|a, b| match js_compare(&a.0, &b.0) {
        Ordering::Equal => Ordering::Equal,
        other => other,
    });
    keyed.into_iter().map(|(_, i)| i).collect()
}

fn title_from_slug(slug: Option<&str>) -> String {
    let Some(slug) = slug else {
        return String::new();
    };
    let spaced = slug.replace('-', " ");
    let mut out = String::with_capacity(spaced.len());
    let mut at_boundary = true;
    for c in spaced.chars() {
        if at_boundary && c.is_ascii_lowercase() {
            out.push(c.to_ascii_uppercase());
        } else {
            out.push(c);
        }
        at_boundary = !c.is_alphanumeric();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::LocalStore;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/derive-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    async fn put(store: &Store, key: &str, json: &str) {
        let value: serde_json::Value = serde_json::from_str(json).expect("fixture json");
        store.put_json(key, &value).await.expect("put fixture");
    }

    fn lines<T: serde::de::DeserializeOwned>(raw: &str) -> Vec<T> {
        raw.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("bundle line"))
            .collect()
    }

    /// A canonical store with one of everything derive reads.
    async fn seeded(name: &str) -> Store {
        let store = Store::Local(LocalStore::new(scratch(name)));

        put(
            &store,
            "canonical/people/alex-paterson.json",
            r#"{
            "slug":"alex-paterson","name":"Alex Paterson","house":"representatives","state":"VIC",
            "electorate":"sampleford","group":"Example Party","groupSlug":"example-party",
            "ids":{"aph":"ALEX1"},"links":{}}"#,
        )
        .await;
        put(
            &store,
            "canonical/people/morgan-rossi.json",
            r#"{
            "slug":"morgan-rossi","name":"Morgan Rossi","house":"senate","state":"TAS",
            "group":"Example Party","groupSlug":"example-party","ids":{},"links":{}}"#,
        )
        .await;

        put(
            &store,
            "canonical/electorates/sampleford.json",
            r#"{
            "slug":"sampleford","name":"Sampleford","state":"VIC","memberSlug":"alex-paterson"}"#,
        )
        .await;
        put(
            &store,
            "canonical/electorates/placeholder-bay.json",
            r#"{
            "slug":"placeholder-bay","name":"Placeholder Bay","state":"NSW"}"#,
        )
        .await;

        // Two divisions: the House one carries a crossed vote and cites a bill.
        put(
            &store,
            "canonical/divisions/a.json",
            r#"{
            "id":"representatives/2026-08-12/2","house":"representatives","date":"2026-08-12",
            "number":2,"name":"Bills - Example Bill 2026; Second Reading","result":"passed",
            "ayes":1,"noes":0,"billIds":["r100"],"links":{},
            "votes":[{"personSlug":"alex-paterson","vote":"aye","againstGroupMajority":true}]}"#,
        )
        .await;
        put(
            &store,
            "canonical/divisions/b.json",
            r#"{
            "id":"senate/2026-07-01/1","house":"senate","date":"2026-07-01","number":1,
            "name":"Motions - Example","result":"rejected","ayes":0,"noes":1,"links":{},
            "votes":[{"personSlug":"morgan-rossi","vote":"no"}]}"#,
        )
        .await;

        // One bill raised by phid, one by a name that resolves to a slug, and a
        // third raised by someone who is not a sitting member.
        put(
            &store,
            "canonical/bills/r100.json",
            r#"{
            "id":"r100","title":"Example Bill 2026","parliament":48,"chamber":"representatives",
            "status":"Before Senate","links":{},
            "movers":[{"name":"Alex Paterson","phid":"alex1"}],
            "timeline":[{"date":"2026-08-01","event":"Introduced (House of Representatives)"}]}"#,
        )
        .await;
        put(
            &store,
            "canonical/bills/r200.json",
            r#"{
            "id":"r200","title":"Another Bill 2026","parliament":48,"chamber":"senate",
            "status":"Act","links":{},
            "sponsors":[{"name":"Morgan Rossi"},{"name":"Someone Retired"}]}"#,
        )
        .await;

        // Two contests for the same seat; only the newer event is current.
        put(
            &store,
            "canonical/elections/old.json",
            r#"{
            "eventId":"27966","eventName":"2022 federal election","electorateSlug":"sampleford",
            "electorateName":"Sampleford","state":"VIC",
            "firstPrefs":[{"name":"Alex Paterson","party":"Example Party","votes":100,"pct":40.0,
                           "elected":false}],"tcp":[]}"#,
        )
        .await;
        put(
            &store,
            "canonical/elections/new.json",
            r#"{
            "eventId":"31496","eventName":"2025 federal election","electorateSlug":"sampleford",
            "electorateName":"Sampleford","state":"VIC",
            "firstPrefs":[{"name":"Alex Paterson","party":"Example Party","votes":200,"pct":55.5,
                           "swing":15.5,"elected":true}],"tcp":[]}"#,
        )
        .await;
        // A contest for a seat that no longer exists must not reach the bundle.
        put(
            &store,
            "canonical/elections/gone.json",
            r#"{
            "eventId":"31496","eventName":"2025 federal election","electorateSlug":"abolished",
            "electorateName":"Abolished","state":"NSW","firstPrefs":[],"tcp":[]}"#,
        )
        .await;

        put(
            &store,
            "canonical/electorate-profiles/sampleford.json",
            r#"{
            "storedAt":"2026-08-01T00:00:00.000Z","enrolment":118432,
            "profile":{"area":"142 sq km","demographic":"Inner metropolitan"}}"#,
        )
        .await;
        put(&store, "canonical/party-facts/example-party.json", r#"{
            "website":"https://example.org.au/","wikipedia":"https://en.wikipedia.org/wiki/Example"}"#).await;
        put(
            &store,
            "canonical/handbook/alex-paterson.json",
            r#"{
            "phid":"ALEX1","storedAt":"2026-08-01T00:00:00.000Z",
            "background":{"born":"1970-01-01","occupations":["Grazier"],"qualifications":[]},
            "positions":[{"role":"Prime Minister","kind":"ministry","from":"2025-05-03"}]}"#,
        )
        .await;

        crate::manifest::record_sync(&store, "wikidata", true, None)
            .await
            .expect("manifest");
        store
    }

    #[tokio::test]
    async fn derive_assembles_every_bundle_from_the_canonical_store() {
        let store = seeded("bundles").await;
        // No env mutation here: tests share a process, and build_parties falls
        // back to the group name from the record when the reference file is not
        // on the test's relative path, which is what these assertions check.
        derive(&store).await.expect("derive");

        let people: Vec<Person> = lines(
            &store
                .get_raw("bundles/people.jsonl")
                .await
                .unwrap()
                .unwrap(),
        );
        assert_eq!(
            people.iter().map(|p| p.slug.as_str()).collect::<Vec<_>>(),
            vec!["alex-paterson", "morgan-rossi"],
            "people are written in slug order"
        );

        // The Handbook profile and positions are folded in.
        let alex = &people[0];
        assert_eq!(
            alex.background.as_ref().map(|b| b.occupations.clone()),
            Some(vec!["Grazier".to_string()])
        );
        assert_eq!(
            alex.positions.as_ref().map(|p| p.len()),
            Some(1),
            "positions come from the handbook record"
        );

        // Election history attaches by candidate name, newest event first.
        let elections = alex.elections.as_ref().expect("election history");
        assert_eq!(
            elections
                .iter()
                .map(|e| e.event.as_str())
                .collect::<Vec<_>>(),
            vec!["31496", "27966"]
        );
        assert!(elections[0].elected);

        // Vote stats count per chamber: one division each, both voted in.
        let stats = alex.stats.as_ref().expect("stats");
        assert_eq!(
            (
                stats.divisions_eligible,
                stats.divisions_voted,
                stats.against_group_majority
            ),
            (1, 1, 1)
        );
        let rossi_stats = people[1].stats.as_ref().expect("stats");
        assert_eq!(rossi_stats.against_group_majority, 0);

        // Divisions come out newest first, which is what every index relies on.
        let divisions: Vec<Division> = lines(
            &store
                .get_raw("bundles/divisions.jsonl")
                .await
                .unwrap()
                .unwrap(),
        );
        assert_eq!(
            divisions.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(),
            vec!["representatives/2026-08-12/2", "senate/2026-07-01/1"]
        );

        // Bills are alphabetical, linked back to their divisions, with raisers
        // resolved by phid or name and unknown raisers left unresolved.
        let bills: Vec<Bill> = lines(&store.get_raw("bundles/bills.jsonl").await.unwrap().unwrap());
        assert_eq!(
            bills.iter().map(|b| b.title.as_str()).collect::<Vec<_>>(),
            vec!["Another Bill 2026", "Example Bill 2026"]
        );
        let example = bills.iter().find(|b| b.id == "r100").unwrap();
        assert_eq!(example.division_ids, vec!["representatives/2026-08-12/2"]);
        assert_eq!(
            example.movers[0].slug.as_deref(),
            Some("alex-paterson"),
            "phid match is case-insensitive"
        );
        let another = bills.iter().find(|b| b.id == "r200").unwrap();
        assert_eq!(another.sponsors[0].slug.as_deref(), Some("morgan-rossi"));
        assert!(
            another.sponsors[1].slug.is_none(),
            "a raiser who is not a sitting member stays unlinked"
        );

        // Electorate profiles and enrolment are merged onto the seat.
        let electorates: Vec<Electorate> = lines(
            &store
                .get_raw("bundles/electorates.jsonl")
                .await
                .unwrap()
                .unwrap(),
        );
        let sampleford = electorates.iter().find(|e| e.slug == "sampleford").unwrap();
        assert_eq!(sampleford.enrolment, Some(118432));
        assert_eq!(
            sampleford.profile.as_ref().and_then(|p| p.area.clone()),
            Some("142 sq km".to_string())
        );
        assert!(electorates
            .iter()
            .find(|e| e.slug == "placeholder-bay")
            .unwrap()
            .profile
            .is_none());

        // One party, both members counted per chamber, with its facts attached.
        let parties: Vec<Party> = lines(
            &store
                .get_raw("bundles/parties.jsonl")
                .await
                .unwrap()
                .unwrap(),
        );
        assert_eq!(parties.len(), 1);
        let seats = parties[0].seats.as_ref().expect("seats");
        assert_eq!((seats.representatives, seats.senate), (1, 1));
        assert!(parties[0].facts.is_some(), "party facts are merged in");

        // Only the newest contest per current seat; abolished seats drop out.
        let current: Vec<ElectorateResult> = lines(
            &store
                .get_raw("bundles/elections.jsonl")
                .await
                .unwrap()
                .unwrap(),
        );
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].event_id, "31496");
        assert_eq!(current[0].electorate_slug, "sampleford");

        // Meta records the manifest and is never marked as sample data.
        let meta: Meta =
            serde_json::from_str(&store.get_raw("bundles/meta.json").await.unwrap().unwrap())
                .expect("meta");
        assert!(!meta.sample);
        assert!(meta.sources.contains_key("wikidata"));
        assert!(!meta.generated_at.is_empty());

        // The quick-search index leads with bills, then people, then seats.
        let quick: Vec<QuickSearchEntry> = serde_json::from_str(
            &store
                .get_raw("bundles/quick-search.json")
                .await
                .unwrap()
                .unwrap(),
        )
        .expect("quick search");
        let kinds: Vec<&str> = quick.iter().map(|e| e.t.as_str()).collect();
        assert_eq!(
            kinds,
            vec![
                "bill",
                "bill",
                "person",
                "person",
                "electorate",
                "electorate"
            ]
        );
        let bill_entry = &quick[0];
        assert!(
            bill_entry.slug == "r100" || bill_entry.slug == "r200",
            "bills are indexed by id"
        );
        assert!(quick.iter().any(|e| e.sub == "Before Senate"));
    }

    #[tokio::test]
    async fn derive_over_an_empty_store_writes_empty_bundles() {
        let store = Store::Local(LocalStore::new(scratch("empty")));
        derive(&store).await.expect("derive over nothing");
        for file in [
            "people.jsonl",
            "parties.jsonl",
            "electorates.jsonl",
            "divisions.jsonl",
            "bills.jsonl",
            "elections.jsonl",
        ] {
            let raw = store
                .get_raw(&format!("bundles/{file}"))
                .await
                .unwrap()
                .unwrap_or_default();
            assert!(raw.trim().is_empty(), "{file} should be empty, got {raw}");
        }
    }
}
