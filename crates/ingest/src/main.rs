mod derive;
mod http;
mod js_url;
mod manifest;
mod sources;
mod store;
mod summarise;

use anyhow::Result;
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
    let mut failures = 0;

    if command == "sync" || command == "all" {
        failures = sync(&store, &options.sources, &options.event, options.rebuild).await?;
    }
    if command == "summarise" || (command == "all" && std::env::var("GEMINI_API_KEY").is_ok()) {
        let people = load_people(&store).await?;
        if let Err(err) = summarise::summarise(&store, &people).await {
            eprintln!("summarise: FAILED - {err}");
            // The AI layer is an enhancement: inside `all` its failure must never
            // block record updates from deploying. Pending items resume next run.
            if command == "summarise" {
                failures += 1;
            }
        }
    }
    if command == "derive" || command == "all" {
        derive::derive(&store).await?;
    }
    if failures > 0 {
        anyhow::bail!("{failures} source(s) failed");
    }
    Ok(())
}

async fn sync(store: &Store, sources: &[String], event: &str, rebuild: bool) -> Result<usize> {
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
        match sources::wikidata::sync_wikidata(store).await {
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
            run_source!("aec", sources::aec::sync_aec(store, id));
        }
    }
    if has("aph") {
        run_source!("aph-bills", sources::aph_bills::sync_aph_bills(store, 48));
    }
    if has("handbook") {
        if people.is_empty() {
            people = load_people(store).await?;
        }
        run_source!(
            "handbook",
            sources::handbook::sync_handbook(store, &mut people)
        );
    }
    if has("aec-profiles") {
        run_source!(
            "aec-profiles",
            sources::aec_profiles::sync_aec_profiles(store, "31496")
        );
    }
    if has("tvfy") {
        if std::env::var("TVFY_API_KEY").is_ok() || rebuild {
            if people.is_empty() {
                people = load_people(store).await?;
            }
            run_source!(
                "tvfy",
                sources::tvfy::sync_tvfy(store, &mut people, rebuild)
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
