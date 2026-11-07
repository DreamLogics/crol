//! The conformance suite (`SPEC.md` 14.2), in `tests/cases/` at the root of
//! the repository.
//!
//! Each case is a directory:
//!
//! - `layers/<n>-<name>/`: the layers, in the order of `<n>`, each holding
//!   `.crod` files at any depth;
//! - `options.crol` (optional): options, as a value-mode document:
//!   `variants: [Grouped(["dev", "prod"]), Discrete("cache")]`, `parents: {
//!   prod_eu: "prod" }`, `contexts: [["prod", "cache"]]`, `variant_keys:
//!   NoNewKeys`, `max_contexts: 16`, and
//!   `schema: "services"` for `crol`'s test of the schema checks;
//! - `expected.crod`: the evaluated data as `crol eval --all-contexts`
//!   prints it, if evaluation succeeds;
//! - `expected.fingerprint`: the data's fingerprint in hex, so that CI
//!   checks that every platform evaluates the same (`SPEC.md` 7.6, 14.2);
//! - `expected.errors`: the diagnostics, one per line, as
//!   `<layer dir>/<path>:line:col: message`, if there are any.
//!
//! `UPDATE_EXPECT=1 cargo test` writes the expected files; review the diff.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use crol_eval::{
    DataOptions, Key, Layer, Severity, Sources, Value, ValueKind, VariantKeys, Variants,
    evaluate_data, evaluate_value, print,
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
        } else if path.extension().is_some_and(|e| e == "crod") {
            let relative = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.push((relative, path));
        }
    }
}

fn strings(value: &Value) -> Vec<String> {
    match &value.kind {
        ValueKind::List(items) => items
            .iter()
            .map(|item| match &item.kind {
                ValueKind::String(s) => s.to_string(),
                _ => panic!("expected a string"),
            })
            .collect(),
        _ => panic!("expected a list of strings"),
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
    let evaluation = evaluate_value(&sources, file);
    let value = evaluation
        .value
        .unwrap_or_else(|| panic!("options.crol: {:?}", evaluation.diagnostics));
    let ValueKind::Keyed(keyed) = value.kind else {
        panic!("options.crol is not a keyed value")
    };
    let mut variants = Variants::new();
    let mut parents = Vec::new();
    for (key, value) in keyed.entries {
        let Key::Str(key) = key else {
            panic!("integer key in options.crol")
        };
        match (&*key, &value.kind) {
            ("max_contexts", ValueKind::Int(i)) => options.max_contexts = *i as usize,
            ("variant_keys", ValueKind::UnitName(rule)) => {
                options.variant_keys = match &**rule {
                    "Any" => VariantKeys::Any,
                    "NoNewKeys" => VariantKeys::NoNewKeys,
                    "SameKeys" => VariantKeys::SameKeys,
                    other => panic!("unknown variant_keys {other}"),
                }
            }
            ("variants", ValueKind::List(items)) => {
                for item in items {
                    match &item.kind {
                        ValueKind::NamedTuple(kind, args) if &**kind == "Grouped" => {
                            variants.add_grouped(strings(&args[0])).unwrap();
                        }
                        ValueKind::NamedTuple(kind, args) if &**kind == "Discrete" => {
                            let ValueKind::String(name) = &args[0].kind else {
                                panic!()
                            };
                            variants.add_discrete(name).unwrap();
                        }
                        ValueKind::NamedTuple(kind, args) if &**kind == "Open" => {
                            let ValueKind::String(name) = &args[0].kind else {
                                panic!()
                            };
                            variants.add_open_group(name).unwrap();
                        }
                        _ => panic!(
                            "variants are `Grouped([...])`, `Discrete(\"...\")` or `Open(\"...\")`"
                        ),
                    }
                }
            }
            ("parents", ValueKind::Keyed(map)) => {
                for (child, parent) in &map.entries {
                    let (Key::Str(child), ValueKind::String(parent)) = (child, &parent.kind) else {
                        panic!("parents map names to names")
                    };
                    parents.push((child.clone(), parent.clone()));
                }
            }
            // For `crol`'s conformance test.
            ("schema", ValueKind::String(_)) => {}
            ("contexts", ValueKind::List(items)) => {
                options.contexts = Some(items.iter().map(strings).collect());
            }
            (key, _) => panic!("unknown option {key} in {}", path.display()),
        }
    }
    for (child, parent) in parents {
        variants.add_parent(&child, &parent).unwrap();
    }
    options.variants = variants;
    options
}

/// Runs one case and returns what it produced: the data, its fingerprint,
/// and the diagnostics.
fn run(case: &Path) -> (Option<String>, Option<String>, String) {
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
    // On threads, the result is the same, diagnostics included.
    let threaded = evaluate_data(&sources, &layers, &options(case).with_threads(Some(4)));
    assert_eq!(
        threaded.diagnostics,
        evaluation.diagnostics,
        "{}: diagnostics differ on threads",
        case.display()
    );
    match (&threaded.value, &evaluation.value) {
        (Some(a), Some(b)) => {
            assert!(
                a.identical(b),
                "{}: the base differs on threads",
                case.display()
            );
            assert_eq!(threaded.contexts.len(), evaluation.contexts.len());
            for (a, b) in threaded.contexts.iter().zip(&evaluation.contexts) {
                assert_eq!(a.context, b.context);
                assert!(
                    a.root(threaded.value.as_ref().unwrap())
                        .identical(&b.root(b_value(&evaluation))),
                    "{}: a context differs on threads",
                    case.display()
                );
            }
        }
        (None, None) => {}
        _ => panic!(
            "{}: only one of the evaluations has a value",
            case.display()
        ),
    }
    let data = evaluation.value.as_ref().map(|value| {
        print::to_all_contexts(
            value,
            &evaluation.contexts,
            &evaluation.variants,
            &Default::default(),
        )
    });
    let fingerprint = evaluation.value.as_ref().map(|value| {
        let hash =
            crol_eval::fingerprint::fingerprint(value, &evaluation.contexts, &evaluation.variants);
        hash.iter().map(|b| format!("{b:02x}")).collect::<String>() + "\n"
    });
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
    (data, fingerprint, errors)
}

fn b_value(evaluation: &crol_eval::Evaluation) -> &Value {
    evaluation.value.as_ref().unwrap()
}

fn copy_formatted(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_formatted(&path, &target);
        } else if path.extension().is_some_and(|e| e == "crod") {
            let text = fs::read_to_string(&path).unwrap();
            let config = crol_syntax::format::FormatConfig::default();
            let text = crol_syntax::format::format(&text, &config).unwrap_or(text);
            fs::write(target, text).unwrap();
        } else {
            fs::copy(&path, target).unwrap();
        }
    }
}

/// Formatting changes no document's meaning (`SPEC.md` 12.2), now that it
/// leaves out commas: every case has the same data and fingerprint after it.
#[test]
fn formatting_keeps_data_and_fingerprints() {
    let formatted = Path::new(env!("CARGO_TARGET_TMPDIR")).join("conformance-formatted");
    let _ = fs::remove_dir_all(&formatted);
    for entry in fs::read_dir(cases_dir()).unwrap() {
        let case = entry.unwrap().path();
        if !case.is_dir() {
            continue;
        }
        let copy = formatted.join(case.file_name().unwrap());
        copy_formatted(&case, &copy);
        let (data, fingerprint, _) = run(&case);
        let (data_after, fingerprint_after, _) = run(&copy);
        assert_eq!(data, data_after, "{}", case.display());
        assert_eq!(fingerprint, fingerprint_after, "{}", case.display());
    }
}

/// Every case with a fingerprint evaluates the same with its layers read
/// from `.crob` files (`SPEC.md` 7.8). A layer that cannot be compiled alone,
/// for using a template of another, stays text.
/// A case's layers, and its fingerprint, if it has both.
fn fingerprinted(case: &Path) -> Option<(Sources, Vec<Layer>, String)> {
    let expected = fs::read_to_string(case.join("expected.fingerprint")).ok()?;
    let dirs = fs::read_dir(case.join("layers")).ok()?;
    let mut dirs: Vec<PathBuf> = dirs.map(|e| e.unwrap().path()).collect();
    dirs.sort();
    let mut sources = Sources::new();
    let mut layers = Vec::new();
    for dir in &dirs {
        let mut files = Vec::new();
        crol_files(dir, dir, &mut files);
        let documents = files
            .into_iter()
            .map(|(relative, path)| {
                let file = sources.add(relative.clone(), fs::read_to_string(path).unwrap());
                (relative, file)
            })
            .collect();
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        layers.push(Layer { name, documents });
    }
    Some((sources, layers, expected))
}

/// That layers evaluate to a fingerprint, with `lowered` for some of them.
fn assert_evaluates(
    case: &Path,
    sources: &Sources,
    layers: &[Layer],
    lowered: &[Option<&[u8]>],
    expected: &str,
) {
    let (compiled, diagnostics) = crol_eval::compile_binary(sources, layers, lowered);
    let compiled = compiled.unwrap_or_else(|| panic!("{case:?}: {diagnostics:?}"));
    let evaluation = crol_eval::evaluate_compiled(&compiled, &options(case));
    let base = evaluation
        .value
        .as_ref()
        .unwrap_or_else(|| panic!("{case:?}: {:?}", evaluation.diagnostics));
    let hash =
        crol_eval::fingerprint::fingerprint(base, &evaluation.contexts, &evaluation.variants);
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(hex, expected.trim(), "{case:?}");
}

#[test]
fn compiled_layers_evaluate_the_same() {
    let mut compared = 0;
    for entry in fs::read_dir(cases_dir()).unwrap() {
        let case = entry.unwrap().path();
        let Some((sources, layers, expected)) = fingerprinted(&case) else {
            continue;
        };
        let encoded: Vec<Option<Vec<u8>>> = layers
            .iter()
            .map(|layer| crol_eval::encode_layer(&sources, layer).ok())
            .collect();
        let read: Vec<Option<crol_eval::binary::BinaryLayer>> = encoded
            .iter()
            .map(|bytes| {
                let bytes = bytes.as_ref()?;
                Some(crol_eval::binary::read_layer(bytes).unwrap())
            })
            .collect();
        let lowered: Vec<Option<&[u8]>> = read
            .iter()
            .map(|layer| layer.as_ref().map(|l| &*l.lowered))
            .collect();
        compared += usize::from(lowered.iter().any(Option::is_some));
        assert_evaluates(&case, &sources, &layers, &lowered, &expected);
    }
    assert!(compared > 10, "only {compared} cases compared");
}

/// What a reload does: each layer that stands alone comes lowered, and the
/// layer that changed comes as text, whichever one that is.
#[test]
fn layers_lowered_for_a_reload_evaluate_the_same() {
    let mut compared = 0;
    for entry in fs::read_dir(cases_dir()).unwrap() {
        let case = entry.unwrap().path();
        let Some((sources, layers, expected)) = fingerprinted(&case) else {
            continue;
        };
        let bytes: Vec<Option<Vec<u8>>> = layers
            .iter()
            .map(|layer| crol_eval::lower_layer(&sources, layer))
            .collect();
        for changed in 0..=layers.len() {
            let lowered: Vec<Option<&[u8]>> = bytes
                .iter()
                .enumerate()
                .map(|(index, bytes)| bytes.as_deref().filter(|_| index != changed))
                .collect();
            compared += usize::from(lowered.iter().any(Option::is_some));
            assert_evaluates(&case, &sources, &layers, &lowered, &expected);
        }
    }
    assert!(compared > 20, "only {compared} cases compared");
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
        let (data, fingerprint, errors) = run(case);
        let expected_data = fs::read_to_string(case.join("expected.crod")).ok();
        let expected_fingerprint = fs::read_to_string(case.join("expected.fingerprint")).ok();
        let expected_errors = fs::read_to_string(case.join("expected.errors")).ok();
        let errors = (!errors.is_empty()).then_some(errors);
        if update {
            for (file, content) in [
                ("expected.crod", &data),
                ("expected.fingerprint", &fingerprint),
                ("expected.errors", &errors),
            ] {
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
        if fingerprint != expected_fingerprint {
            failures.push(format!(
                "{name}: the fingerprint differs: expected {}, found {}",
                expected_fingerprint.unwrap_or_default().trim(),
                fingerprint.unwrap_or_default().trim()
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
