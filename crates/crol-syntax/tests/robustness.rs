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
    "top",
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
    let text = include_str!("fixtures/services.crod");
    for (i, _) in text.char_indices() {
        let prefix = &text[..i];
        assert_eq!(crol_syntax::parse(prefix).syntax().to_string(), prefix);
    }
}

/// What the formatter must not change: the tokens, but for commas.
fn tokens(text: &str) -> Vec<String> {
    crol_syntax::parse(text)
        .syntax()
        .descendants_with_tokens()
        .filter_map(|it| it.into_token())
        .filter(|t| !t.kind().is_trivia() && t.kind() != crol_syntax::SyntaxKind::COMMA)
        .map(|t| t.text().to_owned())
        .collect()
}

/// Leaving out commas must keep every entry apart and every value whole
/// (`SPEC.md` 5.3, 12.2), whatever the entries next to each other are.
#[test]
fn formatting_random_entries_keeps_their_reading() {
    const LINES: &[&str] = &[
        "a: 1",
        "b: &x",
        "+c: 2",
        "-d",
        "-12",
        "-[0]",
        "-[\"e f\"]",
        "?g: 3",
        "[\"h i\"]: 4",
        "[0]",
        ".j",
        "<p>",
        "<p> { k: 1 }",
        "+ 2",
        "- 2",
        "- &y",
        "..&t",
        "&",
        "l = m: 1",
        "n := 2",
        "~ { o: 1 }",
        "| 5",
        "* 2",
        "// c",
        "/* c */",
        "/* c\nd */",
        "q: [1, 2]",
        "r: Foo",
        "(1)",
        "s: {\n t: 1\n -12\n}",
        "-\"u v\"",
        "+ /* c */ w: 1",
        "+ // c\n w: 1",
        "x: 1 /* c */ + 2",
    ];
    let config = crol_syntax::format::FormatConfig::default();
    let narrow = crol_syntax::format::FormatConfig::default().with_max_width(8);
    let mut rng = XorShift(0x5EED_C202);
    let mut checked = 0;
    for _ in 0..20_000 {
        let mut text = String::from("a: 1");
        for _ in 0..1 + rng.next() % 5 {
            text.push_str(["\n", "\n", ",\n", ", ", "\r\n"][(rng.next() % 5) as usize]);
            text.push_str(LINES[(rng.next() % LINES.len() as u64) as usize]);
        }
        if !crol_syntax::parse(&text).errors().is_empty() {
            continue;
        }
        checked += 1;
        for config in [&config, &narrow] {
            let formatted = crol_syntax::format::format(&text, config).unwrap();
            assert_eq!(
                tokens(&formatted),
                tokens(&text),
                "{text:?} became {formatted:?}"
            );
            assert_eq!(
                crol_syntax::format::format(&formatted, config).unwrap(),
                formatted,
                "not idempotent: {text:?}"
            );
        }
    }
    assert!(checked > 100, "only {checked} documents parsed");
}

/// The look at the next line for a new entry stays on that line, so that
/// lines which all ask cannot make the parser quadratic.
#[test]
fn many_lines_that_may_begin_an_entry_parse_quickly() {
    for line in ["[0]\n", "+a<b\n", "+a.b\n", "<a\n", "+[\n", "-\n", "&\n"] {
        let text = format!("a: b\n{}", line.repeat(50_000));
        let start = std::time::Instant::now();
        let parse = crol_syntax::parse(&text);
        assert_eq!(parse.syntax().to_string(), text);
        assert!(start.elapsed().as_secs() < 30, "{line:?} took too long");
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
