mod derive;
mod endpoints;
mod http;
mod js_url;
mod manifest;
mod sources;
mod store;
mod summarise;
#[cfg(test)]
mod test_http;

use anyhow::Result;
use endpoints::Endpoints;
use manifest::record_sync;
use pollywiki_schema::Person;
use store::{LocalStore, S3Store, Store};

const USAGE: &str = "usage: pollywiki-ingest <sync|summarise|derive|all> [options]
  --store local|s3       default local (.store/); s3 needs POLLYWIKI_DATA_BUCKET
  --sources a,b,c        default wikidata,aec-profiles; also: aph,tvfy,handbook,aec
  --event <ids>          AEC event id(s), comma-separated (default 31496)
  --rebuild              tvfy only: re-normalise from cached raw, no API calls
summarise needs GEMINI_API_KEY; 'all' runs it between sync and derive when set.
";

/// new Date().toISOString(): UTC with millisecond precision.
pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Hand-curated reference data checked into the repository.
pub fn reference_path(name: &str) -> std::path::PathBuf {
    let dir =
        std::env::var("POLLYWIKI_REFERENCE_DIR").unwrap_or_else(|_| "data/reference".to_string());
    std::path::Path::new(&dir).join(name)
}

/// Opening date of the newest parliament in the reference data. Division
/// records begin there, so it bounds how far back departed members are kept.
/// Without the file the cutoff sits in the future, which keeps only sitting
/// members rather than pulling in every member since Federation.
pub fn records_begin() -> String {
    records_begin_from(&reference_path("parliaments.json"))
}

/// Split out so the fallback and the happy path are both reachable from a
/// test without moving the process's working directory.
fn records_begin_from(path: &std::path::Path) -> String {
    #[derive(serde::Deserialize)]
    struct Entry {
        opened: String,
    }
    let parsed: Option<indexmap::IndexMap<String, Entry>> = std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok());
    match parsed.and_then(|p| p.into_values().map(|e| e.opened).max()) {
        Some(opened) => opened,
        None => {
            eprintln!(
                "ingest: data/reference/parliaments.json unreadable, keeping sitting members only"
            );
            "9999-01-01".to_string()
        }
    }
}

struct Options {
    store: String,
    sources: Vec<String>,
    event: String,
    rebuild: bool,
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first().map(String::as_str) else {
        eprint!("{USAGE}");
        std::process::exit(2);
    };
    if !["sync", "summarise", "derive", "all"].contains(&command) {
        eprint!("{USAGE}");
        std::process::exit(2);
    }
    let options = parse_options(&args[1..]);

    let result = run(command, &options).await;
    match result {
        Ok(()) => {}
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}

async fn run(command: &str, options: &Options) -> Result<()> {
    let store = make_store(&options.store).await?;
    run_on(command, options, &store, &Endpoints::default()).await
}

/// The command dispatch itself, against a store and endpoints the caller
/// supplies. Tests drive this with a scratch directory and a local server
/// rather than whatever `--store` and the live hosts would resolve to.
async fn run_on(
    command: &str,
    options: &Options,
    store: &Store,
    endpoints: &Endpoints,
) -> Result<()> {
    let mut failures = 0;

    if command == "sync" || command == "all" {
        failures = sync(
            store,
            &options.sources,
            &options.event,
            options.rebuild,
            endpoints,
        )
        .await?;
    }
    if command == "summarise" || (command == "all" && std::env::var("GEMINI_API_KEY").is_ok()) {
        let people = load_people(store).await?;
        if let Err(err) = summarise::summarise(store, &people).await {
            eprintln!("summarise: FAILED - {err}");
            // The AI layer is an enhancement: inside `all` its failure must never
            // block record updates from deploying. Pending items resume next run.
            if command == "summarise" {
                failures += 1;
            }
        }
    }
    if command == "derive" || command == "all" {
        derive::derive(store).await?;
    }
    if failures > 0 {
        anyhow::bail!("{failures} source(s) failed");
    }
    Ok(())
}

async fn sync(
    store: &Store,
    sources: &[String],
    event: &str,
    rebuild: bool,
    endpoints: &Endpoints,
) -> Result<usize> {
    let mut people: Vec<Person> = Vec::new();
    let mut failures = 0;

    macro_rules! run_source {
        ($name:expr, $fut:expr) => {
            match $fut.await {
                Ok(()) => {
                    record_sync(store, $name, true, None).await?;
                    println!("{}: ok", $name);
                }
                Err(err) => {
                    failures += 1;
                    let note = err.to_string();
                    record_sync(store, $name, false, Some(&note)).await?;
                    eprintln!("{}: FAILED - {note}", $name);
                }
            }
        };
    }

    let has = |name: &str| sources.iter().any(|s| s == name);

    if has("wikidata") {
        match sources::wikidata::sync_wikidata(store, endpoints).await {
            Ok(result) => {
                people = result;
                record_sync(store, "wikidata", true, None).await?;
                println!("wikidata: ok");
            }
            Err(err) => {
                failures += 1;
                let note = err.to_string();
                record_sync(store, "wikidata", false, Some(&note)).await?;
                eprintln!("wikidata: FAILED - {note}");
            }
        }
    }
    if has("aec") {
        for id in event.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            run_source!("aec", sources::aec::sync_aec(store, id, endpoints));
        }
    }
    if has("aph") {
        run_source!(
            "aph-bills",
            sources::aph_bills::sync_aph_bills(store, 48, endpoints)
        );
    }
    if has("handbook") {
        if people.is_empty() {
            people = load_people(store).await?;
        }
        run_source!(
            "handbook",
            sources::handbook::sync_handbook(store, &mut people, endpoints)
        );
    }
    if has("aec-profiles") {
        run_source!(
            "aec-profiles",
            sources::aec_profiles::sync_aec_profiles(store, "31496", endpoints)
        );
    }
    if has("tvfy") {
        if std::env::var("TVFY_API_KEY").is_ok() || rebuild {
            if people.is_empty() {
                people = load_people(store).await?;
            }
            run_source!(
                "tvfy",
                sources::tvfy::sync_tvfy(store, &mut people, rebuild, endpoints)
            );
        } else {
            println!("tvfy: skipped (TVFY_API_KEY not set)");
        }
    }
    Ok(failures)
}

async fn load_people(store: &Store) -> Result<Vec<Person>> {
    let mut people = Vec::new();
    for key in store.list("canonical/people/").await? {
        if let Some(person) = store.get_json::<Person>(&key).await? {
            people.push(person);
        }
    }
    Ok(people)
}

async fn make_store(kind: &str) -> Result<Store> {
    if kind == "s3" {
        let bucket = std::env::var("POLLYWIKI_DATA_BUCKET")
            .map_err(|_| anyhow::anyhow!("POLLYWIKI_DATA_BUCKET must be set for --store s3"))?;
        return Ok(Store::S3(S3Store::new(&bucket).await?));
    }
    Ok(Store::Local(LocalStore::new(
        std::env::var("POLLYWIKI_STORE_DIR").unwrap_or_else(|_| ".store".to_string()),
    )))
}

fn parse_options(args: &[String]) -> Options {
    let get = |flag: &str| -> Option<String> {
        args.iter()
            .position(|a| a == &format!("--{flag}"))
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    Options {
        store: get("store").unwrap_or_else(|| "local".to_string()),
        sources: get("sources")
            .unwrap_or_else(|| "wikidata,aec-profiles".to_string())
            .split(',')
            .map(|s| s.trim().to_string())
            .collect(),
        event: get("event").unwrap_or_else(|| "31496".to_string()),
        rebuild: args.iter().any(|a| a == "--rebuild"),
    }
}

#[cfg(test)]
mod orchestration_tests {
    //! The dispatch layer itself: which sources a run reaches, what it does
    //! with a failure, and which commands each verb sets going. Every source
    //! is pointed at a local server, so a run here touches no network.

    use super::*;
    use crate::manifest::read_manifest;
    use crate::store::LocalStore;
    use crate::test_http::{Response, TestServer};
    use std::path::PathBuf;

    const ALL_SOURCES: [&str; 6] = ["wikidata", "aec", "aph", "handbook", "aec-profiles", "tvfy"];

    fn new_store(name: &str) -> Store {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/orchestration-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Store::Local(LocalStore::new(dir))
    }

    fn options(sources: &[&str], rebuild: bool) -> Options {
        Options {
            store: "local".to_string(),
            sources: sources.iter().map(|s| s.to_string()).collect(),
            event: "31496".to_string(),
            rebuild,
        }
    }

    /// Answers nothing, so every source fails on its first request.
    fn dead_server() -> TestServer {
        TestServer::start(|_| Response::status(404, "no such source"))
    }

    #[test]
    fn the_cutoff_is_the_newest_parliament_and_the_future_when_unreadable() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/records-begin");
        std::fs::create_dir_all(&dir).expect("scratch dir");

        let path = dir.join("parliaments.json");
        std::fs::write(
            &path,
            r#"{"47":{"opened":"2022-07-26"},"48":{"opened":"2025-07-22"}}"#,
        )
        .expect("fixture");
        assert_eq!(records_begin_from(&path), "2025-07-22");

        // A file that is missing, or there but not a parliament map, keeps the
        // cutoff in the future so only sitting members survive the filter.
        assert_eq!(records_begin_from(&dir.join("absent.json")), "9999-01-01");
        std::fs::write(&path, "not json").expect("fixture");
        assert_eq!(records_begin_from(&path), "9999-01-01");
    }

    #[tokio::test]
    async fn every_named_source_runs_and_each_failure_is_counted_and_recorded() {
        let server = dead_server();
        let store = new_store("failures");
        // --rebuild reaches tvfy without an API key, so all six run.
        let failures = sync(
            &store,
            &options(&ALL_SOURCES, true).sources,
            "31496",
            true,
            &Endpoints::at(&server.base),
        )
        .await
        .expect("a source failure is not a run failure");
        assert_eq!(failures, 6, "one per source");

        let manifest = read_manifest(&store).await.expect("manifest");
        let mut names: Vec<&String> = manifest.sources.keys().collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "aec",
                "aec-profiles",
                "aph-bills",
                "handbook",
                "tvfy",
                "wikidata"
            ],
            "every source leaves a record of its attempt"
        );
        for (name, status) in &manifest.sources {
            assert!(!status.ok, "{name} should be marked failed");
            assert!(status.note.is_some(), "{name} should carry the reason");
        }
    }

    #[tokio::test]
    async fn one_aec_run_happens_per_event_id() {
        let server = dead_server();
        let store = new_store("events");
        let failures = sync(
            &store,
            &["aec".to_string()],
            "31496, 31633 ,",
            false,
            &Endpoints::at(&server.base),
        )
        .await
        .expect("sync");
        // Two ids, one empty entry dropped; both are attempted separately.
        assert_eq!(failures, 2);
    }

    #[tokio::test]
    async fn tvfy_is_skipped_rather_than_failed_without_a_key() {
        if std::env::var("TVFY_API_KEY").is_ok() {
            eprintln!("TVFY_API_KEY set; skipping the skip test");
            return;
        }
        let server = dead_server();
        let store = new_store("tvfy-skip");
        let failures = sync(
            &store,
            &["tvfy".to_string()],
            "31496",
            false,
            &Endpoints::at(&server.base),
        )
        .await
        .expect("sync");
        assert_eq!(failures, 0, "a skip is not a failure");
        assert_eq!(server.hits(), 0, "nothing was fetched");
        assert!(
            read_manifest(&store)
                .await
                .expect("manifest")
                .sources
                .is_empty(),
            "a skipped source leaves no sync record"
        );
    }

    /// Answers the members query and the electorate profile scrape, so
    /// wikidata and aec-profiles both succeed; anything else is a miss.
    fn working_server() -> TestServer {
        TestServer::start(|req| {
            if req.path.starts_with("/sparql") {
                if req.path.contains("P571") || req.path.contains("inception") {
                    return Response::json(
                        serde_json::json!({ "results": { "bindings": [] } }).to_string(),
                    );
                }
                return Response::json(
                    serde_json::json!({ "results": { "bindings": [{
                        "person": { "value": "http://www.wikidata.org/entity/Q1" },
                        "personLabel": { "value": "Alex Paterson" },
                        "houseQ": { "value": "http://www.wikidata.org/entity/Q18912794" },
                        "electorateLabel": { "value": "Sampleford" },
                        "partyLabel": { "value": "Example Party" }
                    }] } })
                    .to_string(),
                );
            }
            if req.path.contains("GeneralEnrolmentByDivisionDownload") {
                return Response::text(
                    "Enrolment as at some date\nStateAb,DivisionID,DivisionNm,Enrolment\nVIC,101,Sampleford,118432",
                );
            }
            if req.path.starts_with("/aec/profiles/") {
                return Response::html(
                    "<dl><dt>Area:</dt><dd>52 sq km</dd><dt>Demographic rating:</dt>\
                     <dd>Inner Metropolitan</dd></dl>",
                );
            }
            Response::status(404, "unexpected path")
        })
    }

    #[tokio::test]
    async fn a_source_that_succeeds_is_recorded_ok_and_its_people_are_reused() {
        let server = working_server();
        let store = new_store("success");
        let electorate: pollywiki_schema::Electorate =
            serde_json::from_str(r#"{"slug":"sampleford","name":"Sampleford","state":"VIC"}"#)
                .expect("electorate fixture");
        store
            .put_json("canonical/electorates/sampleford.json", &electorate)
            .await
            .expect("seed");

        // handbook sits between the two so it sees the list wikidata returned.
        let failures = sync(
            &store,
            &[
                "wikidata".to_string(),
                "handbook".to_string(),
                "aec-profiles".to_string(),
            ],
            "31496",
            false,
            &Endpoints::at(&server.base),
        )
        .await
        .expect("sync");
        assert_eq!(failures, 1, "only the handbook had nothing to answer it");

        let manifest = read_manifest(&store).await.expect("manifest");
        assert!(manifest.sources["wikidata"].ok);
        assert!(manifest.sources["aec-profiles"].ok);
        assert!(manifest.sources["wikidata"].note.is_none());
        assert!(!manifest.sources["handbook"].ok);

        let people = load_people(&store).await.expect("people");
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].name, "Alex Paterson");
    }

    #[tokio::test]
    async fn sync_reports_its_failures_as_a_run_failure() {
        let server = dead_server();
        let store = new_store("run-sync");
        let err = run_on(
            "sync",
            &options(&["wikidata"], false),
            &store,
            &Endpoints::at(&server.base),
        )
        .await
        .expect_err("a failed source fails the run");
        assert_eq!(err.to_string(), "1 source(s) failed");
    }

    #[tokio::test]
    async fn derive_runs_on_its_own_and_as_part_of_all() {
        let server = dead_server();
        let store = new_store("run-derive");
        let endpoints = Endpoints::at(&server.base);

        run_on("derive", &options(&[], false), &store, &endpoints)
            .await
            .expect("derive over an empty store");
        assert!(
            store
                .get_raw("bundles/people.jsonl")
                .await
                .expect("read")
                .is_some(),
            "derive writes the bundles"
        );

        // 'all' syncs first: no sources named, so nothing fails and derive runs.
        run_on("all", &options(&[], false), &store, &endpoints)
            .await
            .expect("all over an empty store");
        assert_eq!(server.hits(), 0);
    }

    #[tokio::test]
    async fn summarise_without_a_key_fails_its_own_command_but_not_all() {
        if std::env::var("GEMINI_API_KEY").is_ok() {
            eprintln!("GEMINI_API_KEY set; skipping the no-key test");
            return;
        }
        let server = dead_server();
        let store = new_store("run-summarise");
        let endpoints = Endpoints::at(&server.base);

        let err = run_on("summarise", &options(&[], false), &store, &endpoints)
            .await
            .expect_err("no key is a failure for the summarise command");
        assert_eq!(err.to_string(), "1 source(s) failed");

        // Inside 'all' the AI layer is optional, so the same missing key is
        // not even reached: records still derive and deploy.
        run_on("all", &options(&[], false), &store, &endpoints)
            .await
            .expect("all tolerates a missing key");
    }

    #[tokio::test]
    async fn the_default_store_is_a_local_directory_and_s3_needs_a_bucket() {
        let Store::Local(_) = make_store("local").await.expect("local store") else {
            panic!("--store local must open the development directory");
        };
        // Anything unrecognised is local too, rather than a hard failure.
        let Store::Local(_) = make_store("").await.expect("default store") else {
            panic!("an unknown store must fall back to local");
        };
        if std::env::var("POLLYWIKI_DATA_BUCKET").is_ok() {
            eprintln!("POLLYWIKI_DATA_BUCKET set; skipping the missing-bucket test");
            return;
        }
        let message = match make_store("s3").await {
            Ok(_) => panic!("s3 without a bucket must fail"),
            Err(err) => err.to_string(),
        };
        assert_eq!(message, "POLLYWIKI_DATA_BUCKET must be set for --store s3");
    }
}

#[cfg(test)]
mod tests {
    use crate::parse_options;
    use crate::sources::aec::{to_candidates, AecRow};
    use crate::sources::tvfy::key_for;
    use pollywiki_schema::JsNum;

    fn row(pairs: &[(&str, &str)]) -> AecRow {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn aec_to_candidates_filters_computes_and_sorts() {
        let rows = vec![
            row(&[
                ("DivisionNm", "Bean"),
                ("GivenNm", "DAVID"),
                ("Surname", "SMITH"),
                ("PartyNm", "Australian Labor Party"),
                ("PartyAb", "ALP"),
                ("TotalVotes", "60000"),
                ("Swing", "1.25"),
                ("Elected", "Y"),
            ]),
            row(&[
                ("DivisionNm", "Bean"),
                ("GivenNm", "Jessie"),
                ("Surname", "PRICE"),
                ("PartyNm", "Independent"),
                ("PartyAb", "IND"),
                ("TotalVotes", "40000"),
                ("Swing", ""),
                ("Elected", "N"),
            ]),
            row(&[
                ("DivisionNm", "Fenner"),
                ("GivenNm", "Andrew"),
                ("Surname", "LEIGH"),
                ("PartyNm", "Australian Labor Party"),
                ("PartyAb", "ALP"),
                ("TotalVotes", "70000"),
                ("Swing", "2.0"),
                ("Elected", "Y"),
            ]),
        ];
        let candidates = to_candidates(&rows, "Bean");
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].name, "David Smith");
        assert_eq!(candidates[0].party, "Australian Labor Party");
        assert_eq!(candidates[0].votes, 60000);
        assert_eq!(candidates[0].pct, JsNum(60.0));
        assert_eq!(candidates[0].swing, Some(JsNum(1.25)));
        assert!(candidates[0].elected);
        assert_eq!(candidates[1].swing, None);
    }

    #[test]
    fn aec_title_cases_mc_prefixes() {
        let rows = vec![row(&[
            ("DivisionNm", "Bean"),
            ("GivenNm", "MICHAEL"),
            ("Surname", "MCCORMACK"),
            ("PartyNm", "Australian Labor Party"),
            ("PartyAb", "ALP"),
            ("TotalVotes", "60000"),
            ("Swing", "1.25"),
            ("Elected", "Y"),
        ])];
        let candidates = to_candidates(&rows, "Bean");
        assert_eq!(candidates[0].name, "Michael McCormack");
    }

    #[test]
    fn options_default_to_the_documented_values() {
        let options = parse_options(&[]);
        assert_eq!(options.store, "local");
        assert_eq!(options.sources, vec!["wikidata", "aec-profiles"]);
        assert_eq!(options.event, "31496");
        assert!(!options.rebuild);
    }

    #[test]
    fn options_read_flags_and_trim_the_source_list() {
        let args: Vec<String> = [
            "--store",
            "s3",
            "--sources",
            "aph, tvfy ,handbook",
            "--event",
            "31633",
            "--rebuild",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let options = parse_options(&args);
        assert_eq!(options.store, "s3");
        assert_eq!(options.sources, vec!["aph", "tvfy", "handbook"]);
        assert_eq!(options.event, "31633");
        assert!(options.rebuild);
    }

    #[test]
    fn a_flag_with_no_value_falls_back_to_the_default() {
        let options = parse_options(&["--event".to_string()]);
        assert_eq!(options.event, "31496");
    }

    #[test]
    fn tvfy_key_for_flattens_division_ids() {
        assert_eq!(
            key_for("representatives/2025-07-24/3"),
            "representatives-2025-07-24-3"
        );
    }

    #[test]
    fn dot_net_dates_apply_the_event_offset() {
        assert_eq!(
            crate::sources::aph_bills::dot_net_date(Some("/Date(1764075600000+1100)/")),
            Some("2025-11-26".to_string())
        );
        assert_eq!(crate::sources::aph_bills::dot_net_date(Some("nope")), None);
    }
}
