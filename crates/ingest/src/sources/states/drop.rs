//! Member lists downloaded by hand, for parliaments whose sites turn
//! automated readers away. Each download is filed under
//! data/reference/state-drops/<state>/ with the day it was downloaded at the
//! front of its name, e.g. 2026-10-30-legislative-assembly.csv. The newest
//! day's files are the list, and that day is when its members were listed.

use super::RollChamber;
use anyhow::{bail, Result};
use pollywiki_schema::StateCode;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A list this old has likely missed a change; the sync says so.
pub const STALE_AFTER_DAYS: i64 = 45;

/// One day's downloads for a parliament.
#[derive(Debug, PartialEq)]
pub struct Drop {
    /// The day the files were downloaded, as an ISO date.
    pub date: String,
    pub files: Vec<PathBuf>,
}

/// The newest day's files in a parliament's drop folder, or none when the
/// folder is missing or holds nothing dated.
pub fn newest(dir: &Path) -> Result<Option<Drop>> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(None);
    };
    let mut by_date: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for entry in entries {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let dated = name.get(..10).filter(|day| {
            name.as_bytes().get(10) == Some(&b'-')
                && chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").is_ok()
        });
        if let Some(day) = dated {
            by_date.entry(day.to_string()).or_default().push(path);
        }
    }
    Ok(by_date.into_iter().next_back().map(|(date, mut files)| {
        files.sort();
        Drop { date, files }
    }))
}

/// Whole days between a drop and `today`, or none for a date that does not
/// parse.
pub fn age_days(date: &str, today: chrono::NaiveDate) -> Option<i64> {
    let downloaded = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    Some((today - downloaded).num_days())
}

/// The roll a parliament's downloaded files hold. Each parliament's files
/// are its own format, so each has a reader of its own; `source` is the
/// official page they were downloaded from.
pub fn read(state: StateCode, drop: &Drop, source: &str) -> Result<Vec<RollChamber>> {
    if state == StateCode::NSW {
        return super::nsw::read(drop, source);
    }
    let names: Vec<String> = drop
        .files
        .iter()
        .filter_map(|f| f.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    bail!(
        "no reader yet for {state}'s downloaded files ({})",
        names.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str, files: &[&str]) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/state-drops-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        for file in files {
            std::fs::write(dir.join(file), "x").expect("fixture");
        }
        dir
    }

    #[test]
    fn the_newest_day_s_files_are_the_list() {
        let dir = folder(
            "newest",
            &[
                "2026-09-28-legislative-assembly.csv",
                "2026-10-30-legislative-council.csv",
                "2026-10-30-legislative-assembly.csv",
                "README.md",
                "2026-13-01-not-a-date.csv",
                "2026-10-31.csv",
            ],
        );
        let drop = newest(&dir).expect("read").expect("a drop");
        assert_eq!(drop.date, "2026-10-30");
        let names: Vec<String> = drop
            .files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            [
                "2026-10-30-legislative-assembly.csv",
                "2026-10-30-legislative-council.csv"
            ],
            "undated and misdated files are not a list"
        );

        assert!(newest(&dir.join("absent")).expect("read").is_none());
        assert!(newest(&folder("empty", &["notes.txt"]))
            .expect("read")
            .is_none());
    }

    #[test]
    fn a_drop_s_age_is_counted_in_days() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 12, 14).unwrap();
        assert_eq!(age_days("2026-10-30", today), Some(45));
        assert_eq!(age_days("2026-12-14", today), Some(0));
        assert_eq!(age_days("soon", today), None);
    }

    #[test]
    fn a_parliament_with_no_reader_says_which_files_it_could_not_read() {
        let drop = Drop {
            date: "2026-10-30".to_string(),
            files: vec![PathBuf::from("x/2026-10-30-members.pdf")],
        };
        let err = read(StateCode::NT, &drop, "https://example.org").unwrap_err();
        assert_eq!(
            err.to_string(),
            "no reader yet for NT's downloaded files (2026-10-30-members.pdf)"
        );
    }
}
