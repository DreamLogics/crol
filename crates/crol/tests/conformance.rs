//! The conformance cases with a schema (`SPEC.md` 14.2): each is checked
//! both by deserializing into the test types below and by `crol::check`,
//! and both must give the same errors.
//!
//! A case has a schema when its `options.crol` has `schema: "services"`.
//! The errors `crol::check` reports are in `expected.schema-errors`, one
//! per line, as `<layer dir>/<path>:line:col: message`;
//! `UPDATE_EXPECT=1 cargo test` writes them. The evaluation itself is
//! checked by `crol-eval`'s conformance test.

#![allow(missing_docs)]

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use crol::{CrolComposer, CrolData, CrolLayer};
use crol_derive::Schema;
use serde_derive::Deserialize;

/// How much a service may use.
#[derive(Debug, Deserialize, Schema)]
pub struct Limits {
    pub cpu: f64,
    pub memory_mb: u32,
}

#[derive(Debug, Deserialize, Schema)]
pub enum Mode {
    Fast,
    Slow(u32),
}

/// A service that runs one container image.
#[derive(Debug, Deserialize, Schema)]
pub struct Service {
    pub replicas: u32,
    pub image: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub limits: Option<Limits>,
    #[serde(default = "fast")]
    pub mode: Mode,
    #[serde(default)]
    #[crol(refers(Service, Volume))]
    pub depends_on: Vec<crol::Id>,
}

fn fast() -> Mode {
    Mode::Fast
}

#[derive(Debug, Deserialize, Schema)]
pub struct Volume {
    pub size_gb: u32,
}

/// Everything the data can define.
#[derive(Debug, Deserialize, Schema)]
pub enum Definition {
    Service(Service),
    Volume(Volume),
}

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

/// The schema a case names in its options, if any; `crol-eval`'s test
/// reads the other options.
fn schema_of(case: &Path) -> Option<String> {
    let text = fs::read_to_string(case.join("options.crol")).ok()?;
    let mut sources = crol_eval::Sources::new();
    let file = sources.add("options.crol", text);
    let value = crol_eval::evaluate_value(&sources, file).value?;
    let crol_eval::ValueKind::Keyed(keyed) = value.kind else {
        return None;
    };
    match &keyed
        .entries
        .get(&crol_eval::Key::Str("schema".into()))?
        .kind
    {
        crol_eval::ValueKind::String(name) => Some(name.clone()),
        _ => panic!("{}: `schema` is a string", case.display()),
    }
}

fn load(case: &Path) -> CrolData {
    let mut layer_dirs: Vec<PathBuf> = fs::read_dir(case.join("layers"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    layer_dirs.sort();
    let mut composer = CrolComposer::new();
    for dir in layer_dirs {
        // Named by its directory, so that files show as `1-base/a.crol`.
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let mut files = Vec::new();
        crol_files(&dir, &dir, &mut files);
        let documents: Vec<(String, String)> = files
            .into_iter()
            .map(|(relative, path)| (relative, fs::read_to_string(path).unwrap()))
            .collect();
        composer = composer.load_layer(CrolLayer::from_sources(name, documents));
    }
    composer
        .compose_data()
        .unwrap_or_else(|e| panic!("{}: {e}", case.display()))
}

/// The message without the contexts it occurs in: `in {prod}: ...`.
fn without_contexts(message: &str) -> &str {
    match message.strip_prefix("in {") {
        Some(rest) => rest.split_once("}: ").map_or(message, |(_, m)| m),
        None => message,
    }
}

#[test]
fn check_and_deserializing_agree() {
    let update = std::env::var_os("UPDATE_EXPECT").is_some();
    let mut cases: Vec<PathBuf> = fs::read_dir(cases_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    cases.sort();
    let mut ran = 0;
    let mut failures = Vec::new();
    for case in &cases {
        let Some(schema_name) = schema_of(case) else {
            continue;
        };
        assert_eq!(schema_name, "services", "{}", case.display());
        ran += 1;
        let name = case.file_name().unwrap().to_string_lossy().into_owned();
        let data = load(case);
        let schema = crol::schema::build::<Definition>();
        let checked = crol::check(&data, &schema, None, &Default::default());

        // Every error deserializing gives, in every context, is one the
        // check gives, and the other way round; ids only the check sees.
        let mut deserialized = Vec::new();
        let contexts: Vec<_> = data.contexts().collect();
        for context in &contexts {
            for key in data.keys_in(context) {
                if let Err(error) = data.get_in::<Definition>(&key, context) {
                    let found = (error.message().to_owned(), error.span());
                    if !deserialized.contains(&found) {
                        deserialized.push(found);
                    }
                }
            }
        }
        let mut from_check: Vec<(String, Option<crol_eval::Span>)> = checked
            .iter()
            .filter(|d| !d.message.contains("the id `@"))
            .map(|d| (without_contexts(&d.message).to_owned(), Some(d.span)))
            .collect();
        from_check.dedup();
        deserialized.sort_by_key(|(m, s)| (s.map(|s| (s.file, s.range.start())), m.clone()));
        from_check.sort_by_key(|(m, s)| (s.map(|s| (s.file, s.range.start())), m.clone()));
        if deserialized != from_check {
            failures.push(format!(
                "{name}: the check and deserializing disagree\n--- check\n{from_check:#?}\n--- deserializing\n{deserialized:#?}"
            ));
        }

        let mut errors = String::new();
        for diagnostic in &checked {
            let _ = writeln!(
                errors,
                "{}: {}",
                data.sources().location(diagnostic.span),
                diagnostic.message
            );
        }
        let path = case.join("expected.schema-errors");
        let expected = fs::read_to_string(&path).ok();
        let errors = (!errors.is_empty()).then_some(errors);
        if update {
            match &errors {
                Some(errors) => fs::write(&path, errors).unwrap(),
                None if path.exists() => fs::remove_file(&path).unwrap(),
                None => {}
            }
        } else if errors != expected {
            failures.push(format!(
                "{name}: schema errors differ\n--- expected\n{}\n--- actual\n{}",
                expected.unwrap_or_default(),
                errors.unwrap_or_default()
            ));
        }
    }
    assert!(ran > 0, "no case has a schema");
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
