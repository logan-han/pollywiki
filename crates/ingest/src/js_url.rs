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
