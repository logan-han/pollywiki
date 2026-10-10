use crate::manifest::read_manifest;
use crate::sources::aec::pct_of;
use crate::sources::aec_profiles::ElectorateProfile;
use crate::sources::handbook::{HandbookElectorates, HandbookProfile, ELECTORATES_KEY};
use crate::sources::ipea::{IpeaParliamentarian, IpeaQuarter};
use crate::sources::legislation::ACTS_KEY;
use crate::store::Store;
use crate::summarise::{ai_key, bill_note_key, is_transcript, note_key, AiPersonNote, AiSummary};
use anyhow::Result;
use indexmap::IndexMap;
use pollywiki_schema::{
    js_compare, slugify, title_from_slug, Act, AiText, Bill, Division, ElectionContest, Electorate,
    ElectorateResult, ExpenseLine, ExpenseQuarter, House, JsNum, Meta, Party, PartyFacts,
    PartySeats, Person, PersonStats, QuickSearchEntry, SeatResult, SenateResult, SenateSeat,
    StateCode, SummaryKind, BUNDLE_BILLS, BUNDLE_DIVISIONS, BUNDLE_ELECTIONS, BUNDLE_ELECTORATES,
    BUNDLE_PARTIES, BUNDLE_PEOPLE,
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
    let senate = load_all::<SenateResult>(store, "canonical/senate/").await?;

    let electorate_index: HashMap<String, usize> = electorates
        .iter()
        .enumerate()
        .map(|(i, e)| (e.slug.clone(), i))
        .collect();
    for person in &mut people {
        if let Some(slug) = &person.electorate {
            if let Some(&i) = electorate_index.get(slug) {
                person.state = Some(electorates[i].state);
                // Only whoever sits for the seat now is its member; a
                // predecessor keeps the electorate on their own record.
                if !person.is_former() {
                    electorates[i].member_slug = Some(person.slug.clone());
                }
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
            person.committees = profile.committees.filter(|c| !c.is_empty());
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
    let acts: IndexMap<String, Act> = store.get_json(ACTS_KEY).await?.unwrap_or_default();
    for bill in &mut bills {
        bill.act = acts.get(&bill.id.to_lowercase()).cloned();
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
    // The share is worked out again here from the votes, over the formal
    // total, rather than copied: results stored before the ingest set the
    // informal ballots apart took every share over all ballots cast, and
    // this corrects them on the next derive without refetching the AEC.
    for result in &elections {
        let formal = result.formal_votes() as f64;
        for candidate in result.first_prefs.iter().filter(|c| !c.is_informal()) {
            let Some(i) = find_candidate(&people, &slug_to_index, &[&candidate.name], result.state)
            else {
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
                    pct: pct_of(candidate.votes as f64, formal),
                    swing: candidate.swing,
                    elected: candidate.elected,
                    senate: None,
                });
        }
    }
    // Senate contests carry the whole group's share: the count is state-wide
    // and most votes are cast for the group, not the candidate.
    for result in &senate {
        for group in &result.groups {
            for candidate in &group.candidates {
                let names: Vec<&str> = [Some(&candidate.name), candidate.short_name.as_ref()]
                    .into_iter()
                    .flatten()
                    .map(String::as_str)
                    .collect();
                let Some(i) = find_candidate(&people, &slug_to_index, &names, result.state) else {
                    continue;
                };
                if people[i].state != Some(result.state) {
                    continue;
                }
                people[i]
                    .elections
                    .get_or_insert_with(Vec::new)
                    .push(ElectionContest {
                        event: result.event_id.clone(),
                        event_name: result.event_name.clone(),
                        electorate_slug: String::new(),
                        electorate_name: result.state.as_str().to_string(),
                        party: candidate.party.clone(),
                        votes: group.votes,
                        pct: group.pct,
                        swing: None,
                        elected: candidate.elected_order.is_some(),
                        senate: Some(SenateSeat {
                            state: result.state,
                            vacancies: result.vacancies,
                            elected_order: candidate.elected_order,
                        }),
                    });
            }
        }
    }
    for person in &mut people {
        if let Some(elections) = &mut person.elections {
            elections.sort_by(|a, b| js_compare(&b.event, &a.event));
        }
    }

    let handbook_electorates = store
        .get_json::<HandbookElectorates>(ELECTORATES_KEY)
        .await?
        .map(|h| h.electorates)
        .unwrap_or_default();
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
        let history: Vec<SeatResult> = elections
            .iter()
            .filter(|r| r.electorate_slug == electorate.slug && r.state == electorate.state)
            .filter_map(|r| seat_result(r, &people))
            .sorted_newest_first();
        electorate.history = (!history.is_empty()).then_some(history);
        // A name can be reused after a division is abolished, so only the
        // sitting division of that name in that state counts.
        electorate.established = handbook_electorates
            .iter()
            .find(|h| {
                h.ceased.is_none()
                    && h.state == electorate.state.as_str()
                    && slugify(&h.name) == electorate.slug
            })
            .and_then(|h| h.established.clone());
    }

    attach_expenses(store, &mut people).await?;
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
        let seat = match p.house {
            House::Senate => format!(
                "Senator \u{b7} {}",
                p.state.map(|s| s.as_str()).unwrap_or("")
            ),
            House::Representatives => {
                format!("MP \u{b7} {}", title_from_slug(p.electorate.as_deref()))
            }
        };
        quick_search.push(QuickSearchEntry {
            t: "person".to_string(),
            slug: p.slug.clone(),
            name: p.name.clone(),
            sub: if p.is_former() {
                format!("Former {seat}")
            } else {
                seat
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
        "derive: {} people ({} former), {} parties, {} electorates, {} divisions, {} bills, {} electorate results",
        people.len(),
        people.iter().filter(|p| p.is_former()).count(),
        parties.len(),
        electorates.len(),
        divisions.len(),
        bills.len(),
        current_elections.len()
    );
    Ok(())
}

/// Each member's latest quarters of IPEA expenses, newest first. IPEA names
/// members as they are styled, so the match is on first name and surname,
/// then on surname and electorate for a member, or surname and state for a
/// senator, and only where one member fits.
async fn attach_expenses(store: &Store, people: &mut [Person]) -> Result<()> {
    let mut quarters = load_all::<IpeaQuarter>(store, crate::sources::ipea::PREFIX).await?;
    quarters.sort_by(|a, b| b.period.cmp(&a.period));
    for quarter in &quarters {
        for record in &quarter.parliamentarians {
            let Some(i) = expense_owner(people, record) else {
                continue;
            };
            let to_dollars = |cents: i64| JsNum(cents as f64 / 100.0);
            people[i]
                .expenses
                .get_or_insert_with(Vec::new)
                .push(ExpenseQuarter {
                    period: quarter.period.clone(),
                    label: quarter.label.clone(),
                    total: to_dollars(record.categories.iter().map(|(_, c)| c).sum()),
                    categories: record
                        .categories
                        .iter()
                        .map(|(category, cents)| ExpenseLine {
                            category: category.clone(),
                            amount: to_dollars(*cents),
                        })
                        .collect(),
                });
        }
    }
    Ok(())
}

fn expense_owner(people: &[Person], record: &IpeaParliamentarian) -> Option<usize> {
    let state = StateCode::parse(&record.state.to_uppercase());
    let in_state = |p: &Person| state.is_none() || p.state.is_none() || p.state == state;
    let surname = slugify(&record.surname);
    let exact = slugify(&format!("{} {}", record.first_name, record.surname));
    let only = |hits: Vec<usize>| match hits.as_slice() {
        [one] => Some(*one),
        _ => None,
    };
    let by = |keep: &dyn Fn(&Person) -> bool| -> Vec<usize> {
        people
            .iter()
            .enumerate()
            .filter(|(_, p)| keep(p))
            .map(|(i, _)| i)
            .collect()
    };
    let ends_with_surname = |p: &Person| p.slug.ends_with(&format!("-{surname}"));
    if let Some(i) = only(by(&|p: &Person| p.slug == exact && in_state(p))) {
        return Some(i);
    }
    if let Some(electorate) = &record.electorate {
        let electorate = slugify(electorate);
        if let Some(i) = only(by(&|p: &Person| {
            p.electorate.as_deref() == Some(electorate.as_str()) && ends_with_surname(p)
        })) {
            return Some(i);
        }
    }
    only(by(&|p: &Person| {
        p.house == House::Senate && state.is_some() && p.state == state && ends_with_surname(p)
    }))
}

/// The member a ballot name belongs to. Twenty years of candidates hold
/// namesakes, so a match must sit in the contest's state. Failing the exact
/// name, one member with the same surname whose first name is the other's
/// short form ("Raff" on the roll as "Raffaele") is taken, and only one.
fn find_candidate(
    people: &[Person],
    slug_to_index: &HashMap<String, usize>,
    names: &[&str],
    state: StateCode,
) -> Option<usize> {
    let in_state = |i: usize| people[i].state.is_none_or(|s| s == state);
    if let Some(&i) = names
        .iter()
        .find_map(|name| slug_to_index.get(&slugify(name)))
    {
        return in_state(i).then_some(i);
    }
    let split = |slug: String| -> Option<(String, String)> {
        let (first, rest) = slug.split_once('-')?;
        Some((first.to_string(), rest.to_string()))
    };
    let (first, rest) = split(slugify(names.first()?))?;
    let close: Vec<usize> = people
        .iter()
        .enumerate()
        .filter(|(i, p)| {
            in_state(*i)
                && p.state.is_some()
                && split(p.slug.clone()).is_some_and(|(pf, pr)| {
                    pr == rest
                        && pf.len().min(first.len()) >= 3
                        && (pf.starts_with(&first) || first.starts_with(&pf))
                })
        })
        .map(|(i, _)| i)
        .collect();
    match close.as_slice() {
        [only] => Some(*only),
        _ => None,
    }
}

/// Who took the seat at one event, with their two-candidate-preferred share
/// where the AEC published the count. The winner links to a profile only
/// when that member sat for the same state.
fn seat_result(result: &ElectorateResult, people: &[Person]) -> Option<SeatResult> {
    let winner = result
        .first_prefs
        .iter()
        .find(|c| c.elected && !c.is_informal())?;
    let tcp_total: i64 = result.tcp.iter().map(|c| c.votes).sum();
    let tcp_pct = result
        .tcp
        .iter()
        .find(|c| c.name == winner.name)
        .filter(|_| tcp_total > 0)
        .map(|c| pct_of(c.votes as f64, tcp_total as f64));
    let slug = slugify(&winner.name);
    let person_slug = people
        .iter()
        .any(|p| p.slug == slug && p.state.is_none_or(|s| s == result.state))
        .then_some(slug);
    Some(SeatResult {
        event: result.event_id.clone(),
        event_name: result.event_name.clone(),
        member: winner.name.clone(),
        party: winner.party.clone(),
        tcp_pct,
        person_slug,
    })
}

trait NewestFirst {
    fn sorted_newest_first(self) -> Vec<SeatResult>;
}

impl<I: Iterator<Item = SeatResult>> NewestFirst for I {
    fn sorted_newest_first(self) -> Vec<SeatResult> {
        let mut out: Vec<SeatResult> = self.collect();
        out.sort_by(|a, b| js_compare(&b.event, &a.event));
        out
    }
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
    for person in people {
        // Only divisions held while the person sat: someone who arrived at a
        // by-election or left mid-term never had the chance to vote in the rest,
        // and counting those against them would misread the record.
        let eligible = divisions
            .iter()
            .filter(|d| d.house == person.house && person.served_on(&d.date))
            .count() as i64;
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
    // Seat counts describe the parliament as it stands, so former members are
    // left out; a group only they belonged to drops off with them.
    for person in people.iter().filter(|p| !p.is_former()) {
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
            "firstPrefs":[{"name":"Casey Doe","party":"Independent","votes":150,"pct":60.0,
                           "elected":true},
                          {"name":"Alex Paterson","party":"Example Party","votes":100,"pct":40.0,
                           "elected":false}],"tcp":[]}"#,
        )
        .await;
        // Stored as the ingest once wrote it: the informal ballots named
        // twice over and counted into every share, so 222 of 500 ballots
        // reads 44.4 where the formal share is 222 of 400.
        put(
            &store,
            "canonical/elections/new.json",
            r#"{
            "eventId":"31496","eventName":"2025 federal election","electorateSlug":"sampleford",
            "electorateName":"Sampleford","state":"VIC",
            "firstPrefs":[{"name":"Alex Paterson","party":"Example Party","votes":222,"pct":44.4,
                           "swing":15.5,"elected":true},
                          {"name":"Casey Doe","party":"Independent","votes":178,"pct":35.6,
                           "elected":false},
                          {"name":"Informal Informal","party":"Informal","votes":100,"pct":20.0,
                           "swing":1.2,"elected":false}],"tcp":[]}"#,
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
        // Each share is of the formal votes, whatever the stored figure was,
        // and the informal ballots are nobody's contest.
        assert_eq!(elections[0].pct.0, 55.5);
        assert_eq!(elections[0].votes, 222);
        assert_eq!(elections[1].pct.0, 40.0);
        assert!(people
            .iter()
            .flat_map(|p| p.elections.iter().flatten())
            .all(|e| e.party != "Informal"));

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
    async fn senate_counts_seat_histories_and_handbook_extras_join_within_the_state() {
        let store = seeded("senate-history").await;
        put(
            &store,
            "canonical/senate/31496/tas.json",
            r#"{
            "eventId":"31496","eventName":"2025 federal election","state":"TAS",
            "vacancies":6,"formalVotes":1000,
            "groups":[{"ticket":"A","party":"Example Party","votes":400,"pct":40,
              "candidates":[
                {"name":"Morgan Lee Rossi","shortName":"Morgan Rossi","party":"Example Party",
                 "votes":50,"electedOrder":2},
                {"name":"Alex Paterson","party":"Example Party","votes":3}]}]}"#,
        )
        .await;
        // The roll's long form of a member's first name, and a different
        // first name on the same surname.
        put(
            &store,
            "canonical/senate/24310/tas.json",
            r#"{
            "eventId":"24310","eventName":"2019 federal election","state":"TAS",
            "vacancies":6,"formalVotes":1000,
            "groups":[{"ticket":"B","party":"Example Party","votes":300,"pct":30,
              "candidates":[
                {"name":"Morgana Rossi","party":"Example Party","votes":20,"electedOrder":3},
                {"name":"Mo Rossi","party":"Example Party","votes":5}]}]}"#,
        )
        .await;
        // A namesake who stood for a House seat in another state.
        put(
            &store,
            "canonical/elections/namesake.json",
            r#"{
            "eventId":"24310","eventName":"2019 federal election","electorateSlug":"elsewhere",
            "electorateName":"Elsewhere","state":"NSW",
            "firstPrefs":[{"name":"Alex Paterson","party":"Other Party","votes":10,"pct":10,
                           "elected":false}],"tcp":[]}"#,
        )
        .await;
        put(
            &store,
            "canonical/elections/2004.json",
            r#"{
            "eventId":"12246","eventName":"2004 federal election","electorateSlug":"sampleford",
            "electorateName":"Sampleford","state":"VIC",
            "firstPrefs":[{"name":"Casey Doe","party":"Independent","votes":60,"pct":60,
                           "elected":true},
                          {"name":"Alex Paterson","party":"Example Party","votes":40,"pct":40,
                           "elected":false}],
            "tcp":[{"name":"Casey Doe","party":"Independent","votes":55,"pct":55,"elected":true},
                   {"name":"Alex Paterson","party":"Example Party","votes":45,"pct":45,
                    "elected":false}]}"#,
        )
        .await;
        put(
            &store,
            "canonical/handbook/alex-paterson.json",
            r#"{
            "phid":"ALEX1","storedAt":"2026-08-01T00:00:00.000Z",
            "background":{"occupations":[],"qualifications":[]},"positions":[],
            "committees":[{"name":"Example Affairs","kind":"Joint Standing","from":"2025-07-28"}]}"#,
        )
        .await;
        put(
            &store,
            ELECTORATES_KEY,
            r#"{"storedAt":"2026-08-01T00:00:00.000Z","electorates":[
              {"name":"Sampleford","state":"NSW","established":"1949-03-11"},
              {"name":"Sampleford","state":"VIC","established":"1900-10-08","ceased":"1922-01-01"},
              {"name":"Sampleford","state":"VIC","established":"1922-01-02"}]}"#,
        )
        .await;
        derive(&store).await.expect("derive");

        let people: Vec<Person> = lines(
            &store
                .get_raw("bundles/people.jsonl")
                .await
                .unwrap()
                .unwrap(),
        );
        let alex = people.iter().find(|p| p.slug == "alex-paterson").unwrap();
        let events: Vec<&str> = alex
            .elections
            .iter()
            .flatten()
            .map(|e| e.event.as_str())
            .collect();
        // The NSW namesake and the Tasmanian Senate candidate are someone else.
        assert_eq!(events, ["31496", "27966", "12246"]);
        assert!(alex.elections.iter().flatten().all(|e| e.senate.is_none()));
        assert_eq!(
            alex.committees.as_ref().map(|c| c[0].name.as_str()),
            Some("Example Affairs")
        );

        let rossi = people.iter().find(|p| p.slug == "morgan-rossi").unwrap();
        let contests = rossi.elections.as_ref().expect("senate contests");
        // "Mo" is too short a form to stand for anyone.
        assert_eq!(contests.len(), 2);
        assert_eq!(
            contests[1].senate.as_ref().and_then(|s| s.elected_order),
            Some(3),
            "the long form of the first name is the same member"
        );
        let contest = &contests[0];
        let seat = contest.senate.as_ref().expect("a Senate row");
        assert_eq!((seat.vacancies, seat.elected_order), (6, Some(2)));
        assert!(contest.elected);
        assert_eq!(
            contest.votes, 400,
            "the group's votes, not the candidate's own"
        );
        assert_eq!(contest.pct.0, 40.0);
        assert!(rossi.committees.is_none(), "no profile, no committees");

        let electorates: Vec<Electorate> = lines(
            &store
                .get_raw("bundles/electorates.jsonl")
                .await
                .unwrap()
                .unwrap(),
        );
        let sampleford = electorates.iter().find(|e| e.slug == "sampleford").unwrap();
        // The sitting division of the name in the seat's own state.
        assert_eq!(sampleford.established.as_deref(), Some("1922-01-02"));
        let history = sampleford.history.as_ref().expect("history");
        let rows: Vec<(&str, &str, Option<&str>)> = history
            .iter()
            .map(|h| {
                (
                    h.event.as_str(),
                    h.member.as_str(),
                    h.person_slug.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("31496", "Alex Paterson", Some("alex-paterson")),
                ("27966", "Casey Doe", None),
                ("12246", "Casey Doe", None)
            ]
        );
        assert!(
            history[0].tcp_pct.is_none(),
            "no two-candidate count, no figure"
        );
        assert_eq!(history[2].tcp_pct.map(|p| p.0), Some(55.0));
        assert!(electorates
            .iter()
            .find(|e| e.slug == "placeholder-bay")
            .unwrap()
            .history
            .is_none());
    }

    #[tokio::test]
    async fn expenses_attach_by_name_then_seat_and_never_to_a_guess() {
        let store = seeded("expenses").await;
        let quarter = |period: &str, label: &str, rows: &str| {
            format!(
                r#"{{"period":"{period}","label":"{label}","sourceUrl":"u","lastModified":"m",
                    "parliamentarians":[{rows}]}}"#
            )
        };
        // Alex under a formal first name but the right seat; Rossi under
        // initials in the right state; a namesake senator in another state.
        let rows = r#"
            {"officeCode":"PATA","name":"Alexander PATERSON MP","firstName":"Alexander",
             "surname":"PATERSON","electorate":"SAMPLEFORD","state":"VIC",
             "categories":[["Office Facilities",1000050],["Travel Allowance",-50]]},
            {"officeCode":"ROSM","name":"Senator M ROSSI","firstName":"M","surname":"ROSSI",
             "state":"TAS","categories":[["Office Administration",2500]]},
            {"officeCode":"ROSS","name":"Senator Sam ROSSI","firstName":"Sam","surname":"ROSSI",
             "state":"NSW","categories":[["Office Administration",9900]]}"#;
        put(
            &store,
            "canonical/ipea/2026Q01.json",
            &quarter("2026Q01", "Jan-Mar 2026", rows),
        )
        .await;
        put(
            &store,
            "canonical/ipea/2026Q02.json",
            &quarter("2026Q02", "Apr-Jun 2026", rows),
        )
        .await;
        derive(&store).await.expect("derive");

        let people: Vec<Person> = lines(
            &store
                .get_raw("bundles/people.jsonl")
                .await
                .unwrap()
                .unwrap(),
        );
        let alex = people.iter().find(|p| p.slug == "alex-paterson").unwrap();
        let expenses = alex.expenses.as_ref().expect("matched on seat");
        assert_eq!(
            expenses
                .iter()
                .map(|q| q.period.as_str())
                .collect::<Vec<_>>(),
            ["2026Q02", "2026Q01"],
            "newest first"
        );
        assert_eq!(
            expenses[0].total.0, 10000.0,
            "cents summed, then to dollars"
        );
        assert_eq!(expenses[0].categories[0].amount.0, 10000.5);
        assert_eq!(expenses[0].categories[1].amount.0, -0.5);

        let rossi = people.iter().find(|p| p.slug == "morgan-rossi").unwrap();
        let expenses = rossi
            .expenses
            .as_ref()
            .expect("matched on surname and state");
        assert_eq!(
            expenses[0].total.0, 25.0,
            "the NSW namesake is not added in"
        );
    }

    #[tokio::test]
    async fn eligibility_and_seats_follow_each_member_s_own_term() {
        let store = Store::Local(LocalStore::new(scratch("terms")));

        // Sitting throughout, in a seat of their own.
        put(
            &store,
            "canonical/people/alex-paterson.json",
            r#"{
            "slug":"alex-paterson","name":"Alex Paterson","house":"representatives",
            "electorate":"placeholder-bay","group":"Example Party","groupSlug":"example-party",
            "since":"2022-05-21","ids":{},"links":{}}"#,
        )
        .await;
        // Left mid-parliament; the seat went to a by-election.
        put(
            &store,
            "canonical/people/casey-obrien.json",
            r#"{
            "slug":"casey-obrien","name":"Casey O'Brien","house":"representatives",
            "electorate":"sampleford","group":"Retired Party","groupSlug":"retired-party",
            "since":"2022-05-21","until":"2026-03-14","ids":{},"links":{}}"#,
        )
        .await;
        // Won that by-election, so the earlier division was never theirs to vote in.
        put(
            &store,
            "canonical/people/dana-brooks.json",
            r#"{
            "slug":"dana-brooks","name":"Dana Brooks","house":"representatives",
            "electorate":"sampleford","group":"Example Party","groupSlug":"example-party",
            "since":"2026-05-09","ids":{},"links":{}}"#,
        )
        .await;
        put(
            &store,
            "canonical/electorates/sampleford.json",
            r#"{"slug":"sampleford","name":"Sampleford","state":"VIC"}"#,
        )
        .await;
        put(
            &store,
            "canonical/electorates/placeholder-bay.json",
            r#"{"slug":"placeholder-bay","name":"Placeholder Bay","state":"NSW"}"#,
        )
        .await;
        put(
            &store,
            "canonical/divisions/a.json",
            r#"{
            "id":"representatives/2026-02-10/1","house":"representatives","date":"2026-02-10",
            "number":1,"name":"Motions - Before the by-election","result":"passed",
            "ayes":2,"noes":0,"links":{},
            "votes":[{"personSlug":"alex-paterson","name":"Alex Paterson","vote":"aye"},
                     {"personSlug":"casey-obrien","name":"Casey O'Brien","vote":"aye"}]}"#,
        )
        .await;
        put(
            &store,
            "canonical/divisions/b.json",
            r#"{
            "id":"representatives/2026-06-15/1","house":"representatives","date":"2026-06-15",
            "number":1,"name":"Motions - After the by-election","result":"passed",
            "ayes":2,"noes":0,"links":{},
            "votes":[{"personSlug":"alex-paterson","name":"Alex Paterson","vote":"aye"},
                     {"personSlug":"dana-brooks","name":"Dana Brooks","vote":"aye"}]}"#,
        )
        .await;

        derive(&store).await.expect("derive");

        let people: Vec<Person> = lines(
            &store
                .get_raw("bundles/people.jsonl")
                .await
                .unwrap()
                .unwrap(),
        );
        let eligible = |slug: &str| {
            people
                .iter()
                .find(|p| p.slug == slug)
                .and_then(|p| p.stats.as_ref())
                .map(|s| (s.divisions_eligible, s.divisions_voted))
                .expect("stats")
        };
        assert_eq!(eligible("alex-paterson"), (2, 2), "sat for both divisions");
        assert_eq!(
            eligible("casey-obrien"),
            (1, 1),
            "a division held after they left was never theirs to vote in"
        );
        assert_eq!(
            eligible("dana-brooks"),
            (1, 1),
            "a division held before they arrived is not a missed vote"
        );

        // The seat belongs to whoever holds it now.
        let electorates: Vec<Electorate> = lines(
            &store
                .get_raw("bundles/electorates.jsonl")
                .await
                .unwrap()
                .unwrap(),
        );
        let sampleford = electorates
            .iter()
            .find(|e| e.slug == "sampleford")
            .expect("sampleford");
        assert_eq!(sampleford.member_slug.as_deref(), Some("dana-brooks"));

        // Seat counts and party pages describe the parliament as it stands.
        let parties: Vec<Party> = lines(
            &store
                .get_raw("bundles/parties.jsonl")
                .await
                .unwrap()
                .unwrap(),
        );
        assert_eq!(
            parties.iter().map(|p| p.slug.as_str()).collect::<Vec<_>>(),
            vec!["example-party"],
            "a group only a former member belonged to holds no seats"
        );
        assert_eq!(
            parties[0].seats.as_ref().map(|s| s.representatives),
            Some(2)
        );
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
