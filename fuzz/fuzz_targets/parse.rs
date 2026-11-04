//! The parser never panics, gives back its input byte for byte, and the
//! formatter is idempotent on what it accepts (`SPEC.md` 14.2).

#![no_main]

use crol_syntax::format::{FormatConfig, format};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|text: &str| {
    let parse = crol_syntax::parse(text);
    assert_eq!(parse.syntax().text().to_string(), text);
    let config = FormatConfig::default();
    if let Ok(formatted) = format(text, &config) {
        assert_eq!(
            format(&formatted, &config).ok().as_deref(),
            Some(formatted.as_str()),
            "formatting is not idempotent"
        );
    }
});
