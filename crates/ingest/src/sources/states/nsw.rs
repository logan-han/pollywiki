//! New South Wales. The Parliament's site turns automated readers away, so
//! its members' contact spreadsheets are downloaded by hand from its
//! downloadable lists page and filed as a drop. Each row names a member's
//! party, electorate and ministry, and the house they sit in shows in their
//! post-nominals, M.P. or M.L.C., so a file for either house, or for both,
//! reads the same way. Columns are found by their headings, so a layout
//! that moves them still reads, and one that drops them says which.

use super::drop::Drop;
use super::{split_offices, RollChamber, RollMember};
use anyhow::{anyhow, bail, Result};
use std::path::Path;

pub fn read(drop: &Drop, source: &str) -> Result<Vec<RollChamber>> {
    let files: Vec<&Path> = drop
        .files
        .iter()
        .map(|f| f.as_path())
        .filter(|f| f.extension().is_some_and(|e| e.eq_ignore_ascii_case("csv")))
        .collect();
    if files.is_empty() {
        bail!("no CSV among the files downloaded for NSW");
    }
    let mut assembly: Vec<RollMember> = Vec::new();
    let mut council: Vec<RollMember> = Vec::new();
    for file in files {
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let bytes = std::fs::read(file)?;
        let text = String::from_utf8_lossy(&bytes);
        let mut rows = csv::ReaderBuilder::new()
            .flexible(true)
            .from_reader(text.trim_start_matches('\u{feff}').as_bytes());
        let headings: Vec<String> = rows
            .headers()?
            .iter()
            .map(|h| h.trim().to_uppercase())
            .collect();
        let column = |names: &[&str]| headings.iter().position(|h| names.contains(&h.as_str()));
        let surname = column(&["SURNAME", "LAST NAME", "FAMILY NAME"])
            .ok_or_else(|| anyhow!("{name}: no SURNAME column"))?;
        let party = column(&["PARTY"]).ok_or_else(|| anyhow!("{name}: no PARTY column"))?;
        let given = column(&["PREFERRED NAME", "FIRST NAME", "GIVEN NAMES", "GIVEN NAME"]);
        let initials = column(&["INITIALS"]);
        let electorate = column(&["ELECTORATE"]);
        let ministry = column(&["MINISTRY"]);
        let post_nominals = column(&["POST-NOMINALS", "POSTNOMINALS"]);
        for row in rows.records() {
            let row = row?;
            let cell = |at: Option<usize>| at.and_then(|i| row.get(i)).map(str::trim).unwrap_or("");
            let family = cell(Some(surname));
            if family.is_empty() {
                continue;
            }
            let first = Some(cell(given))
                .filter(|g| !g.is_empty())
                .unwrap_or_else(|| cell(initials));
            let member = RollMember {
                name: format!("{first} {family}").trim().to_string(),
                party: cell(Some(party)).to_string(),
                seat: Some(cell(electorate).to_string()).filter(|s| !s.is_empty()),
                since: None,
                offices: split_offices(cell(ministry)),
                profile: None,
            };
            let house = match house_of(cell(post_nominals), &name) {
                Some(House::Council) => &mut council,
                Some(House::Assembly) => &mut assembly,
                None => bail!("{name}: no way to tell which house {} sits in", member.name),
            };
            if !house
                .iter()
                .any(|m| m.name == member.name && m.seat == member.seat)
            {
                house.push(member);
            }
        }
    }
    Ok(vec![
        RollChamber {
            slug: "legislative-assembly".to_string(),
            source: source.to_string(),
            members: assembly,
        },
        RollChamber {
            slug: "legislative-council".to_string(),
            source: source.to_string(),
            members: council,
        },
    ])
}

enum House {
    Assembly,
    Council,
}

/// The house a member sits in: M.L.C. after their name is the Council and
/// M.P. the Assembly; failing those, the file's own name.
fn house_of(post_nominals: &str, file: &str) -> Option<House> {
    let letters = post_nominals.to_uppercase().replace('.', "");
    let words: Vec<&str> = letters
        .split(|c: char| c == ',' || c.is_whitespace())
        .collect();
    if words.contains(&"MLC") {
        return Some(House::Council);
    }
    if words.contains(&"MP") {
        return Some(House::Assembly);
    }
    let file = file.to_lowercase();
    if file.contains("council") || file.contains("-lc") {
        Some(House::Council)
    } else if file.contains("assembly") || file.contains("-la") {
        Some(House::Assembly)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The header and rows as the Parliament's contact spreadsheet lays them
    /// out, extra trailing cells and all.
    const HEADER: &str = "INITIALS,SURNAME,TITLE,POST-NOMINALS,SALUTATION,PARTY,ELECTORATE,MINISTRY,CONTACT ADDRESS LINE1,CONTACT ADDRESS EMAIL";

    /// A drop of these files in a folder of its own, as tests run at once.
    fn drop_of(folder: &str, files: &[(&str, &str)]) -> Drop {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/states-tests/nsw")
            .join(folder);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let mut paths = Vec::new();
        for (name, body) in files {
            let path = dir.join(name);
            std::fs::write(&path, body).expect("fixture");
            paths.push(path);
        }
        Drop {
            date: "2026-10-30".to_string(),
            files: paths,
        }
    }

    #[test]
    fn each_row_finds_its_house_by_post_nominals() {
        let both = format!(
            "{HEADER}\n\
             R.S.,Amery,The Hon.,M.P.,Mr Amery,Australian Labor Party,Mount Druitt,,Office,mountdruitt@example.org,,,,Opposition Whip,\n\
             C.J.,Minns,The Hon.,M.P.,Mr Minns,Australian Labor Party,Kogarah,Premier and Minister for Western Sydney,Office,kogarah@example.org\n\
             J.,Ajaka,The Hon.,M.L.C.,Mr Ajaka,Liberal Party,,,Parliament House,john@example.org,,,,Parliamentary Secretary,\n\
             ,,,,,,,,,\n"
        );
        let drop = drop_of("both", &[("2026-10-30-members.csv", &both)]);
        let chambers = read(&drop, "https://example.org/lists").expect("read");
        let assembly = &chambers[0].members;
        let names: Vec<&str> = assembly.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names,
            ["R.S. Amery", "C.J. Minns"],
            "a blank row is no member"
        );
        assert_eq!(assembly[1].seat.as_deref(), Some("Kogarah"));
        assert_eq!(
            assembly[1].offices,
            ["Premier", "Minister for Western Sydney"]
        );
        assert_eq!(chambers[0].source, "https://example.org/lists");

        let council = &chambers[1].members;
        assert_eq!(council[0].name, "J. Ajaka");
        assert_eq!(council[0].party, "Liberal Party");
        assert!(council[0].seat.is_none(), "elected state-wide");
    }

    #[test]
    fn a_first_name_column_wins_over_initials_and_a_file_can_name_its_house() {
        let council = "INITIALS,FIRST NAME,SURNAME,PARTY,MINISTRY\n\
                       J.,John,Ajaka,Liberal Party,\n";
        let drop = drop_of(
            "council",
            &[("2026-10-30-legislative-council.csv", council)],
        );
        let chambers = read(&drop, "https://example.org").expect("read");
        assert_eq!(chambers[1].members[0].name, "John Ajaka");
        assert!(chambers[0].members.is_empty());
    }

    #[test]
    fn a_file_without_the_columns_it_needs_says_which() {
        let drop = drop_of(
            "columns",
            &[(
                "2026-10-30-members.csv",
                "NAME,ELECTORATE\nJane,Sampleford\n",
            )],
        );
        let err = read(&drop, "https://example.org").unwrap_err();
        assert_eq!(err.to_string(), "2026-10-30-members.csv: no SURNAME column");

        let drop = drop_of("pdf", &[("2026-10-30-members.pdf", "x")]);
        assert!(
            read(&drop, "https://example.org").is_err(),
            "no CSV, nothing to read"
        );
    }
}
