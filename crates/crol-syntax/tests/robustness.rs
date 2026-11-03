//! The parser never panics, always terminates and is always lossless, on any
//! input (`SPEC.md` 14.2). `cargo fuzz` covers this more thoroughly; this
//! test keeps a cheap version of it in every `cargo test`.

/// Fragments that random documents are made of: every token, the common
/// pairs, and some malformed text.
const FRAGMENTS: &[&str] = &[
    "{",
    "}",
    "[",
    "]",
    "(",
    ")",
    "<",
    ">",
    ",",
    ":",
    "::",
    ":=",
    ".",
    "..",
    "...",
    "+",
    "-",
    "*",
    "/",
    "%",
    "~",
    "|",
    "&",
    "^",
    "@",
    "?",
    "=",
    "#",
    "!",
    "#![",
    "a",
    "web",
    "_",
    "self",
    "parent",
    "root",
    "default",
    "use",
    "super",
    "true",
    "1",
    "0x1F",
    "1.5",
    "1.",
    "1u8",
    "\"s\"",
    "\"open",
    "r#\"raw\"#",
    "r#x",
    "'c'",
    "'",
    "// c\n",
    "/* c */",
    "/* open",
    " ",
    "\n",
    "\t",
    "\u{FEFF}",
    "é",
    "$",
    "\\",
    "&self",
    "&parent.&parent",
    ".&parent",
    "<a & !b>",
    "..&t",
];

/// A small deterministic generator, so failures reproduce.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn random_documents_parse_losslessly() {
    let mut rng = XorShift(0x5EED_C201);
    for _ in 0..5_000 {
        let len = rng.next() % 40;
        let text: String = (0..len)
            .map(|_| FRAGMENTS[(rng.next() % FRAGMENTS.len() as u64) as usize])
            .collect();
        let parse = crol_syntax::parse(&text);
        assert_eq!(parse.syntax().to_string(), text, "not lossless: {text:?}");
        for error in parse.errors() {
            assert!(
                usize::from(error.range().end()) <= text.len(),
                "error out of range in {text:?}"
            );
        }
    }
}

#[test]
fn every_prefix_of_an_example_parses_losslessly() {
    let text = include_str!("fixtures/services.crol");
    for (i, _) in text.char_indices() {
        let prefix = &text[..i];
        assert_eq!(crol_syntax::parse(prefix).syntax().to_string(), prefix);
    }
}

#[test]
fn deep_nesting_is_an_error_not_a_crash() {
    for text in [
        format!("a: {}{}", "[".repeat(20_000), "]".repeat(20_000)),
        format!("a: {}1", "-".repeat(20_000)),
        format!("a: 1{}", " + 1".repeat(20_000)),
        format!("a: {}", "{ x: ".repeat(20_000)),
        format!("a<{}b>: 1", "!".repeat(20_000)),
        format!("a: {}", "..{".repeat(20_000)),
    ] {
        let parse = crol_syntax::parse(&text);
        assert_eq!(parse.syntax().to_string(), text);
        assert!(
            parse
                .errors()
                .iter()
                .any(|e| e.message().contains("too deeply"))
        );
    }
}
