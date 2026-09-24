//! Maintainer comments stay in the sources under assets/ and are left out of
//! what the build ships. The scripts are inlined into every page, so their
//! comments would be downloaded again with every page view, and the
//! stylesheet holds up the first render until it arrives.

use regex::Regex;
use std::sync::LazyLock;

/// A script without its whole-line `//` comments. A comment after code on the
/// same line stays, and every other line is kept as it is, blank ones
/// included, so automatic semicolon insertion sees the same line breaks.
///
/// This is only safe for scripts with no string or template literal that runs
/// over more than one line, where a line could start with `//` and not be a
/// comment. Block comments are left alone. The site's own scripts have
/// neither, and a test below holds every one of them to that.
pub fn js_without_comment_lines(js: &str) -> String {
    js.split_inclusive('\n')
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect()
}

/// A stylesheet without its `/* … */` comments. Each comment becomes a space,
/// so the tokens either side of it stay apart, then trailing space and the
/// blank lines the comments leave behind are dropped.
///
/// This is only safe while no string or url() in the stylesheet holds `/*`,
/// which a test below checks for the site's own stylesheets.
pub fn css_without_comments(css: &str) -> String {
    static COMMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)/\*.*?\*/").unwrap());
    let mut out = String::with_capacity(css.len());
    for line in COMMENT.replace_all(css, " ").lines() {
        let line = line.trim_end();
        if !line.is_empty() {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ASSETS;

    #[test]
    fn a_script_keeps_its_code_and_trailing_comments_and_drops_comment_lines() {
        let js = "// Header behaviour.\nconst a = 1\n\n  // Indented, inside a block.\n  return // Enter falls through: /search/?q=\u{2026}\nconst url = 'https://example.org/'\n";
        assert_eq!(
            js_without_comment_lines(js),
            "const a = 1\n\n  return // Enter falls through: /search/?q=\u{2026}\nconst url = 'https://example.org/'\n"
        );
        // A last line with no newline is kept or dropped the same way.
        assert_eq!(js_without_comment_lines("a()\n// end"), "a()\n");
        assert_eq!(js_without_comment_lines("a()\nb()"), "a()\nb()");
        assert_eq!(js_without_comment_lines(""), "");
    }

    #[test]
    fn every_shipped_script_suits_comment_line_stripping() {
        let scripts = ASSETS.get_dir("js").expect("assets/js");
        let mut checked = 0;
        for file in scripts.files() {
            let name = file.path().display().to_string();
            let source = file.contents_utf8().expect("utf8 script");
            // Only line comments are stripped, so a block comment would ship.
            assert!(!source.contains("/*"), "{name} has a block comment");
            for (i, line) in source.lines().enumerate() {
                // An odd count of backticks opens a template literal that runs
                // onto the next line, where a leading // is text, not a comment.
                assert!(
                    line.matches('`').count() % 2 == 0,
                    "{name}:{} opens a template literal over lines",
                    i + 1
                );
                // So does a string continued with a trailing backslash.
                assert!(
                    !line.ends_with('\\'),
                    "{name}:{} continues a string over lines",
                    i + 1
                );
            }
            checked += 1;
        }
        assert!(checked >= 8, "expected the site's scripts, found {checked}");
    }

    #[test]
    fn a_stylesheet_loses_its_comments_and_keeps_its_rules() {
        let css = "/* Tokens. */\n:root {\n  --faint: #999; /* for rules */\n}\n\n/* A comment\n   over lines. */\na/**/b { content: '\\25B8' / ''; }\n";
        assert_eq!(
            css_without_comments(css),
            ":root {\n  --faint: #999;\n}\na b { content: '\\25B8' / ''; }\n"
        );
    }

    #[test]
    fn no_shipped_stylesheet_hides_a_comment_opener_in_a_string() {
        for name in ["fonts.css", "global.css"] {
            let css = ASSETS
                .get_file(name)
                .and_then(|f| f.contents_utf8())
                .expect("stylesheet");
            // Walk the sheet as the tokeniser does: a comment runs to its
            // close, a string to its closing quote, a url() to its bracket.
            // Every marker is ASCII, so the walk is over bytes.
            let bytes = css.as_bytes();
            let find = |from: usize, pat: &[u8]| {
                bytes[from..]
                    .windows(pat.len())
                    .position(|w| w == pat)
                    .map(|at| from + at)
            };
            let mut i = 0;
            let mut comments = 0;
            while i < bytes.len() {
                if bytes[i..].starts_with(b"/*") {
                    i = find(i + 2, b"*/").expect("an unclosed comment") + 2;
                    comments += 1;
                } else if bytes[i] == b'"' || bytes[i] == b'\'' {
                    let quote = bytes[i];
                    let mut j = i + 1;
                    while bytes[j] != quote {
                        j += if bytes[j] == b'\\' { 2 } else { 1 };
                    }
                    assert!(!css[i..j].contains("/*"), "{name}: {}", &css[i..=j]);
                    i = j + 1;
                } else if bytes[i..].starts_with(b"url(") {
                    let end = find(i, b")").expect("an unclosed url()");
                    assert!(!css[i..end].contains("/*"), "{name}: {}", &css[i..end]);
                    i = end + 1;
                } else {
                    i += 1;
                }
            }
            assert!(comments > 0, "{name} has comments to strip");
            let stripped = css_without_comments(css);
            assert!(
                !stripped.contains("/*") && !stripped.contains("*/"),
                "{name}"
            );
            assert_eq!(
                stripped.matches('{').count(),
                stripped.matches('}').count(),
                "{name}"
            );
        }
    }
}
