//! Two escapers, by position: interpolated text escapes the full entity set;
//! interpolated attribute values escape only & and ". Static template text is
//! written literally at the call sites.

pub fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

pub fn esc_attr(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_escaping_covers_the_full_entity_set() {
        assert_eq!(
            esc("Tom & Jerry <b>\"quoted\"</b> it's"),
            "Tom &amp; Jerry &lt;b&gt;&quot;quoted&quot;&lt;/b&gt; it&#39;s"
        );
        // Non-ASCII passes through: the pages are served as utf-8.
        assert_eq!(
            esc("Ni\u{f1}o \u{2014} caf\u{e9}"),
            "Ni\u{f1}o \u{2014} caf\u{e9}"
        );
        assert_eq!(esc(""), "");
    }

    #[test]
    fn attribute_escaping_covers_only_what_can_break_an_attribute() {
        // Inside a double-quoted attribute, only & and " can terminate it.
        assert_eq!(esc_attr("a & b \"c\""), "a &amp; b &quot;c&quot;");
        // Angle brackets and apostrophes are left as they are, by design.
        assert_eq!(esc_attr("<x> it's"), "<x> it's");
        assert_eq!(esc_attr(""), "");
    }

    #[test]
    fn escaping_is_not_applied_twice() {
        // Escaping an already-escaped string doubles the ampersand, which is
        // why call sites escape exactly once, at interpolation.
        assert_eq!(esc(&esc("a & b")), "a &amp;amp; b");
    }
}
