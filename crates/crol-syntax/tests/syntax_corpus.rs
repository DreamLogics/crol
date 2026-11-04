//! The shared syntax corpus in `tests/syntax/` (`SPEC.md` 12.6): the Rust
//! parser accepts every file in `valid/` and rejects every file in
//! `invalid/`. CI runs the tree-sitter grammar over the same files, so that
//! the two parsers agree.

use std::path::Path;

fn files(dir: &str) -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/syntax")
        .join(dir);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        // Not in a published crate.
        return Vec::new();
    };
    let mut files: Vec<(String, String)> = entries
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "crol"))
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read_to_string(&p).unwrap())
        })
        .collect();
    files.sort();
    files
}

#[test]
fn valid_files_parse() {
    for (name, text) in files("valid") {
        let parse = crol_syntax::parse(&text);
        assert!(parse.errors().is_empty(), "{name}: {:?}", parse.errors());
    }
}

#[test]
fn invalid_files_fail() {
    for (name, text) in files("invalid") {
        let parse = crol_syntax::parse(&text);
        assert!(!parse.errors().is_empty(), "{name} parses without errors");
    }
}
