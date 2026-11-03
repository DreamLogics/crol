//! The conformance suite (`SPEC.md` 14.2), in `tests/cases/` at the root of
//! the repository.
//!
//! Each case is a directory:
//!
//! - `layers/<n>-<name>/`: the layers, in the order of `<n>`, each holding
//!   `.crol` files at any depth;
//! - `options.crol` (optional): options, as a value-mode document;
//! - `expected.crol`: the evaluated data, if evaluation succeeds;
//! - `expected.errors`: the diagnostics, one per line, as
//!   `<layer dir>/<path>:line:col: message`, if there are any.
//!
//! `UPDATE_EXPECT=1 cargo test` writes the expected files; review the diff.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use crol_eval::{
    DataOptions, Key, Layer, Severity, Sources, ValueKind, evaluate_data, evaluate_value, print,
};

fn cases_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/cases")
}

fn crol_files(dir: &Path, base: &Path, out: &mut Vec<(String, PathBuf)>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            crol_files(&path, base, out);
        } else if path.extension().is_some_and(|e| e == "crol") {
            let relative = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.push((relative, path));
        }
    }
}

fn options(case: &Path) -> DataOptions {
    let mut options = DataOptions::default();
    let path = case.join("options.crol");
    let Ok(text) = fs::read_to_string(&path) else {
        return options;
    };
    let mut sources = Sources::new();
    let file = sources.add("options.crol", text);
    let value = evaluate_value(&sources, file)
        .value
        .expect("options.crol evaluates");
    let ValueKind::Keyed(keyed) = value.kind else {
        panic!("options.crol is not a keyed value")
    };
    for (key, value) in keyed.entries {
        match (key, value.kind) {
            (Key::Str(k), ValueKind::Bool(b)) if k == "warn_cross_file_patches" => {
                options.warn_cross_file_patches = b;
            }
            (key, _) => panic!("unknown option {key} in {}", path.display()),
        }
    }
    options
}

/// Runs one case and returns what it produced: the data, and the
/// diagnostics.
fn run(case: &Path) -> (Option<String>, String) {
    let mut sources = Sources::new();
    let mut layers = Vec::new();
    let mut layer_dirs: Vec<PathBuf> = fs::read_dir(case.join("layers"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    layer_dirs.sort();
    for dir in layer_dirs {
        let dir_name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let name = dir_name
            .split_once('-')
            .map_or(dir_name.as_str(), |(_, n)| n)
            .to_owned();
        let mut files = Vec::new();
        crol_files(&dir, &dir, &mut files);
        let documents = files
            .into_iter()
            .map(|(relative, path)| {
                let text = fs::read_to_string(&path).unwrap();
                let file = sources.add(format!("{dir_name}/{relative}"), text);
                (relative, file)
            })
            .collect();
        layers.push(Layer { name, documents });
    }
    let evaluation = evaluate_data(&sources, &layers, &options(case));
    let data = evaluation
        .value
        .as_ref()
        .map(|value| print::to_document(value, &Default::default()));
    let mut errors = String::new();
    for diagnostic in &evaluation.diagnostics {
        let prefix = match diagnostic.severity {
            Severity::Error => "",
            Severity::Warning => "warning: ",
        };
        let _ = writeln!(
            errors,
            "{prefix}{}: {}",
            sources.location(diagnostic.span),
            diagnostic.message
        );
    }
    (data, errors)
}

#[test]
fn conformance() {
    let update = std::env::var_os("UPDATE_EXPECT").is_some();
    let mut cases: Vec<PathBuf> = fs::read_dir(cases_dir())
        .expect("tests/cases exists")
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    cases.sort();
    assert!(!cases.is_empty());
    let mut failures = Vec::new();
    for case in &cases {
        let name = case.file_name().unwrap().to_string_lossy().into_owned();
        let (data, errors) = run(case);
        let expected_data = fs::read_to_string(case.join("expected.crol")).ok();
        let expected_errors = fs::read_to_string(case.join("expected.errors")).ok();
        let errors = (!errors.is_empty()).then_some(errors);
        if update {
            for (file, content) in [("expected.crol", &data), ("expected.errors", &errors)] {
                let path = case.join(file);
                match content {
                    Some(content) => fs::write(&path, content).unwrap(),
                    None if path.exists() => fs::remove_file(&path).unwrap(),
                    None => {}
                }
            }
            continue;
        }
        if data != expected_data {
            failures.push(format!(
                "{name}: data differs\n--- expected\n{}\n--- actual\n{}",
                expected_data.unwrap_or_default(),
                data.unwrap_or_default()
            ));
        }
        if errors != expected_errors {
            failures.push(format!(
                "{name}: diagnostics differ\n--- expected\n{}\n--- actual\n{}",
                expected_errors.unwrap_or_default(),
                errors.unwrap_or_default()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} failing cases:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
