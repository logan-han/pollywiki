//! Queensland. The Parliament's website and its open data API turn away
//! automated readers, but its documents host serves the members' mailing
//! list: a Word table of every member's name, electorate, portfolio and
//! party, kept current by the Parliament. A member with two electorate
//! offices is listed once per office.

use super::{split_offices, RollChamber, RollMember};
use crate::endpoints::Endpoints;
use crate::http::fetch_bytes;
use crate::store::Store;
use anyhow::{anyhow, bail, Result};
use regex::Regex;
use std::io::Read;
use std::sync::LazyLock;

const FILE: &str = "/Members/mailingLists/MEMMERGE.docx";

pub async fn read(store: &Store, endpoints: &Endpoints) -> Result<Vec<RollChamber>> {
    let source = format!("{}{FILE}", endpoints.qld_documents);
    let archive = fetch_bytes(&source, &endpoints.opts(1000)).await?;
    store
        .put_raw("raw/states/qld/MEMMERGE.docx", &archive)
        .await?;
    let rows = docx_rows(&archive)?;
    let Some((head, body)) = rows.split_first() else {
        bail!("the members file has no table");
    };
    let column = |name: &str| {
        head.iter()
            .position(|h| h.eq_ignore_ascii_case(name))
            .ok_or_else(|| anyhow!("the members file has no {name} column"))
    };
    let (first, last, electorate, portfolio, party) = (
        column("First")?,
        column("Last")?,
        column("Electorate")?,
        column("Portfolio")?,
        column("Party")?,
    );
    let cell = |row: &[String], i: usize| row.get(i).map(|c| c.trim()).unwrap_or("").to_string();

    let mut members: Vec<RollMember> = Vec::new();
    for row in body {
        let surname = cell(row, last);
        let name = format!(
            "{} {}",
            cell(row, first),
            surname.strip_suffix(" MP").unwrap_or(&surname)
        );
        let seat = cell(row, electorate);
        let seat = seat
            .strip_prefix("Member for ")
            .unwrap_or(&seat)
            .to_string();
        if name.trim().is_empty() {
            continue;
        }
        // Once per member, however many electorate offices they keep.
        if members
            .iter()
            .any(|m| m.name == name && m.seat.as_deref() == Some(seat.as_str()))
        {
            continue;
        }
        members.push(RollMember {
            name,
            party: cell(row, party),
            seat: Some(seat).filter(|s| !s.is_empty()),
            since: None,
            offices: split_offices(&cell(row, portfolio)),
            profile: None,
        });
    }
    Ok(vec![RollChamber {
        slug: "legislative-assembly".to_string(),
        source,
        members,
    }])
}

/// The rows of a Word document's tables, each row as its cells' text.
fn docx_rows(archive: &[u8]) -> Result<Vec<Vec<String>>> {
    static ROW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<w:tr[ >].*?</w:tr>").unwrap());
    static CELL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)<w:tc[ >].*?</w:tc>").unwrap());
    // Text runs, and the breaks, tabs and paragraph ends that separate words.
    static PIECE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)<w:t(?:\s[^>]*)?>(.*?)</w:t>|<w:br\s*/>|<w:tab\s*/>|</w:p>").unwrap()
    });
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive))
        .map_err(|e| anyhow!("the members file is not a Word document ({e})"))?;
    let mut xml = String::new();
    zip.by_name("word/document.xml")
        .map_err(|_| anyhow!("the members file has no document body"))?
        .read_to_string(&mut xml)?;
    Ok(ROW
        .find_iter(&xml)
        .map(|row| {
            CELL.find_iter(row.as_str())
                .map(|cell| {
                    let mut text = String::new();
                    for piece in PIECE.captures_iter(cell.as_str()) {
                        match piece.get(1) {
                            Some(run) => text.push_str(run.as_str()),
                            None => text.push(' '),
                        }
                    }
                    let text = text
                        .replace("&amp;", "&")
                        .replace("&lt;", "<")
                        .replace("&gt;", ">")
                        .replace("&quot;", "\"")
                        .replace("&apos;", "'");
                    text.split_whitespace().collect::<Vec<_>>().join(" ")
                })
                .collect()
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::LocalStore;
    use crate::test_http::{Response, TestServer};
    use std::io::Write;

    fn docx(rows: &[[&str; 8]]) -> Vec<u8> {
        let cell = |text: &str| {
            let runs = text
                .split('\n')
                .map(|line| format!("<w:r><w:t xml:space=\"preserve\">{line}</w:t></w:r>"))
                .collect::<Vec<_>>()
                .join("<w:r><w:br/></w:r>");
            format!("<w:tc><w:tcPr><w:tcW w:w=\"1\"/></w:tcPr><w:p>{runs}</w:p></w:tc>")
        };
        let body: String = rows
            .iter()
            .map(|row| {
                format!(
                    "<w:tr w:rsidR=\"1\"><w:trPr/>{}</w:tr>",
                    row.iter().map(|c| cell(c)).collect::<String>()
                )
            })
            .collect();
        let xml = format!("<w:document><w:body><w:tbl>{body}</w:tbl></w:body></w:document>");
        let mut out = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut out);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zip.start_file("word/document.xml", options).expect("start");
            zip.write_all(xml.as_bytes()).expect("write");
            zip.finish().expect("finish");
        }
        out.into_inner()
    }

    #[tokio::test]
    async fn the_mailing_list_gives_each_member_once_with_their_offices() {
        let file = docx(&[
            [
                "Title",
                "First",
                "Last",
                "Electorate",
                "Portfolio",
                "Electorate Postal Address",
                "Salutation",
                "Party",
            ],
            [
                "Hon",
                "David",
                "Crisafulli MP",
                "Member for Broadwater",
                "Premier and Minister for Veterans",
                "PO Box 1\nPARADISE POINT QLD 4216",
                "Premier",
                "LNP",
            ],
            [
                "Mr",
                "Robbie",
                "Katter MP",
                "Member for Traeger",
                "",
                "Mount Isa office",
                "Mr Katter",
                "KAP",
            ],
            [
                "Mr",
                "Robbie",
                "Katter MP",
                "Member for Traeger",
                "",
                "Cloncurry office",
                "Mr Katter",
                "KAP",
            ],
            [
                "Ms",
                "Amy",
                "MacMahon MP",
                "Member for South Brisbane",
                "",
                "PO Box 2",
                "Ms MacMahon",
                "GRN",
            ],
        ]);
        let server = TestServer::start(move |req| match req.path.as_str() {
            "/qld-documents/Members/mailingLists/MEMMERGE.docx" => Response::bytes(
                file.clone(),
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            ),
            _ => Response::status(404, "unexpected path"),
        });
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/states-tests/qld");
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::Local(LocalStore::new(dir));

        let chambers = read(&store, &Endpoints::at(&server.base))
            .await
            .expect("read");
        let members = &chambers[0].members;
        let rows: Vec<(&str, &str, Option<&str>)> = members
            .iter()
            .map(|m| (m.name.as_str(), m.party.as_str(), m.seat.as_deref()))
            .collect();
        assert_eq!(
            rows,
            [
                ("David Crisafulli", "LNP", Some("Broadwater")),
                ("Robbie Katter", "KAP", Some("Traeger")),
                ("Amy MacMahon", "GRN", Some("South Brisbane"))
            ],
            "two offices, one member"
        );
        assert_eq!(members[0].offices, ["Premier", "Minister for Veterans"]);
        assert!(members[1].offices.is_empty());
        assert!(chambers[0]
            .source
            .ends_with("/Members/mailingLists/MEMMERGE.docx"));
    }
}
