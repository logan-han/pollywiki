//! Text out of HTML: tags dropped, entities decoded, whitespace collapsed.

use regex::Regex;
use std::sync::LazyLock;

fn entity_for(name: &str) -> Option<&'static str> {
    match name.to_lowercase().as_str() {
        "amp" => Some("&"),
        "nbsp" => Some(" "),
        "quot" => Some("\""),
        "apos" => Some("'"),
        "lsquo" => Some("\u{2018}"),
        "rsquo" => Some("\u{2019}"),
        "ldquo" => Some("\u{201C}"),
        "rdquo" => Some("\u{201D}"),
        "ndash" => Some("\u{2013}"),
        "mdash" => Some("\u{2014}"),
        "hellip" => Some("\u{2026}"),
        "lt" => Some("<"),
        "gt" => Some(">"),
        _ => None,
    }
}

pub fn strip_html(html: &str) -> String {
    static TAGS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]*>").unwrap());
    static DEC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"&#(\d+);").unwrap());
    static HEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)&#x([0-9a-f]+);").unwrap());
    static NAMED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)&([a-z]+);").unwrap());
    static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

    let text = TAGS.replace_all(html, " ");
    let text = DEC.replace_all(&text, |caps: &regex::Captures| {
        caps[1]
            .parse::<u32>()
            .ok()
            .and_then(char::from_u32)
            .map(String::from)
            .unwrap_or_default()
    });
    let text = HEX.replace_all(&text, |caps: &regex::Captures| {
        u32::from_str_radix(&caps[1], 16)
            .ok()
            .and_then(char::from_u32)
            .map(String::from)
            .unwrap_or_default()
    });
    let text = NAMED.replace_all(&text, |caps: &regex::Captures| {
        entity_for(&caps[1])
            .map(str::to_string)
            .unwrap_or_else(|| caps[0].to_string())
    });
    SPACES.replace_all(&text, " ").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_stripping_decodes_named_decimal_and_hex_entities() {
        assert_eq!(
            strip_html("<p>A&nbsp;bill &amp; a&#8201;note &#x2013; done</p>"),
            "A bill & a note \u{2013} done"
        );
        assert_eq!(
            strip_html("<i>Caf&eacute;</i> &unknown;"),
            "Caf&eacute; &unknown;"
        );
        assert_eq!(strip_html("  <b>  spaced  </b>  "), "spaced");
        assert_eq!(strip_html(""), "");
    }
}
