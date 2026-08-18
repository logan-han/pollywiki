//! encodeURIComponent-compatible percent encoding.

use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};

/// Everything except A-Z a-z 0-9 - _ . ! ~ * ' ( ), matching JavaScript.
const ENCODE_URI_COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

pub fn encode_uri_component(input: &str) -> String {
    utf8_percent_encode(input, ENCODE_URI_COMPONENT).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_matches_javascript_encode_uri_component() {
        // The reserved set JavaScript leaves alone.
        assert_eq!(encode_uri_component("-_.!~*'()"), "-_.!~*'()");
        assert_eq!(encode_uri_component("a b"), "a%20b");
        assert_eq!(encode_uri_component("a/b?c=d&e"), "a%2Fb%3Fc%3Dd%26e");
        assert_eq!(encode_uri_component("O'Brien"), "O'Brien");
        // Non-ASCII goes out as utf-8 percent triples.
        assert_eq!(encode_uri_component("Ni\u{f1}o"), "Ni%C3%B1o");
        assert_eq!(encode_uri_component(""), "");
    }
}
