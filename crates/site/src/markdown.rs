//! CommonMark rendering via markdown-rs, the same author's Rust port of the
//! micromark renderer this site's markdown was authored against: raw HTML in
//! the source is escaped by default and the output shape matches byte for byte.
//!
//! One parity gap needs correcting: inside a blockquote, micromark renders a
//! list as LOOSE (items wrapped in <p>) when a blank quote line separates it
//! from a following deeper blockquote — TVFY's house style for quoting
//! amendment text after a bulleted summary. markdown-rs keeps such lists
//! tight. A phantom list item forces markdown-rs to agree, then vanishes.

use std::sync::LazyLock;

use regex::Regex;

/// Private-use marker; if the source ever contains it, the correction is
/// skipped rather than risking corruption.
const PHANTOM: char = '\u{E000}';

pub fn to_html(markdown: &str) -> String {
    let prepared = force_loose_lists(markdown);
    let html = markdown::to_html(&prepared);
    match prepared {
        p if p == markdown => html,
        _ => strip_phantom_items(&html),
    }
}

static QUOTE_BLANK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*(>\s*)+$").unwrap());
static LIST_MARKER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s{0,3}([-*+]|\d{1,9}[.)])[ \t]").unwrap());

fn quote_depth(line: &str) -> usize {
    line.chars().filter(|&c| c == '>').count()
}

fn strip_quotes(line: &str) -> &str {
    let mut rest = line;
    loop {
        let trimmed = rest.trim_start_matches([' ', '\t']);
        match trimmed.strip_prefix('>') {
            Some(after) => rest = after,
            None => return trimmed,
        }
    }
}

/// Inserts a phantom last item wherever micromark would loosen a quoted list:
/// list item lines, a blank quote line, then a deeper blockquote line.
fn force_loose_lists(markdown: &str) -> String {
    if markdown.contains(PHANTOM) {
        return markdown.to_string();
    }
    let eol = if markdown.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let lines: Vec<&str> = markdown.split('\n').collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 4);
    for (i, raw) in lines.iter().enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        out.push(line.to_string());
        if !QUOTE_BLANK.is_match(line) {
            continue;
        }
        let depth = quote_depth(line);
        if depth == 0 {
            continue;
        }
        // The next line must open a deeper blockquote.
        let next = lines
            .get(i + 1)
            .map(|l| l.strip_suffix('\r').unwrap_or(l))
            .unwrap_or("");
        if quote_depth(next) <= depth || !next.trim_start().starts_with('>') {
            continue;
        }
        // The content block right above must be a list item (walk to the top
        // of the contiguous run so multi-line items still qualify).
        let mut first_of_run: Option<&str> = None;
        for j in (0..i).rev() {
            let prev = lines[j].strip_suffix('\r').unwrap_or(lines[j]);
            if QUOTE_BLANK.is_match(prev) || prev.trim().is_empty() {
                break;
            }
            first_of_run = Some(strip_quotes(prev));
        }
        let Some(first) = first_of_run else { continue };
        let Some(caps) = LIST_MARKER.captures(first) else {
            continue;
        };
        // A different bullet or delimiter would start a new list, so the
        // phantom copies the original marker (any ordinal continues an
        // ordered list as long as the delimiter matches).
        let marker = match &caps[1] {
            bullet @ ("-" | "*" | "+") => bullet.to_string(),
            ordered => format!("1{}", &ordered[ordered.len() - 1..]),
        };
        out.push(format!("{} {marker} {PHANTOM}", ">".repeat(depth)));
        out.push(">".repeat(depth));
    }
    let joined = out.join(eol);
    if joined == markdown {
        markdown.to_string()
    } else {
        joined
    }
}

fn strip_phantom_items(html: &str) -> String {
    let mut out = html.to_string();
    for eol in ["\r\n", "\n"] {
        let loose = format!("<li>{eol}<p>{PHANTOM}</p>{eol}</li>{eol}");
        out = out.replace(&loose, "");
        let tight = format!("<li>{PHANTOM}</li>{eol}");
        out = out.replace(&tight, "");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::to_html;

    /// The probe matrix, verified against micromark's own output.
    #[test]
    fn matches_micromark_on_quoted_list_looseness() {
        assert_eq!(
            to_html("> - a\n>\n>>> b"),
            "<blockquote>\n<ul>\n<li>\n<p>a</p>\n</li>\n</ul>\n<blockquote>\n<blockquote>\n<p>b</p>\n</blockquote>\n</blockquote>\n</blockquote>"
        );
        assert_eq!(
            to_html("> - a\n>>> b"),
            "<blockquote>\n<ul>\n<li>a</li>\n</ul>\n<blockquote>\n<blockquote>\n<p>b</p>\n</blockquote>\n</blockquote>\n</blockquote>"
        );
        assert_eq!(
            to_html("- a\n\n> b"),
            "<ul>\n<li>a</li>\n</ul>\n<blockquote>\n<p>b</p>\n</blockquote>"
        );
        assert_eq!(
            to_html("> - a\n> - b\n>\n>>> c"),
            "<blockquote>\n<ul>\n<li>\n<p>a</p>\n</li>\n<li>\n<p>b</p>\n</li>\n</ul>\n<blockquote>\n<blockquote>\n<p>c</p>\n</blockquote>\n</blockquote>\n</blockquote>"
        );
    }

    #[test]
    fn leaves_quoted_paragraphs_alone() {
        assert_eq!(
            to_html("> para\n>\n>>> b"),
            markdown::to_html("> para\n>\n>>> b")
        );
    }

    #[test]
    fn escapes_raw_html() {
        assert_eq!(
            to_html("hi <b>there</b>"),
            "<p>hi &lt;b&gt;there&lt;/b&gt;</p>"
        );
    }
}
