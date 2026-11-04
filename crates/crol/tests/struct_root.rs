//! A struct as the root type: the whole document, across layers (`SPEC.md`
//! 4.1, 7.1, 10.3).

#![allow(missing_docs)]

use crol::schema::{self, SchemaFile};
use crol::{CheckOptions, CrolComposer, CrolData, CrolLayer};
use crol_derive::Schema;
use indexmap::IndexMap;
use serde_derive::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
pub struct Options {
    pub a: bool,
    #[serde(default)]
    pub b: bool,
}

#[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
#[crol(defaults)]
pub struct Config {
    pub name: String,
    pub options: Options,
    #[serde(default = "three")]
    pub retries: u32,
    pub note: Option<String>,
}

fn three() -> u32 {
    3
}

/// Fields take their defaults only with `..`, whatever the options say.
#[derive(Debug, PartialEq, Deserialize, Schema)]
#[crol(explicit_defaults)]
pub struct Strict {
    pub name: String,
    #[serde(default)]
    pub retries: u32,
}

/// Fields take their defaults without `..`.
#[derive(Debug, PartialEq, Deserialize, Schema)]
#[crol(explicit_defaults = false)]
pub struct Relaxed {
    pub name: String,
    #[serde(default)]
    pub retries: u32,
}

#[derive(Debug, PartialEq, Deserialize, Schema)]
#[serde(transparent)]
pub struct Wrapped(Config);

#[derive(Debug, PartialEq, Deserialize, Schema)]
#[serde(transparent)]
pub struct Named(IndexMap<String, Options>);

#[derive(Debug, PartialEq, Deserialize, Schema)]
#[serde(transparent)]
pub struct Port(u16);

#[derive(Debug, PartialEq, Deserialize, Schema)]
pub enum Definition {
    Options(Options),
    Plain { name: String },
}

#[derive(Debug, PartialEq, Deserialize, Schema)]
pub struct Common {
    pub level: u32,
}

#[derive(Debug, PartialEq, Deserialize, Schema)]
pub struct Flattened {
    pub name: String,
    #[serde(flatten)]
    pub common: Common,
}

#[derive(Debug, PartialEq, Deserialize, Schema)]
#[serde(deny_unknown_fields)]
pub struct Closed {
    pub name: String,
}

fn composer(layers: &[&str]) -> CrolComposer {
    let mut composer = CrolComposer::new();
    for (i, text) in layers.iter().enumerate() {
        let name = format!("layer{i}");
        composer = composer.load_layer(CrolLayer::from_sources(
            name,
            [("a.crod".to_owned(), (*text).to_owned())],
        ));
    }
    composer
}

fn data(layers: &[&str], schema: &SchemaFile) -> CrolData {
    composer(layers)
        .schema(schema.clone())
        .compose_data()
        .unwrap()
}

fn errors(layers: &[&str], schema: &SchemaFile) -> Vec<String> {
    errors_with(layers, schema, None, &CheckOptions::default())
}

fn errors_with(
    layers: &[&str],
    schema: &SchemaFile,
    root: Option<&str>,
    options: &CheckOptions,
) -> Vec<String> {
    let data = data(layers, schema);
    crol::check(&data, schema, root, options)
        .into_iter()
        .map(|d| d.message)
        .collect()
}

const WHOLE: &str = "name: \"x\", options: { a: true, b: false }, retries: 1, note: None";

fn whole() -> Config {
    Config {
        name: "x".to_owned(),
        options: Options { a: true, b: false },
        retries: 1,
        note: None,
    }
}

#[test]
fn a_root_key_is_a_field_of_a_struct_root() {
    let schema = schema::build::<Config>();
    assert_eq!(errors(&[WHOLE], &schema), Vec::<String>::new());
    let config: Config = composer(&[WHOLE]).compose().unwrap();
    assert_eq!(config, whole());
}

#[test]
fn only_the_composed_root_is_complete() {
    let schema = schema::build::<Config>();
    let layers = [
        "name: \"x\"",
        "options: { a: true, .. }, retries: 1",
        "note: None",
    ];
    assert_eq!(errors(&layers, &schema), Vec::<String>::new());
    let config: Config = composer(&layers).compose().unwrap();
    assert_eq!(config, whole());
    // The first alone is not.
    let found = errors(&layers[..1], &schema);
    assert_eq!(
        found,
        [
            "`Config` is missing the fields `options`, `retries`, `note`; write them, or end the block with `..` to leave out fields that have defaults"
        ]
    );
}

#[test]
fn check_and_compose_put_a_missing_field_on_the_top_document() {
    let schema = schema::build::<Config>();
    let layers = ["name: \"x\"", "\n\nnote: None"];
    let data = data(&layers, &schema);
    let found = crol::check(&data, &schema, None, &CheckOptions::default());
    assert_eq!(found.len(), 1);
    assert!(!found[0].span.is_detached());
    let location = data.sources().location(found[0].span);
    assert_eq!(location.line_col.line, 1);
    let error = composer(&layers)
        .schema(schema)
        .compose::<Config>()
        .unwrap_err();
    assert_eq!(error.span(), Some(found[0].span));
    assert_eq!(error.message(), found[0].message);
}

#[test]
fn an_unknown_root_key_lists_the_fields() {
    let schema = schema::build::<Config>();
    let text = format!("{WHOLE}, foo: {{ bar: 2 }}");
    let wanted = "`Config` has no field `foo`; its fields are `name`, `options`, `retries`, `note`";
    assert_eq!(errors(&[&text], &schema), [wanted]);
    let error = composer(&[&text]).compose::<Config>().unwrap_err();
    assert_eq!(error.message(), wanted);
    // Unless they are allowed, as in a block.
    let mut options = CheckOptions::default();
    options.allow_unknown_fields = true;
    assert_eq!(
        errors_with(&[&text], &schema, None, &options),
        Vec::<String>::new()
    );
}

#[test]
fn a_template_is_no_field() {
    let schema = schema::build::<Config>();
    let text = format!("t := {{ a: true, b: false }}\n{WHOLE}");
    assert_eq!(errors(&[&text], &schema), Vec::<String>::new());
}

#[test]
fn root_defaults_let_fields_be_left_out() {
    let schema = schema::build::<Config>();
    let layers = ["name: \"x\", options: { a: true, .. }", ".."];
    assert_eq!(errors(&layers, &schema), Vec::<String>::new());
    let config: Config = composer(&layers).compose().unwrap();
    assert_eq!(
        config,
        Config {
            retries: 3,
            ..Config {
                options: Options { a: true, b: false },
                ..whole()
            }
        }
    );
    // A field without a default is still missing.
    let layers = ["options: { a: true, .. }", ".."];
    assert_eq!(errors(&layers, &schema), ["missing field `name`"]);
    let error = composer(&layers).compose::<Config>().unwrap_err();
    assert_eq!(error.message(), "missing field `name`");
}

#[test]
fn root_defaults_in_a_variant_hold_in_that_context() {
    let schema = schema::build::<Config>();
    let text = "name: \"x\", options: { a: true, .. }, <prod> { .. }";
    let found = errors(&[text], &schema);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].starts_with("in {}: `Config` is missing"),
        "{found:?}"
    );
    let config: Config = composer(&[text])
        .with_variant("prod")
        .unwrap()
        .compose()
        .unwrap();
    assert_eq!(config.retries, 3);
    assert!(composer(&[text]).compose::<Config>().is_err());
}

#[test]
fn a_spread_at_the_root_stays_an_error() {
    let error = composer(&["t := { a: 1 }\n..&t"])
        .compose::<IndexMap<String, u32>>()
        .unwrap_err();
    assert!(
        error.to_string().contains("a spread cannot be at the root"),
        "{error}"
    );
}

#[test]
fn removing_a_field_is_as_in_a_block() {
    let schema = schema::build::<Config>();
    // It takes its default, without `..`.
    let layers = [WHOLE, "-retries, -note"];
    assert_eq!(errors(&layers, &schema), Vec::<String>::new());
    let config: Config = composer(&layers).compose().unwrap();
    assert_eq!(
        config,
        Config {
            retries: 3,
            ..whole()
        }
    );
    // A required one is missing.
    let layers = [WHOLE, "-name"];
    assert_eq!(errors(&layers, &schema), ["missing field `name`"]);
    assert!(composer(&layers).compose::<Config>().is_err());
    // Set again, it is back.
    let layers = [WHOLE, "-name", "name: \"y\""];
    assert_eq!(errors(&layers, &schema), Vec::<String>::new());
    // Only in one context.
    let layers = [WHOLE, "<prod> { -retries }"];
    assert_eq!(errors(&layers, &schema), Vec::<String>::new());
    let config: Config = composer(&layers)
        .with_variant("prod")
        .unwrap()
        .compose()
        .unwrap();
    assert_eq!(config.retries, 3);
}

#[test]
fn ensure_and_declare_work_on_fields() {
    let schema = schema::build::<Config>();
    let layers = [WHOLE, "?retries: 9"];
    let config: Config = composer(&layers).compose().unwrap();
    assert_eq!(config.retries, 1);
    assert_eq!(errors(&layers, &schema), Vec::<String>::new());
    let error = composer(&[WHOLE, "+name: \"y\""])
        .compose::<Config>()
        .unwrap_err();
    assert!(error.to_string().contains("already exists"), "{error}");
}

#[test]
fn explicit_defaults_and_require_complete_decide_as_in_a_block() {
    let schema = schema::build::<Strict>();
    let mut loose = CheckOptions::default();
    loose.require_complete = false;
    // The type comes first: `..` is needed even with `require_complete` off.
    let found = errors_with(&["name: \"x\""], &schema, None, &loose);
    assert_eq!(found.len(), 1, "{found:?}");
    let layers = ["name: \"x\"", ".."];
    assert_eq!(
        errors_with(&layers, &schema, None, &loose),
        Vec::<String>::new()
    );
    let strict: Strict = composer(&layers).schema(schema).compose().unwrap();
    assert_eq!(strict.retries, 0);

    let schema = schema::build::<Relaxed>();
    assert_eq!(errors(&["name: \"x\""], &schema), Vec::<String>::new());
    let relaxed: Relaxed = composer(&["name: \"x\""])
        .schema(schema.clone())
        .compose()
        .unwrap();
    assert_eq!(relaxed.retries, 0);
    // `..` is allowed, and changes nothing.
    assert_eq!(errors(&["name: \"x\", .."], &schema), Vec::<String>::new());
    assert_eq!(errors(&["retries: 1"], &schema), ["missing field `name`"]);

    // A type that does not say: `require_complete` does.
    let schema = schema::build::<Config>();
    let layers = ["name: \"x\", options: { a: true, b: true }"];
    assert_eq!(
        errors_with(&layers, &schema, None, &loose),
        Vec::<String>::new()
    );
    assert_eq!(errors(&layers, &schema).len(), 1);
}

#[test]
fn wrappers_are_what_they_wrap() {
    let schema = schema::build::<Wrapped>();
    assert_eq!(errors(&[WHOLE], &schema), Vec::<String>::new());
    let found = errors(&["name: 1, options: { a: true, .. }, .."], &schema);
    assert_eq!(found.len(), 1, "{found:?}");
    let found = errors(&[&format!("{WHOLE}, foo: 1")], &schema);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("has no field `foo`"), "{found:?}");

    // A map: a root key is a key, and each root value a value of it.
    let schema = schema::build::<Named>();
    let text = "one: { a: true, b: false }, two: { a: false, .. }";
    assert_eq!(errors(&[text], &schema), Vec::<String>::new());
    assert_eq!(errors(&["one: 1"], &schema).len(), 1);
    let named: Named = composer(&[text]).compose().unwrap();
    assert_eq!(named.0.len(), 2);

    // A scalar has no keys: it is the type of each root value.
    let schema = schema::build::<Port>();
    assert_eq!(errors(&["a: 1, b: 2"], &schema), Vec::<String>::new());
    assert_eq!(errors(&["a: \"x\""], &schema).len(), 1);
}

#[test]
fn a_map_as_the_root_type_is_checked() {
    let schema = schema::build::<IndexMap<String, Options>>();
    let text = "one: { a: true, b: false }";
    assert_eq!(errors(&[text], &schema), Vec::<String>::new());
    assert_eq!(errors(&["one: 1"], &schema).len(), 1);
    // And reads back from its file.
    let text = schema.to_text().unwrap();
    assert_eq!(SchemaFile::from_text(&text).unwrap(), schema);
}

#[test]
fn an_enum_is_the_type_of_each_root_value() {
    let schema = schema::build::<Definition>();
    let text = "a: Options { a: true, b: true }, b: Plain { name: \"x\" }";
    assert_eq!(errors(&[text], &schema), Vec::<String>::new());
    let found = errors(&["name: \"x\""], &schema);
    assert_eq!(found, ["expected a `Definition` variant, found \"x\""]);
    // `..` at the root says nothing about them.
    assert_eq!(errors(&[text, ".."], &schema), Vec::<String>::new());
    let all: IndexMap<String, Definition> = composer(&[text, ".."]).compose().unwrap();
    assert_eq!(all.len(), 2);
}

#[test]
fn the_root_argument_chooses_the_type_and_its_kind_the_rule() {
    let schema = schema::build::<Definition>();
    // The struct `Options`, by path: the whole document.
    let path = schema
        .type_path("Options")
        .expect("the schema has it")
        .to_owned();
    let options = CheckOptions::default();
    assert_eq!(
        errors_with(&["a: true, b: false"], &schema, Some(&path), &options),
        Vec::<String>::new()
    );
    assert_eq!(
        errors_with(&["a: true, b: false, c: 1"], &schema, Some(&path), &options).len(),
        1
    );
}

#[test]
fn flatten_and_deny_unknown_fields_on_the_root() {
    let schema = schema::build::<Flattened>();
    assert_eq!(
        errors(&["name: \"x\", level: 1"], &schema),
        Vec::<String>::new()
    );
    let root: Flattened = composer(&["name: \"x\", level: 1"]).compose().unwrap();
    assert_eq!(root.common.level, 1);

    let schema = schema::build::<Closed>();
    let found = errors(&["name: \"x\", more: 1"], &schema);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        composer(&["name: \"x\", more: 1"])
            .compose::<Closed>()
            .is_err()
    );
}

#[test]
fn default_reads_the_field_at_a_struct_root() {
    let schema = schema::build::<Config>();
    let text = "name: \"x\", options: { a: true, .. }, retries: &default + 1, note: None";
    let config: Config = composer(&[text]).schema(schema).compose().unwrap();
    assert_eq!(config.retries, 4);
}

#[test]
fn fill_defaults_fills_the_root() {
    let schema = schema::build::<Config>();
    let data = data(
        &["name: \"x\", options: { a: true, .. }, note: None", ".."],
        &schema,
    );
    let root = data.view().base.clone();
    let span = crol_eval::Span::detached();
    let filled = crol::fill_defaults(&root, &schema, None, span);
    let crol_eval::ValueKind::Keyed(keyed) = &filled.kind else {
        panic!("the root is keyed")
    };
    let retries = &keyed.entries[&crol_eval::Key::Str("retries".into())];
    assert!(matches!(retries.kind, crol_eval::ValueKind::Int(3)));
    // Every field is written now.
    assert!(!keyed.defaults);
}

#[test]
fn the_fingerprint_changes_only_with_a_root_defaults() {
    let plain = data(&[WHOLE], &schema::build::<Config>());
    let same = data(&[WHOLE, "name: \"x\""], &schema::build::<Config>());
    let with = data(&[WHOLE, ".."], &schema::build::<Config>());
    assert_eq!(plain.fingerprint(), same.fingerprint());
    assert_ne!(plain.fingerprint(), with.fingerprint());
    let variant = data(&[WHOLE, "<prod> { .. }"], &schema::build::<Config>());
    assert_ne!(variant.fingerprint(), with.fingerprint());
}
