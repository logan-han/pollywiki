//! Per-division and per-bill share cards, rasterised at build time.
//!
//! One 1200x630 PNG per division: chamber and date, the question in Source
//! Serif, the outcome, the tally and a proportion bar, over the wordmark.
//! One per bill: its chamber and parliament, its title and how far it has
//! come. A bill's card, like the site-wide default, keeps its substance in
//! the middle square, which is all a chat app's thumbnail shows of it.
//! Everything is drawn from the same tokens and vendored fonts the pages use.
//!
//! The cards are best-effort. If the fonts cannot be decoded the renderer
//! reports no cards and every division falls back to the site-wide default,
//! so a build never fails over a share image.

use crate::data::{division_key, format_date, ordinal, title_tier, SiteData};
use crate::html::esc;
use anyhow::{Context, Result};
use pollywiki_schema::{Bill, Division, DivisionResult, House};
use resvg::tiny_skia;
use resvg::usvg;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

const W: f64 = 1200.0;
const H: f64 = 630.0;
const MARGIN: f64 = 84.0;
const INNER: f64 = W - 2.0 * MARGIN;

/// The middle of the card, and the width of the square around it a chat
/// app's thumbnail crops to, less a margin of its own.
const CENTRE: f64 = W / 2.0;
const COLUMN: f64 = H - 2.0 * 48.0;

// global.css :root, light scheme.
const PAPER: &str = "#f6f3ec";
const INK: &str = "#161513";
const MUTED: &str = "#4d4a43";
const FAINT: &str = "#6b675e";
const HAIR: &str = "#d6d0c2";
const AYE: &str = "#2f6b4c";
const NO: &str = "#8c3b31";
const HOUSE: &str = "#47664f";
const SENATE: &str = "#99473c";

/// The weight the pages set headlines in.
const TITLE_WEIGHT: f32 = 600.0;

/// The subsets the cards draw with. All are already served to browsers.
const SERIF_WOFF2: &str = "fonts/source-serif-4-latin-opsz-normal.woff2";
const SERIF_ITALIC_WOFF2: &str = "fonts/source-serif-4-latin-wght-italic.woff2";
const MONO_WOFF2: &str = "fonts/spline-sans-mono-latin-wght-normal.woff2";

const WGHT: ttf_parser::Tag = ttf_parser::Tag::from_bytes(b"wght");

/// Site-relative path of a bill's card.
pub fn bill_card_path(bill: &Bill) -> String {
    format!("/og/bills/{}.png", bill.id)
}

/// Site-relative path of a division's card.
pub fn card_path(division: &Division) -> String {
    format!(
        "/og/divisions/{}/{}.png",
        division.house,
        division_key(division)
    )
}

pub struct Cards {
    options: usvg::Options<'static>,
    serif_family: String,
    mono_family: String,
    serif_data: Vec<u8>,
}

/// The woff2 subsets carry their own internal family names, which need not
/// match the names the stylesheet gives them, so they are read back from the
/// database rather than assumed.
fn family_of(db: &usvg::fontdb::Database, id: usvg::fontdb::ID) -> Option<String> {
    db.face(id)?
        .families
        .first()
        .map(|(name, _)| name.to_string())
}

impl Cards {
    /// None when the vendored fonts cannot be decoded; callers fall back to the
    /// default card.
    pub fn load() -> Option<Cards> {
        match Cards::try_load() {
            Ok(cards) => Some(cards),
            Err(err) => {
                eprintln!("og: division cards skipped ({err:#})");
                None
            }
        }
    }

    fn try_load() -> Result<Cards> {
        let serif_data = decode(SERIF_WOFF2)?;
        let serif_italic = decode(SERIF_ITALIC_WOFF2)?;
        let mono_data = decode(MONO_WOFF2)?;

        let mut db = usvg::fontdb::Database::new();
        let serif_id = load_one(&mut db, serif_data.clone())?;
        load_one(&mut db, serif_italic)?;
        let mono_id = load_one(&mut db, mono_data)?;
        let serif_family = family_of(&db, serif_id).context("serif family name missing")?;
        let mono_family = family_of(&db, mono_id).context("mono family name missing")?;

        let options = usvg::Options {
            font_family: serif_family.clone(),
            fontdb: Arc::new(db),
            ..Default::default()
        };

        Ok(Cards {
            options,
            serif_family,
            mono_family,
            serif_data,
        })
    }

    /// Writes one card per division and returns division id -> site path for
    /// the ones that rendered.
    pub fn write_all(&self, out_dir: &Path, data: &SiteData) -> Result<HashMap<String, String>> {
        let mut written = HashMap::with_capacity(data.divisions.len());
        for division in &data.divisions {
            let rel = card_path(division);
            let file = out_dir.join(rel.trim_start_matches('/'));
            std::fs::create_dir_all(file.parent().expect("card path has a parent"))?;
            let png = self
                .render(division)
                .with_context(|| format!("rendering card for {}", division.id))?;
            std::fs::write(&file, png)?;
            written.insert(division.id.clone(), rel);
        }
        Ok(written)
    }

    /// Writes one card per bill and returns bill id -> site path for the ones
    /// that rendered.
    pub fn write_bills(&self, out_dir: &Path, data: &SiteData) -> Result<HashMap<String, String>> {
        let mut written = HashMap::with_capacity(data.bills.len());
        for bill in &data.bills {
            let rel = bill_card_path(bill);
            let file = out_dir.join(rel.trim_start_matches('/'));
            std::fs::create_dir_all(file.parent().expect("card path has a parent"))?;
            let png = self
                .rasterise(&self.bill_svg(bill)?)
                .with_context(|| format!("rendering card for {}", bill.id))?;
            std::fs::write(&file, png)?;
            written.insert(bill.id.clone(), rel);
        }
        Ok(written)
    }

    /// A bill's card: the chamber it began in and its parliament, the title
    /// as large as fits the middle square, then its stage as the pages' dots
    /// and its progress in their words.
    fn bill_svg(&self, bill: &Bill) -> Result<String> {
        let mut serif = ttf_parser::Face::parse(&self.serif_data, 0)
            .map_err(|e| anyhow::anyhow!("serif face: {e}"))?;
        serif.set_variation(WGHT, TITLE_WEIGHT);
        let serif_family = esc(&self.serif_family);
        let mono = esc(&self.mono_family);
        let (chamber, chamber_ink) = match bill.chamber {
            House::Representatives => ("House", HOUSE),
            House::Senate => ("Senate", SENATE),
        };

        // The largest size whose wrap fits its line budget, else the smallest,
        // elided.
        let (size, lines) = [(56.0, 4), (48.0, 5), (40.0, 6)]
            .iter()
            .map(|&(size, limit)| (size, limit, wrap(&serif, &bill.title, size, COLUMN, limit)))
            .find(|(_, _, lines)| !lines.last().is_some_and(|l| l.ends_with('\u{2026}')))
            .map(|(size, _, lines)| (size, lines))
            .unwrap_or_else(|| (40.0, wrap(&serif, &bill.title, 40.0, COLUMN, 6)));

        let mut out = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{W}\" height=\"{H}\" viewBox=\"0 0 {W} {H}\"><rect width=\"{W}\" height=\"{H}\" fill=\"{PAPER}\"/>"
        );
        out.push_str(&format!(
            "<text x=\"{CENTRE}\" y=\"74\" text-anchor=\"middle\" font-family=\"{mono}\" font-size=\"20\" font-weight=\"500\" letter-spacing=\"1.2\" fill=\"{chamber_ink}\">{}</text>",
            esc(&format!(
                "{} BILL \u{b7} {} PARLIAMENT",
                chamber.to_uppercase(),
                ordinal(bill.parliament).to_uppercase()
            )),
        ));
        out.push_str(&rule(100.0));

        // The title, centred in the space between the rule and the dots.
        let line_height = size * 1.12;
        let block = line_height * lines.len() as f64;
        let mut baseline = 118.0 + (300.0 - block) / 2.0 + size * 0.82;
        for line in &lines {
            out.push_str(&format!(
                "<text x=\"{CENTRE}\" y=\"{baseline:.1}\" text-anchor=\"middle\" font-family=\"{serif_family}\" font-size=\"{size}\" font-weight=\"{TITLE_WEIGHT}\" letter-spacing=\"{:.2}\" fill=\"{INK}\">{}</text>",
                -0.02 * size,
                esc(line),
            ));
            baseline += line_height;
        }

        // Stage dots, filled as far as the bill has come, then its progress.
        let stage = crate::components::bill_stage(bill);
        for step in 1..=4u8 {
            let cx = CENTRE + (f64::from(step) - 2.5) * 34.0;
            if step <= stage {
                out.push_str(&format!(
                    "<circle cx=\"{cx}\" cy=\"452\" r=\"10\" fill=\"{INK}\"/>"
                ));
            } else {
                out.push_str(&format!(
                    "<circle cx=\"{cx}\" cy=\"452\" r=\"9\" fill=\"none\" stroke=\"{INK}\" stroke-width=\"2\"/>"
                ));
            }
        }
        out.push_str(&format!(
            "<text x=\"{CENTRE}\" y=\"504\" text-anchor=\"middle\" font-family=\"{mono}\" font-size=\"22\" letter-spacing=\"1.3\" fill=\"{MUTED}\">{}</text>",
            esc(&crate::pages::bill_progress_line(bill).to_uppercase()),
        ));
        out.push_str(&double_rule(540.0));
        out.push_str(&format!(
            "<text x=\"{CENTRE}\" y=\"596\" text-anchor=\"middle\" font-family=\"{serif_family}\" font-size=\"32\" font-weight=\"700\" letter-spacing=\"-0.6\" fill=\"{INK}\">pollywiki<tspan font-weight=\"400\" fill=\"{FAINT}\">.au</tspan></text>"
        ));
        out.push_str("</svg>");
        Ok(out)
    }

    /// Writes the site-wide card every page without its own shares: the
    /// wordmark over the nine parliaments it keeps the record of.
    pub fn write_default(&self, out_dir: &Path) -> Result<()> {
        let png = self.rasterise(&self.default_svg())?;
        std::fs::write(
            out_dir.join(crate::layout::DEFAULT_OG_IMAGE.trim_start_matches('/')),
            png,
        )?;
        Ok(())
    }

    fn default_svg(&self) -> String {
        let serif = esc(&self.serif_family);
        let mono = esc(&self.mono_family);
        let parliaments = crate::layout::PARLIAMENTS
            .iter()
            .map(|(short, _, _)| *short)
            .collect::<Vec<_>>()
            .join("  ");
        let mut out = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{W}\" height=\"{H}\" viewBox=\"0 0 {W} {H}\"><rect width=\"{W}\" height=\"{H}\" fill=\"{PAPER}\"/>"
        );
        // Centred, so the square a chat app crops to keeps the wordmark whole.
        out.push_str(&format!(
            "<text x=\"{CENTRE}\" y=\"96\" text-anchor=\"middle\" font-family=\"{mono}\" font-size=\"18\" letter-spacing=\"2.2\" fill=\"{FAINT}\">THE AUSTRALIAN PARLIAMENTARY RECORD</text>"
        ));
        out.push_str(&rule(132.0));
        out.push_str(&format!(
            "<text x=\"{CENTRE}\" y=\"318\" text-anchor=\"middle\" font-family=\"{serif}\" font-size=\"104\" font-weight=\"700\" letter-spacing=\"-2\" fill=\"{INK}\">pollywiki<tspan font-weight=\"400\" fill=\"{FAINT}\">.au</tspan></text>"
        ));
        out.push_str(&format!(
            "<text x=\"{CENTRE}\" y=\"412\" text-anchor=\"middle\" font-family=\"{serif}\" font-size=\"34\" font-style=\"italic\" fill=\"{MUTED}\">Every parliament's record, unedited.</text>"
        ));
        out.push_str(&double_rule(500.0));
        out.push_str(&format!(
            "<text x=\"{CENTRE}\" y=\"556\" text-anchor=\"middle\" font-family=\"{mono}\" font-size=\"19\" xml:space=\"preserve\" fill=\"{FAINT}\">{}</text>",
            esc(&parliaments)
        ));
        out.push_str("</svg>");
        out
    }

    fn rasterise(&self, svg: &str) -> Result<Vec<u8>> {
        let tree = usvg::Tree::from_str(svg, &self.options)
            .map_err(|e| anyhow::anyhow!("usvg parse: {e}"))?;
        let mut pixmap =
            tiny_skia::Pixmap::new(W as u32, H as u32).context("allocating the card pixmap")?;
        resvg::render(
            &tree,
            tiny_skia::Transform::identity(),
            &mut pixmap.as_mut(),
        );
        pixmap
            .encode_png()
            .map_err(|e| anyhow::anyhow!("png encode: {e}"))
    }

    fn render(&self, division: &Division) -> Result<Vec<u8>> {
        let svg = self.svg(division)?;
        let tree = usvg::Tree::from_str(&svg, &self.options)
            .map_err(|e| anyhow::anyhow!("usvg parse: {e}"))?;
        let mut pixmap =
            tiny_skia::Pixmap::new(W as u32, H as u32).context("allocating the card pixmap")?;
        resvg::render(
            &tree,
            tiny_skia::Transform::identity(),
            &mut pixmap.as_mut(),
        );
        pixmap
            .encode_png()
            .map_err(|e| anyhow::anyhow!("png encode: {e}"))
    }

    fn svg(&self, division: &Division) -> Result<String> {
        let mut serif = ttf_parser::Face::parse(&self.serif_data, 0)
            .map_err(|e| anyhow::anyhow!("serif face: {e}"))?;
        serif.set_variation(WGHT, TITLE_WEIGHT);

        let (chamber, chamber_ink) = match division.house {
            House::Representatives => ("House of Representatives", HOUSE),
            House::Senate => ("Senate", SENATE),
        };
        let (outcome, outcome_ink) = match division.result {
            DivisionResult::Passed => ("Carried", AYE),
            DivisionResult::Rejected => ("Negatived", NO),
        };

        // Same tiering the h1 uses, scaled for a 1200-wide card.
        let title_size = match title_tier(&division.name) {
            "title-l" => 66.0,
            "title-m" => 52.0,
            _ => 44.0,
        };
        let lines = wrap(&serif, &division.name, title_size, INNER, 4);

        let total = (division.ayes + division.noes).max(1) as f64;
        let aye_width = division.ayes as f64 / total * INNER;

        let mut out = String::with_capacity(4 * 1024);
        out.push_str(&format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{W}\" height=\"{H}\" viewBox=\"0 0 {W} {H}\">"
        ));
        out.push_str(&format!(
            "<rect width=\"{W}\" height=\"{H}\" fill=\"{PAPER}\"/>"
        ));

        // The chamber in its own colour, as the page's meta line sets it, with
        // the division number and date right-aligned on the same line.
        out.push_str(&format!(
            "<text x=\"{MARGIN}\" y=\"88\" font-family=\"{}\" font-size=\"22\" font-weight=\"500\" letter-spacing=\"{:.2}\" fill=\"{chamber_ink}\">{}</text>",
            esc(&self.mono_family),
            0.06 * 22.0,
            esc(&chamber.to_uppercase()),
        ));
        out.push_str(&format!(
            "<text x=\"{:.1}\" y=\"88\" text-anchor=\"end\" font-family=\"{}\" font-size=\"22\" letter-spacing=\"{:.2}\" fill=\"{FAINT}\">{}</text>",
            W - MARGIN,
            esc(&self.mono_family),
            0.06 * 22.0,
            esc(&format!(
                "DIVISION {} \u{b7} {}",
                division.number,
                format_date(&division.date).to_uppercase()
            )),
        ));
        out.push_str(&rule(132.0));

        // The question, wrapped to the measure.
        let line_height = title_size * 1.1;
        let mut baseline = 196.0 + title_size * 0.76;
        for line in &lines {
            out.push_str(&format!(
                "<text x=\"{MARGIN}\" y=\"{baseline:.1}\" font-family=\"{}\" font-size=\"{title_size}\" font-weight=\"{TITLE_WEIGHT}\" letter-spacing=\"{:.2}\" fill=\"{INK}\">{}</text>",
                esc(&self.serif_family),
                -0.02 * title_size,
                esc(line),
            ));
            baseline += line_height;
        }

        // Outcome and tally, then the proportion bar.
        out.push_str(&format!(
            "<text x=\"{MARGIN}\" y=\"452\" font-family=\"{}\" font-size=\"46\" font-weight=\"{TITLE_WEIGHT}\" fill=\"{outcome_ink}\">{}</text>",
            esc(&self.serif_family),
            esc(outcome),
        ));
        out.push_str(&format!(
            "<text x=\"{:.1}\" y=\"452\" text-anchor=\"end\" font-family=\"{}\" font-size=\"36\" fill=\"{MUTED}\">{}</text>",
            W - MARGIN,
            esc(&self.mono_family),
            esc(&format!(
                "{} AYE \u{b7} {} NO",
                division.ayes, division.noes
            )),
        ));
        out.push_str(&format!(
            "<rect x=\"{MARGIN}\" y=\"480\" width=\"{INNER}\" height=\"12\" fill=\"{HAIR}\"/>"
        ));
        if aye_width > 0.0 {
            out.push_str(&format!(
                "<rect x=\"{MARGIN}\" y=\"480\" width=\"{aye_width:.1}\" height=\"12\" fill=\"{INK}\"/>"
            ));
        }
        out.push_str(&double_rule(536.0));

        // Wordmark and motto, matching the default card.
        out.push_str(&format!(
            "<text x=\"{MARGIN}\" y=\"584\" font-family=\"{}\" font-size=\"30\" font-weight=\"700\" letter-spacing=\"-0.6\" fill=\"{INK}\">pollywiki<tspan font-weight=\"400\" fill=\"{FAINT}\">.au</tspan></text>",
            esc(&self.serif_family),
        ));
        out.push_str(&format!(
            "<text x=\"{:.1}\" y=\"584\" text-anchor=\"end\" font-family=\"{}\" font-size=\"26\" font-style=\"italic\" fill=\"{MUTED}\">the record, unedited</text>",
            W - MARGIN,
            esc(&self.serif_family),
        ));
        out.push_str("</svg>");
        Ok(out)
    }
}

/// A section rule, full ink as the pages draw theirs.
fn rule(y: f64) -> String {
    format!("<rect x=\"{MARGIN}\" y=\"{y}\" width=\"{INNER}\" height=\"1.5\" fill=\"{INK}\"/>")
}

/// The header and footer's double rule, scaled up for the card.
fn double_rule(y: f64) -> String {
    format!(
        "<rect x=\"{MARGIN}\" y=\"{y}\" width=\"{INNER}\" height=\"1.5\" fill=\"{INK}\"/><rect x=\"{MARGIN}\" y=\"{}\" width=\"{INNER}\" height=\"1.5\" fill=\"{INK}\"/>",
        y + 4.0
    )
}

fn decode(name: &str) -> Result<Vec<u8>> {
    let file = crate::ASSETS
        .get_file(name)
        .with_context(|| format!("{name} missing from embedded assets"))?;
    wuff::decompress_woff2(file.contents()).map_err(|e| anyhow::anyhow!("decoding {name}: {e:?}"))
}

fn load_one(db: &mut usvg::fontdb::Database, data: Vec<u8>) -> Result<usvg::fontdb::ID> {
    let before = db.len();
    db.load_font_data(data);
    db.faces()
        .nth(before)
        .map(|face| face.id)
        .context("font data carried no usable face")
}

/// Advance width of `text` at `size`, in the same units as `size`.
fn measure(face: &ttf_parser::Face, text: &str, size: f64) -> f64 {
    let upem = face.units_per_em() as f64;
    if upem == 0.0 {
        return 0.0;
    }
    let units: f64 = text
        .chars()
        .filter_map(|c| face.glyph_index(c))
        .filter_map(|glyph| face.glyph_hor_advance(glyph))
        .map(|advance| advance as f64)
        .sum();
    units / upem * size
}

/// Greedy word wrap to `max` width, capped at `limit` lines. The last line is
/// elided rather than dropped so a long question still reads as truncated.
fn wrap(face: &ttf_parser::Face, text: &str, size: f64, max: f64, limit: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let candidate = if current.is_empty() {
            word.to_string()
        } else {
            format!("{current} {word}")
        };
        if measure(face, &candidate, size) <= max || current.is_empty() {
            current = candidate;
            continue;
        }
        lines.push(std::mem::take(&mut current));
        if lines.len() == limit {
            // Out of room: mark the overflow on the last line and stop.
            let last = lines.last_mut().expect("just pushed");
            while measure(face, &format!("{last} \u{2026}"), size) > max && last.contains(' ') {
                let cut = last.rfind(' ').expect("contains a space");
                last.truncate(cut);
            }
            last.push_str(" \u{2026}");
            return lines;
        }
        current = word.to_string();
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn division(name: &str) -> Division {
        serde_json::from_str(&format!(
            r#"{{"id":"representatives/2026-08-12/1","house":"representatives","date":"2026-08-12","number":1,
                 "name":{},"result":"rejected","ayes":40,"noes":83}}"#,
            serde_json::Value::String(name.to_string())
        ))
        .expect("division fixture")
    }

    fn bill(title: &str) -> Bill {
        serde_json::from_str(&format!(
            r#"{{"id":"r7566","title":{},"parliament":48,"chamber":"representatives",
                 "status":"Before Senate","timeline":[{{"date":"2026-10-29","event":"Third reading agreed to (House of Representatives)"}}]}}"#,
            serde_json::Value::String(title.to_string())
        ))
        .expect("bill fixture")
    }

    #[test]
    fn bill_cards_render_and_keep_a_long_title_in_the_middle_square() {
        let Some(cards) = Cards::load() else {
            panic!("vendored fonts should decode");
        };
        assert_eq!(
            bill_card_path(&bill("Any Bill 2026")),
            "/og/bills/r7566.png"
        );
        let svg = cards
            .bill_svg(&bill(
                "Treasury Laws Amendment (Tax Reform No. 4) Bill 2026",
            ))
            .unwrap();
        assert!(svg.contains("HOUSE BILL \u{b7} 48TH PARLIAMENT"));
        assert!(svg.contains("PASSED 1ST HOUSE \u{b7} BEFORE SENATE"));
        assert!(cards.rasterise(&svg).unwrap().starts_with(b"\x89PNG"));

        // A title far past what fits still ends inside six lines of the column.
        let long = "Administrative and Judicial Review Legislation Amendment ".repeat(6);
        let svg = cards.bill_svg(&bill(&long)).unwrap();
        assert!(svg.contains('\u{2026}'), "the overflow is marked");
        let mut serif = ttf_parser::Face::parse(&cards.serif_data, 0).unwrap();
        serif.set_variation(WGHT, TITLE_WEIGHT);
        for line in wrap(&serif, &long, 40.0, COLUMN, 6) {
            assert!(
                measure(&serif, &line, 40.0) <= COLUMN,
                "line overflows: {line}"
            );
        }
    }

    #[test]
    fn card_paths_mirror_the_page_paths() {
        assert_eq!(
            card_path(&division("Anything")),
            "/og/divisions/representatives/2026-08-12-1.png"
        );
    }

    #[test]
    fn cards_render_and_wrap_long_questions() {
        let Some(cards) = Cards::load() else {
            panic!("vendored fonts should decode");
        };
        let short = cards
            .render(&division("Motions \u{2014} Australia and Japan"))
            .unwrap();
        assert!(short.starts_with(b"\x89PNG"), "expected a PNG header");

        // A question far past the measure must still fit inside four lines.
        let long_name =
            "Bills \u{2014} ".to_string() + &"Cash Distribution Framework Amendment ".repeat(12);
        let long = cards.render(&division(&long_name)).unwrap();
        assert!(long.starts_with(b"\x89PNG"));

        let mut serif = ttf_parser::Face::parse(&cards.serif_data, 0).unwrap();
        serif.set_variation(WGHT, TITLE_WEIGHT);
        let lines = wrap(&serif, &long_name, 44.0, INNER, 4);
        assert_eq!(lines.len(), 4);
        assert!(lines[3].ends_with('\u{2026}'));
        for line in &lines {
            assert!(
                measure(&serif, line, 44.0) <= INNER,
                "line overflows: {line}"
            );
        }
    }
}
