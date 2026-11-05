//! The opening of a federal bill's explanatory memorandum, read for each bill
//! APH has yet to summarise. Until the summary appears, the memorandum is the
//! official text a note on the bill is written from.
//!
//! ParlInfo, which keeps every memorandum, answers scripts as ParlWork does,
//! so the Word file is fetched as a browser too. Each memorandum is read once:
//! a stored one is kept until ParlWork links a different file.

use crate::endpoints::Endpoints;
use crate::http::fetch_bytes;
use crate::sources::aph_bills::{strip_html, whole_memorandum, ParlWorkDetail, BROWSER_UA};
use crate::store::Store;
use anyhow::{anyhow, Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::sync::LazyLock;

/// Characters of a memorandum a note is written from: its general outline,
/// and on a short memorandum the notes on clauses after it.
const OUTLINE_BUDGET: usize = 12_000;

/// The production host of the links ParlWork gives.
const PARLINFO: &str = "https://parlinfo.aph.gov.au";

/// One bill's memorandum as stored: the file it was read from, and its opening.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Memorandum {
    pub source: String,
    pub outline: String,
}

pub fn memorandum_key(id: &str) -> String {
    format!("canonical/memoranda/{id}.json")
}

/// Reads the memorandum of each bill named, from the Word file its stored
/// ParlWork detail links, unless that file has been read already. A file that
/// cannot be fetched or read is reported and tried again next run.
pub async fn sync_memoranda(store: &Store, endpoints: &Endpoints, ids: &[String]) -> Result<()> {
    let opts = endpoints.opts(2000).with_header("user-agent", BROWSER_UA);
    let mut read = 0;
    for id in ids {
        let Some(detail) = store
            .get_json::<ParlWorkDetail>(&format!("raw/aph/bill-{id}.json"))
            .await?
        else {
            continue;
        };
        let Some(word) = detail
            .bill
            .as_ref()
            .and_then(|bill| whole_memorandum(bill.memoranda.as_deref().unwrap_or_default()))
            .and_then(|memorandum| memorandum.word_link.clone())
            .filter(|link| !link.is_empty())
        else {
            continue;
        };
        let key = memorandum_key(id);
        let stored: Option<Memorandum> = store.get_json(&key).await?;
        if stored.is_some_and(|m| m.source == word) {
            continue;
        }
        let url = match word.strip_prefix(PARLINFO) {
            Some(path) => format!("{}{path}", endpoints.parlinfo),
            None => word.clone(),
        };
        match fetch_bytes(&url, &opts)
            .await
            .and_then(|docx| outline(&docx))
        {
            Ok(outline) => {
                store
                    .put_json(
                        &key,
                        &Memorandum {
                            source: word,
                            outline,
                        },
                    )
                    .await?;
                read += 1;
            }
            Err(err) => eprintln!("aph-bills: memorandum for {id} not read - {err}"),
        }
    }
    if read > 0 {
        println!("aph-bills: read {read} memoranda for bills APH has yet to summarise");
    }
    Ok(())
}

/// One paragraph of the document: its Word style and its text.
struct Paragraph {
    style: String,
    text: String,
}

impl Paragraph {
    /// A line of the table of contents, which names the outline too.
    fn in_contents(&self) -> bool {
        self.style.to_lowercase().starts_with("toc")
    }

    /// A heading by its style, or by its shape where a memorandum sets its
    /// headings by hand: a short line that does not end as a sentence does.
    fn is_heading(&self) -> bool {
        self.style.to_lowercase().contains("head")
            || (self.text.chars().count() <= 90 && !self.text.ends_with(['.', ',', ':', ';']))
    }

    /// The level of a "Heading N" style, the only styles every memorandum
    /// that uses levels shares.
    fn level(&self) -> Option<u32> {
        static LEVEL: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"(?i)^heading\s*(\d)$").unwrap());
        LEVEL
            .captures(&self.style)
            .and_then(|level| level[1].parse().ok())
    }
}

/// The memorandum's general outline, the part that says what the bill does,
/// and, while the budget lasts, what follows it: on a short memorandum, the
/// notes on each clause. Tables, and the outline's notes on dates, costs and
/// human rights, are left out, and the reading stops at the next part of the
/// same rank, at a chapter, or at the statement of compatibility.
pub fn outline(docx: &[u8]) -> Result<String> {
    static START: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^((general\s+)?outline|overview|purpose of the bill)\b").unwrap()
    });
    static STOP: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)^(statement of compatibility|attachment\b|regulation impact statement|acronyms|abbreviations|glossary)",
        )
        .unwrap()
    });
    static SKIP: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)^(date of effect|proposal announced|financial impact|impact analysis|human rights|compliance cost|commencement|consultation)",
        )
        .unwrap()
    });

    let paragraphs = paragraphs(&document_xml(docx)?);
    let start = paragraphs
        .iter()
        .position(|p| !p.in_contents() && p.text.chars().count() <= 60 && START.is_match(&p.text))
        .ok_or_else(|| anyhow!("the memorandum has no outline"))?;
    let rank = paragraphs[start].level();

    let mut kept: Vec<String> = Vec::new();
    let mut size = 0;
    let mut skipping = false;
    for p in &paragraphs[start + 1..] {
        let outranked = rank.is_some_and(|rank| p.level().is_some_and(|level| level <= rank));
        if outranked || p.style.to_lowercase().contains("chapter") {
            break;
        }
        if p.is_heading() {
            if STOP.is_match(&p.text) {
                break;
            }
            skipping = SKIP.is_match(&p.text);
            // A schedule's own "Outline" heading only repeats the part's name.
            if skipping || START.is_match(&p.text) {
                continue;
            }
        } else if skipping {
            continue;
        }
        let length = p.text.chars().count();
        if size + length > OUTLINE_BUDGET {
            if kept.is_empty() {
                kept.push(p.text.chars().take(OUTLINE_BUDGET).collect());
            }
            break;
        }
        size += length + 1;
        kept.push(p.text.clone());
    }
    if kept.is_empty() {
        return Err(anyhow!("the memorandum's outline is empty"));
    }
    Ok(kept.join("\n"))
}

/// The body of a Word file.
fn document_xml(docx: &[u8]) -> Result<String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(docx))
        .map_err(|e| anyhow!("the memorandum is not a Word document ({e})"))?;
    let mut xml = String::new();
    zip.by_name("word/document.xml")
        .map_err(|_| anyhow!("the memorandum has no document body"))?
        .read_to_string(&mut xml)
        .context("reading the memorandum")?;
    Ok(xml)
}

/// The document's paragraphs outside its tables, in order, each with its
/// style. Empty paragraphs are dropped.
fn paragraphs(xml: &str) -> Vec<Paragraph> {
    static TABLE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)<w:tbl[ >].*?</w:tbl>").unwrap());
    static PARAGRAPH: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)<w:p[ >].*?</w:p>").unwrap());
    static STYLE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"<w:pStyle w:val="([^"]*)""#).unwrap());
    // Text runs, and the breaks and tabs that separate words.
    static PIECE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)<w:t(?:\s[^>]*)?>(.*?)</w:t>|<w:br\s*/>|<w:tab\s*/>").unwrap()
    });
    let body = TABLE.replace_all(xml, "");
    PARAGRAPH
        .find_iter(&body)
        .filter_map(|paragraph| {
            let paragraph = paragraph.as_str();
            let mut text = String::new();
            for piece in PIECE.captures_iter(paragraph) {
                match piece.get(1) {
                    Some(run) => text.push_str(run.as_str()),
                    None => text.push(' '),
                }
            }
            let text = strip_html(&text);
            (!text.is_empty()).then(|| Paragraph {
                style: STYLE
                    .captures(paragraph)
                    .map(|style| style[1].to_string())
                    .unwrap_or_default(),
                text,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::LocalStore;
    use crate::test_http::{Response, TestServer};
    use std::io::Write;
    use std::path::PathBuf;

    /// A Word file of the given paragraphs, each a style and its text; a
    /// style of "table" puts the text in a table cell instead.
    fn docx(paragraphs: &[(&str, &str)]) -> Vec<u8> {
        let body: String = paragraphs
            .iter()
            .map(|(style, text)| {
                let run = format!("<w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r>");
                match *style {
                    "table" => format!("<w:tbl><w:tr><w:tc><w:p>{run}</w:p></w:tc></w:tr></w:tbl>"),
                    "" => format!("<w:p w:rsidR=\"1\">{run}</w:p>"),
                    style => {
                        format!("<w:p><w:pPr><w:pStyle w:val=\"{style}\"/></w:pPr>{run}</w:p>")
                    }
                }
            })
            .collect();
        let xml = format!("<w:document><w:body>{body}</w:body></w:document>");
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

    #[test]
    fn a_treasury_outline_keeps_each_schedule_and_stops_at_the_first_chapter() {
        let file = docx(&[
            ("Baseparagraphcentred", "EXPLANATORY MEMORANDUM"),
            ("TOC1", "General outline and financial impact1"),
            ("Heading1", "Glossary"),
            ("table", "CGT"),
            ("Heading1", "General outline and financial impact"),
            (
                "Heading2",
                "Schedule 1 - Innovative business CGT concession",
            ),
            ("Heading3", "Outline"),
            (
                "Normal",
                "Schedule 1 to the Bill gives investors a 50 per cent CGT discount.",
            ),
            ("Heading5", "Financial impact"),
            ("Normal", "Schedule 1 is estimated to cost $160 million."),
            ("table", "-40"),
            ("Heading5", "Human rights implications"),
            (
                "Normal",
                "Schedule 1 does not raise any human rights issues.",
            ),
            ("Heading2", "Schedule 2 - PAYG monthly instalments"),
            ("Heading3", "Outline"),
            (
                "Normal",
                "Schedule 2 lets entities pay PAYG instalments monthly &amp; more.",
            ),
            ("Chapterheading", "Innovative business CGT concession"),
            ("Normal", "Detailed explanation of new law."),
        ]);
        assert_eq!(
            outline(&file).unwrap(),
            "Schedule 1 - Innovative business CGT concession\n\
             Schedule 1 to the Bill gives investors a 50 per cent CGT discount.\n\
             Schedule 2 - PAYG monthly instalments\n\
             Schedule 2 lets entities pay PAYG instalments monthly & more."
        );
    }

    #[test]
    fn a_hand_set_outline_runs_on_into_the_notes_on_clauses_until_the_statement() {
        // A private senator's memorandum: no heading styles, the outline argued
        // and the clauses saying what the bill would actually do.
        let file = docx(&[
            ("", "REPEAL NET ZERO BILL 2025"),
            ("", "OUTLINE"),
            (
                "",
                "The purpose of this Bill is to remove the current Net Zero policy.",
            ),
            ("", "FINANCIAL IMPACT"),
            ("", "It is hard to determine the total savings."),
            ("", "NOTES ON CLAUSES"),
            ("", "Clause 2: Commencement"),
            ("", "2. The Act commences the day after Royal Assent."),
            ("", "Item 1"),
            (
                "",
                "4. The Climate Change Act 2022 is repealed by item 1 of Schedule 1.",
            ),
            ("", "Statement of Compatibility with Human Rights"),
            ("", "This Bill is compatible with human rights."),
        ]);
        assert_eq!(
            outline(&file).unwrap(),
            "The purpose of this Bill is to remove the current Net Zero policy.\n\
             NOTES ON CLAUSES\n\
             Clause 2: Commencement\n\
             2. The Act commences the day after Royal Assent.\n\
             Item 1\n\
             4. The Climate Change Act 2022 is repealed by item 1 of Schedule 1."
        );
    }

    #[test]
    fn an_outline_is_cut_at_the_budget_and_a_memorandum_without_one_is_refused() {
        let long = "A sentence about the bill. ".repeat(150);
        let paragraph = long.trim();
        let mut paragraphs = vec![("", "GENERAL OUTLINE")];
        paragraphs.extend(std::iter::repeat_n(("", paragraph), 5));
        let text = outline(&docx(&paragraphs)).unwrap();
        assert!(text.chars().count() <= OUTLINE_BUDGET);
        assert!(text.lines().count() < 5, "cut short of the whole");
        assert!(
            text.lines().all(|line| line == paragraph),
            "whole paragraphs only"
        );

        let none = docx(&[("", "EXPLANATORY MEMORANDUM"), ("", "Notes on clauses.")]);
        assert!(outline(&none).is_err());
        assert!(outline(b"not a zip").is_err());
    }

    fn new_store(name: &str) -> Store {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/aph-memoranda-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Store::Local(LocalStore::new(dir))
    }

    fn detail(word: &str) -> serde_json::Value {
        serde_json::json!({ "Bill": { "ExplanatoryMemoranda": [
            { "DisplayName": "Explanatory memorandum", "HtmlLink": "https://parlinfo.aph.gov.au/em",
              "WordLink": word }
        ] } })
    }

    #[tokio::test]
    async fn each_memorandum_is_read_once_from_the_file_parlwork_links() {
        let server = TestServer::start(|req| {
            if req.path.starts_with("/parlinfo/parlInfo/download/") {
                return Response::bytes(
                    docx(&[
                        ("", "OUTLINE"),
                        ("", "The Bill amends the Example Act 2001."),
                    ]),
                    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                );
            }
            Response::status(404, "unexpected path")
        });
        let store = new_store("once");
        let endpoints = Endpoints::at(&server.base);
        let first = "https://parlinfo.aph.gov.au/parlInfo/download/legislation/ems/r1_ems/em.docx";
        store
            .put_json("raw/aph/bill-r1.json", &detail(first))
            .await
            .unwrap();
        // A bill with no stored detail is passed over, not fetched.
        let ids = ["r1".to_string(), "r2".to_string()];

        sync_memoranda(&store, &endpoints, &ids).await.unwrap();
        let stored: Memorandum = store
            .get_json(&memorandum_key("r1"))
            .await
            .unwrap()
            .expect("memorandum stored");
        assert_eq!(stored.source, first);
        assert_eq!(stored.outline, "The Bill amends the Example Act 2001.");
        assert_eq!(server.hits(), 1);

        sync_memoranda(&store, &endpoints, &ids).await.unwrap();
        assert_eq!(
            server.hits(),
            1,
            "a memorandum already read is not fetched again"
        );

        let revised =
            "https://parlinfo.aph.gov.au/parlInfo/download/legislation/ems/r1_rem/rem.docx";
        store
            .put_json("raw/aph/bill-r1.json", &detail(revised))
            .await
            .unwrap();
        sync_memoranda(&store, &endpoints, &ids).await.unwrap();
        assert_eq!(server.hits(), 2, "a newly linked file is read");
        let stored: Memorandum = store
            .get_json(&memorandum_key("r1"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.source, revised);
    }

    #[tokio::test]
    async fn a_memorandum_that_cannot_be_read_is_left_for_the_next_run() {
        let server = TestServer::start(|_| Response::status(403, "forbidden"));
        let store = new_store("unreadable");
        store
            .put_json(
                "raw/aph/bill-r1.json",
                &detail("https://parlinfo.aph.gov.au/parlInfo/download/em.docx"),
            )
            .await
            .unwrap();
        sync_memoranda(&store, &Endpoints::at(&server.base), &["r1".to_string()])
            .await
            .expect("an unreadable memorandum is not fatal");
        assert!(store
            .get_json::<Memorandum>(&memorandum_key("r1"))
            .await
            .unwrap()
            .is_none());
    }
}
