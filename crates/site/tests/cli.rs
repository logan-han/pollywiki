//! The site binary's front door: the flags it takes, the environment it reads
//! and what it does when the bundles are not there. The build itself is
//! covered in depth by the render tests; this is the layer above them, which
//! ends in `process::exit` and so cannot be reached from a unit test.

use std::path::PathBuf;
use std::process::{Command, Output};

fn sample_bundles() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/sample/bundles")
        .canonicalize()
        .expect("sample bundles are committed next to the crate")
}

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/site-cli-tests")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn run(args: &[&str], bundles: &PathBuf, site_url: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pollywiki-site"))
        .args(args)
        .env("BUNDLES_DIR", bundles)
        .env("SITE_URL", site_url)
        .output()
        .expect("the site binary runs")
}

#[test]
fn help_prints_the_usage_without_building() {
    let out = scratch("help");
    let output = run(
        &["--help", "--out", &out.to_string_lossy()],
        &sample_bundles(),
        "https://pollywiki.test",
    );
    assert_eq!(output.status.code(), Some(0));
    let usage = String::from_utf8_lossy(&output.stdout);
    assert!(usage.starts_with("usage: pollywiki-site"), "got {usage}");
    assert!(
        usage.contains("BUNDLES_DIR"),
        "the environment is documented"
    );
    assert!(!out.exists(), "--help builds nothing");
}

#[test]
fn a_build_reads_the_bundles_and_origin_from_the_environment() {
    let out = scratch("build");
    let output = run(
        &["--out", &out.to_string_lossy()],
        &sample_bundles(),
        "https://cli.example",
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // SITE_URL is what the canonical links and the sitemap are written against.
    let home = std::fs::read_to_string(out.join("index.html")).expect("home page");
    assert!(home.contains("<link rel=\"canonical\" href=\"https://cli.example/\">"));
    let sitemap = std::fs::read_to_string(out.join("sitemap-index.xml")).expect("sitemap index");
    assert!(sitemap.contains("https://cli.example/sitemap-0.xml"));
    assert!(out.join("404.html").is_file());
}

#[test]
fn an_unreadable_bundle_fails_the_build_loudly() {
    let out = scratch("broken-out");
    let bundles = scratch("broken-bundles");
    std::fs::create_dir_all(&bundles).expect("bundle dir");
    std::fs::write(bundles.join("people.jsonl"), "{ not json }\n").expect("broken bundle");

    let output = run(
        &["--out", &out.to_string_lossy()],
        &bundles,
        "https://pollywiki.test",
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("build failed:"), "got {stderr}");
    assert!(
        stderr.contains("loading bundles from") && stderr.contains("parsing people.jsonl"),
        "the directory and the file are both named: {stderr}"
    );
}

#[test]
fn an_empty_bundle_directory_still_builds_a_site() {
    let out = scratch("empty-out");
    let bundles = scratch("empty-bundles");
    std::fs::create_dir_all(&bundles).expect("bundle dir");

    let output = run(
        &["--out", &out.to_string_lossy()],
        &bundles,
        "https://pollywiki.test",
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // No meta.json means the build knows it is not showing the real record.
    let home = std::fs::read_to_string(out.join("index.html")).expect("home page");
    assert!(home.contains("Sample data only."));
}
