//! Every CROL example in `SPEC.md` parses without errors, and formats
//! idempotently.
//!
//! The specification lives at the root of the repository, not in the
//! package, so this test is skipped where it is absent (in a published
//! crate).

use std::path::Path;

use crol_syntax::format::{FormatConfig, format};

/// Code blocks in the specification that are Rust, not CROL.
fn is_rust(block: &str) -> bool {
    let first = block.trim_start();
    ["#[", "pub ", "let ", "fn ", "use ", "enum "]
        .iter()
        .any(|start| first.starts_with(start))
}

#[test]
fn spec_examples_parse() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../SPEC.md");
    let Ok(spec) = std::fs::read_to_string(&path) else {
        return;
    };
    let mut failures = Vec::new();
    let mut checked = 0;
    let mut lines = spec.lines().enumerate();
    while let Some((start, line)) = lines.next() {
        if line != "```rust" {
            continue;
        }
        let block: String = lines
            .by_ref()
            .take_while(|(_, line)| !line.starts_with("```"))
            .map(|(_, line)| format!("{line}\n"))
            .collect();
        if is_rust(&block) {
            continue;
        }
        checked += 1;
        let parse = crol_syntax::parse(&block);
        assert_eq!(parse.syntax().to_string(), block);
        if let Some(error) = parse.errors().first() {
            failures.push(format!("SPEC.md:{}: {error}", start + 1));
            continue;
        }
        // The formatter accepts it, and is idempotent on it.
        let config = FormatConfig::default();
        let formatted = format(&block, &config).unwrap();
        if format(&formatted, &config).as_ref() != Ok(&formatted) {
            failures.push(format!(
                "SPEC.md:{}: formatting is not idempotent",
                start + 1
            ));
        }
    }
    assert!(
        checked >= 20,
        "found only {checked} examples; is the extraction broken?"
    );
    assert!(
        failures.is_empty(),
        "examples that fail:\n{}",
        failures.join("\n")
    );
}
