//! The binary's own front door: argument dispatch and exit codes. These run
//! the built binary, because `main` is the one layer a unit test cannot reach
//! -- it ends in `process::exit`.
//!
//! Only commands that touch no network are run: `derive` over an empty scratch
//! store, and the two argument errors.

use std::path::PathBuf;
use std::process::{Command, Output};

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/cli-tests")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// The binary with its store pointed at `dir` and the optional integrations
/// switched off, so the run is decided by the arguments alone.
fn run(dir: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pollywiki-ingest"))
        .args(args)
        .env("POLLYWIKI_STORE_DIR", dir)
        .env_remove("POLLYWIKI_DATA_BUCKET")
        .env_remove("GEMINI_API_KEY")
        .env_remove("TVFY_API_KEY")
        .output()
        .expect("the ingest binary runs")
}

#[test]
fn no_command_prints_the_usage_and_exits_two() {
    let dir = scratch("usage");
    let output = run(&dir, &[]);
    assert_eq!(output.status.code(), Some(2));
    let usage = String::from_utf8_lossy(&output.stderr);
    assert!(usage.starts_with("usage: pollywiki-ingest"), "got {usage}");
    assert!(usage.contains("--store local|s3"), "the flags are listed");
}

#[test]
fn an_unknown_command_is_refused_rather_than_guessed() {
    let dir = scratch("unknown");
    let output = run(&dir, &["syncc"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).starts_with("usage: pollywiki-ingest"));
}

#[test]
fn derive_builds_the_bundles_and_exits_zero() {
    let dir = scratch("derive");
    let output = run(&dir, &["derive", "--store", "local"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        dir.join("bundles/people.jsonl").is_file(),
        "derive writes its bundles under the store directory"
    );
}

#[test]
fn a_failed_run_reports_the_reason_and_exits_one() {
    let dir = scratch("failure");
    // summarise is the one command that fails on configuration alone.
    let output = run(&dir, &["summarise"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("GEMINI_API_KEY not set"),
        "the reason is printed: {stderr}"
    );
    assert!(
        stderr.contains("1 source(s) failed"),
        "and the tally: {stderr}"
    );
}
