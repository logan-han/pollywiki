//! Per-division share cards, rasterised at build time.
//!
//! One 1200x630 PNG per division: chamber chip and date, the question in
//! Newsreader, the outcome, the tally and a proportion bar, over the wordmark.
//! Everything is drawn from the same tokens and vendored fonts the pages use.
//!
//! The cards are best-effort. If the fonts cannot be decoded the renderer
//! reports no cards and every division falls back to the site-wide default,
//! so a build never fails over a share image.

use crate::data::{division_key, format_date, title_tier, SiteData};
use crate::html::esc;
use anyhow::{Context, Result};
use pollywiki_schema::{Division, DivisionResult, House};
use resvg::tiny_skia;
use resvg::usvg;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

const W: f64 = 1200.0;
const H: f64 = 630.0;
const MARGIN: f64 = 84.0;
const INNER: f64 = W - 2.0 * MARGIN;

// global.css :root, light scheme.
const PAPER: &str = "#fafaf7";
const INK: &str = "#1c2321";
const MUTED: &str = "#5b655f";
const FAINT: &str = "#6f6e65";
const HAIR_STRONG: &str = "#c9ccbf";
const AYE: &str = "#2f6b4c";
const NO: &str = "#8c3b31";

/// The subsets the cards draw with. Both are already served to browsers.
const SERIF_WOFF2: &str = "fonts/newsreader-latin-wght-normal.woff2";
const SERIF_ITALIC_WOFF2: &str = "fonts/newsreader-latin-wght-italic.woff2";
const MONO_WOFF2: &str = "fonts/ibm-plex-mono-latin-400-normal.woff2";

const WGHT: ttf_parser::Tag = ttf_parser::Tag::from_bytes(b"wght");

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
    mono_data: Vec<u8>,
}

/// The woff2 subsets carry their own internal family names ("Newsreader 16pt
/// 16pt" and so on), so they are read back from the database rather than
/// assumed.
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
        let mono_id = load_one(&mut db, mono_data.clone())?;
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
            mono_data,
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
        serif.set_variation(WGHT, 700.0);
        let mono = ttf_parser::Face::parse(&self.mono_data, 0)
            .map_err(|e| anyhow::anyhow!("mono face: {e}"))?;

        let (chamber, chip_ink, chip_tint) = match division.house {
            House::Representatives => ("House", "#47664f", "#e8efe8"),
            House::Senate => ("Senate", "#99473c", "#f4e9e5"),
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

        // Chamber chip, sized to its own label like the CSS chip does.
        let chip_label = chamber.to_uppercase();
        let chip_text = measure(&mono, &chip_label, 20.0) + 0.08 * 20.0 * chip_label.len() as f64;
        let chip_w = chip_text + 28.0;
        out.push_str(&format!(
            "<rect x=\"{MARGIN}\" y=\"62\" width=\"{chip_w:.1}\" height=\"38\" rx=\"6\" fill=\"{chip_tint}\" stroke=\"{chip_ink}\" stroke-opacity=\"0.3\"/>"
        ));
        out.push_str(&format!(
            "<text x=\"{:.1}\" y=\"88\" font-family=\"{}\" font-size=\"20\" font-weight=\"600\" letter-spacing=\"{:.2}\" fill=\"{chip_ink}\">{}</text>",
            MARGIN + 14.0,
            esc(&self.mono_family),
            0.08 * 20.0,
            esc(&chip_label),
        ));

        // Division number and date, right-aligned on the same line.
        out.push_str(&format!(
            "<text x=\"{:.1}\" y=\"88\" text-anchor=\"end\" font-family=\"{}\" font-size=\"22\" fill=\"{FAINT}\">{}</text>",
            W - MARGIN,
            esc(&self.mono_family),
            esc(&format!(
                "DIVISION {} \u{b7} {}",
                division.number,
                format_date(&division.date).to_uppercase()
            )),
        ));
        out.push_str(&rule(132.0));

        // The question, wrapped to the measure.
        let line_height = title_size * 1.16;
        let mut baseline = 196.0 + title_size * 0.76;
        for line in &lines {
            out.push_str(&format!(
                "<text x=\"{MARGIN}\" y=\"{baseline:.1}\" font-family=\"{}\" font-size=\"{title_size}\" font-weight=\"700\" fill=\"{INK}\">{}</text>",
                esc(&self.serif_family),
                esc(line),
            ));
            baseline += line_height;
        }

        // Outcome and tally, then the proportion bar.
        out.push_str(&format!(
            "<text x=\"{MARGIN}\" y=\"452\" font-family=\"{}\" font-size=\"46\" font-weight=\"700\" fill=\"{outcome_ink}\">{}</text>",
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
            "<rect x=\"{MARGIN}\" y=\"480\" width=\"{INNER}\" height=\"12\" rx=\"3\" fill=\"{HAIR_STRONG}\"/>"
        ));
        if aye_width > 0.0 {
            out.push_str(&format!(
                "<rect x=\"{MARGIN}\" y=\"480\" width=\"{aye_width:.1}\" height=\"12\" rx=\"3\" fill=\"{INK}\"/>"
            ));
        }
        out.push_str(&rule(536.0));

        // Wordmark and motto, matching the default card.
        out.push_str(&format!(
            "<text x=\"{MARGIN}\" y=\"584\" font-family=\"{}\" font-size=\"30\" font-weight=\"700\" fill=\"{INK}\">pollywiki<tspan font-weight=\"400\" fill=\"{MUTED}\">.au</tspan></text>",
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

fn rule(y: f64) -> String {
    format!(
        "<rect x=\"{MARGIN}\" y=\"{y}\" width=\"{INNER}\" height=\"1\" fill=\"{HAIR_STRONG}\"/>"
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
        serif.set_variation(WGHT, 700.0);
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
