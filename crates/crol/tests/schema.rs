//! Schemas (`SPEC.md` 10.9, 10.10) and the derives.

#![allow(missing_docs)]

use crol::schema::{self, FieldDefault, SchemaFile, TypeDef, TypeExpr};
use crol_derive::{Schema, VariantName};
use serde_derive::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
#[serde(default)]
#[crol(defaults)]
/// How much a service may use.
pub struct Limits {
    pub cpu: f64,
    pub memory_mb: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            cpu: 1.0,
            memory_mb: 512,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Schema)]
#[crol(defaults)]
/// A service that runs one container image.
pub struct Service {
    /// Number of copies to run.
    pub replicas: u32,
    #[crol(refers(Service, Volume))]
    pub depends_on: Vec<crol::Id>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub limits: Limits,
    #[serde(rename = "url")]
    pub endpoint: Option<String>,
    #[serde(skip)]
    pub cache: u8,
}

#[derive(Debug, Deserialize, Serialize, Schema)]
pub struct Volume {
    pub size_gb: u32,
}

#[derive(Debug, Deserialize, Serialize, Schema)]
/// Everything the data can define.
pub enum Definition {
    Service(Service),
    Volume(Volume),
}

#[derive(Debug, Deserialize, Serialize, Schema)]
pub struct Pair<A, B>(A, B);

#[derive(Debug, Deserialize, Serialize, Schema)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum Shape {
    Circle { radius: f64 },
    BigBox,
}

#[derive(Debug, Deserialize, Schema)]
pub struct Renamed {
    #[serde(alias = "old", alias = "older")]
    pub new: u8,
}

#[test]
fn aliases_are_recorded() {
    let schema = schema::build::<Renamed>();
    let Some(TypeDef::Struct { fields, .. }) = schema.get("schema::Renamed") else {
        panic!()
    };
    assert_eq!(fields["new"].aliases, ["old", "older"]);
}

#[derive(Debug, Deserialize, Schema)]
pub struct Moved {
    #[serde(alias = "host")]
    pub address: String,
    pub port: u16,
}

#[derive(Debug, Deserialize, Schema)]
pub enum Moves {
    Moved(Moved),
}

#[test]
fn a_field_written_by_an_alias_is_complete() {
    let compose = |text: &str, with_schema: bool| {
        let mut composer = crol::CrolComposer::new()
            .load_layer(crol::CrolLayer::from_sources("base", [("a.crol", text)]));
        if with_schema {
            composer = composer.schema(schema::build::<Moves>());
        }
        composer.compose_data().unwrap()
    };
    let text = "+a: Moved { host: \"h\", port: 1 },";
    // With the schema, `host` writes `address`.
    let data = compose(text, true);
    let Moves::Moved(moved) = data.get::<Moves>("a").unwrap();
    assert_eq!(moved.address, "h");
    assert!(crol::check(&data, &schema::build::<Moves>(), None, &Default::default()).is_empty());
    // Without it, the deserializer cannot tell `host` is `address`.
    let error = compose(text, false).get::<Moves>("a").unwrap_err();
    assert!(
        error.message().contains("missing the field `address`"),
        "{error}"
    );
    // A field written by no name is still missing.
    let data = compose("+a: Moved { port: 1 },", true);
    let error = data.get::<Moves>("a").unwrap_err();
    assert!(
        error.message().contains("missing the field `address`"),
        "{error}"
    );
    let checked = crol::check(&data, &schema::build::<Moves>(), None, &Default::default());
    assert_eq!(checked.len(), 1, "{checked:?}");
}

#[derive(Debug, Clone, PartialEq, Eq, VariantName)]
enum Env {
    Dev,
    ProdEu,
    #[crol(rename = "qa")]
    Testing,
}

#[test]
fn the_schema_describes_the_types() {
    let schema = schema::build::<Definition>();
    assert_eq!(schema.root, "schema::Definition");
    let Some(TypeDef::Struct {
        fields,
        doc,
        default,
        ..
    }) = schema.get("schema::Service")
    else {
        panic!("{:?}", schema.types.keys().collect::<Vec<_>>())
    };
    assert_eq!(doc, "A service that runs one container image.");
    assert!(default.is_none(), "Service has no #[serde(default)]");
    assert_eq!(fields["replicas"].doc, "Number of copies to run.");
    assert_eq!(fields["replicas"].default, FieldDefault::Required);
    assert_eq!(
        fields["depends_on"].ty,
        TypeExpr::List(Box::new(TypeExpr::Id(vec![
            "schema::Service".into(),
            "schema::Volume".into()
        ])))
    );
    assert!(!fields.contains_key("cache"));
    assert!(matches!(&fields["tags"].default, FieldDefault::Value(v) if v.to_text() == "[]"));
    assert!(
        matches!(&fields["limits"].default, FieldDefault::Value(v) if v.to_text() == "Limits {cpu: 1.0, memory_mb: 512}")
    );
    assert!(matches!(&fields["url"].default, FieldDefault::Value(v) if v.to_text() == "None"));

    let Some(TypeDef::Struct {
        default: Some(default),
        ..
    }) = schema.get("schema::Limits")
    else {
        panic!()
    };
    assert_eq!(default.to_text(), "Limits {cpu: 1.0, memory_mb: 512}");
}

#[test]
fn generics_renames_and_tags() {
    let schema = schema::build::<Pair<u32, Shape>>();
    assert_eq!(schema.root, "schema::Pair<U32, schema::Shape>");
    let Some(TypeDef::Enum { variants, repr, .. }) = schema.get("schema::Shape") else {
        panic!()
    };
    assert_eq!(variants.keys().collect::<Vec<_>>(), ["circle", "big-box"]);
    assert_eq!(*repr, schema::Repr::Internal { tag: "kind".into() });
}

#[test]
fn variant_names() {
    use crol::VariantName as _;
    assert_eq!(Env::ProdEu.name(), "prod_eu");
    assert_eq!(Env::Testing.name(), "qa");
    assert_eq!(Env::from_name("dev"), Some(Env::Dev));
    assert_eq!(Env::from_name("testing"), None);
}

#[test]
fn schema_files_read_back() {
    let schema = schema::build::<Definition>();
    let text = schema.to_text().unwrap();
    expect_test::expect_file!["schema.expected.crol"].assert_eq(&text);
    let read = SchemaFile::from_text(&text).unwrap();
    assert_eq!(read, schema);
}

#[test]
fn check_file_writes_then_compares() {
    let path = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("schema/check.crol");
    let _ = std::fs::remove_file(&path);
    schema::check_file::<Definition>(&path).unwrap();
    schema::check_file::<Definition>(&path).unwrap();
    std::fs::write(&path, "stale").unwrap();
    let error = schema::check_file::<Definition>(&path).unwrap_err();
    assert!(
        error.to_string().contains("CROL_UPDATE_SCHEMA=1"),
        "{error}"
    );
}

/// Data with one error: `crol::check` and deserializing report it with the
/// same message and span (`SPEC.md` 10.9).
#[test]
fn check_agrees_with_the_deserializer() {
    use std::collections::BTreeMap;

    let cases = [
        r#"+web: Service { replicas: -1, depends_on: [], url: None, .. },"#,
        r#"+web: Service { replicas: 1, depends_on: [], url: None, extra: 1, .. },"#,
        r#"+web: Service { replicas: 1, depends_on: [], url: None },"#,
        r#"+web: Limits { cpu: 1.0, .. },"#,
        r#"+web: Service { replicas: 1, depends_on: ["db"], url: None, .. },"#,
        r#"+web: Service { replicas: 1, depends_on: [], url: 3, .. },"#,
        r#"+data: Volume { size_gb: "big" },"#,
        r#"+data: Unknown { size_gb: 1 },"#,
    ];
    let schema = schema::build::<Definition>();
    for case in cases {
        let data = crol::CrolComposer::new()
            .load_layer(crol::CrolLayer::from_sources("base", [("a.crol", case)]))
            .compose_data()
            .unwrap();
        let checked = crol::check(&data, &schema, None, &Default::default());
        let deserialized = data
            .deserialize_root::<BTreeMap<String, Definition>>(None)
            .unwrap_err();
        assert_eq!(checked.len(), 1, "{case}: {checked:?}");
        assert_eq!(checked[0].message, deserialized.message(), "{case}");
        assert_eq!(Some(checked[0].span), deserialized.span(), "{case}");
    }
}

#[test]
fn check_validates_ids() {
    let schema = schema::build::<Definition>();
    let data = crol::CrolComposer::new()
        .load_layer(crol::CrolLayer::from_sources(
            "base",
            [(
                "a.crol",
                r#"
                +web: Service { replicas: 1, depends_on: [@db, @nothing, @api], url: None, .. },
                +db: Volume { size_gb: 1 },
                +api: Service { replicas: 1, depends_on: [], url: None, .. },
                "#,
            )],
        ))
        .compose_data()
        .unwrap();
    let messages: Vec<String> = crol::check(&data, &schema, None, &Default::default())
        .into_iter()
        .map(|d| d.message)
        .collect();
    assert_eq!(messages, ["the id `@nothing` names no root key"]);

    let data = crol::CrolComposer::new()
        .load_layer(crol::CrolLayer::from_sources(
            "base",
            [(
                "a.crol",
                "+web: Service { replicas: 1, depends_on: [@n], url: None, .. },\n+n: 5,",
            )],
        ))
        .compose_data()
        .unwrap();
    let messages: Vec<String> = crol::check(&data, &schema, None, &Default::default())
        .into_iter()
        .map(|d| d.message)
        .collect();
    assert_eq!(
        messages,
        [
            "the id `@n` names a value without a type name, but it may only name `Service` or `Volume`",
            "expected a `Definition` variant, found 5",
        ]
    );
}

/// `&default` reads the Rust defaults from the schema (`SPEC.md` 6.1.3).
#[test]
fn default_references() {
    let source = r#"
        +web: Service {
            // `Service` has no struct-level default, so this is missing.
            ..(&default | {}),
            replicas: 2,
            depends_on: [],
            url: None,
            limits: Limits { ..&default, cpu: 2.0 },
            ..
        },
        +big: Service {
            replicas: 1,
            depends_on: [],
            url: None,
            limits: &default ~ { cpu: 4.0 },
            tags: &default + ["big"],
        },
        +nested: Service {
            replicas: 1,
            depends_on: [],
            url: None,
            limits: { cpu: &default * 2, memory_mb: &default },
            ..
        },
        +fallback: Service { replicas: &default | 3, depends_on: [], url: None, .. },
    "#;
    let data = crol::CrolComposer::new()
        .schema(schema::build::<Definition>())
        .load_layer(crol::CrolLayer::from_sources("base", [("a.crol", source)]))
        .compose_data()
        .unwrap();
    let web: Service = data.get("web").unwrap();
    assert_eq!(
        web.limits,
        Limits {
            cpu: 2.0,
            memory_mb: 512
        }
    );
    let big: Service = data.get("big").unwrap();
    assert_eq!((big.limits.cpu, big.limits.memory_mb), (4.0, 512));
    assert_eq!(big.tags, ["big"]);
    let nested: Service = data.get("nested").unwrap();
    assert_eq!((nested.limits.cpu, nested.limits.memory_mb), (2.0, 512));
    // `replicas` has no default, so `&default` is missing and `|` falls back.
    let fallback: Service = data.get("fallback").unwrap();
    assert_eq!(fallback.replicas, 3);

    // Without a schema, `&default` is an error.
    let error = crol::CrolComposer::new()
        .load_layer(crol::CrolLayer::from_sources(
            "base",
            [("a.crol", "+x: { a: &default },")],
        ))
        .compose_data()
        .unwrap_err();
    assert!(error.to_string().contains("no schema is given"), "{error}");
}

#[test]
fn defaults_are_filled_in_as_serde_fills_them() {
    let data = crol::CrolComposer::new()
        .load_layer(crol::CrolLayer::from_sources(
            "base",
            [(
                "a.crol",
                "+web: Service { replicas: 2, depends_on: [], url: None, limits: { cpu: 2.0, memory_mb: 1 }, .. },\n\
                 web: & ~ { -limits },\n\
                 +db: Volume { size_gb: 1 },",
            )],
        ))
        .compose_data()
        .unwrap();
    let mut sources = crol::Sources::new();
    let filled = crol::Span {
        file: sources.add("<default>", ""),
        range: Default::default(),
    };
    let schema = schema::build::<Definition>();
    let root = data.view().base.clone();
    let root = crol::fill_defaults(&root, &schema, None, filled);
    let text = crol_eval::print::to_document_noted(&root, &Default::default(), &|v| {
        (v.span == filled).then(|| "default".to_owned())
    });
    assert_eq!(
        text,
        "web: Service {\n    replicas: 2,\n    depends_on: [],\n    url: None,\n    \
         tags: [], // default\n    limits: Limits { cpu: 1.0, memory_mb: 512 }, // default\n}\n\
         db: Volume { size_gb: 1 }\n"
    );
}

/// Fields left out take their defaults without `..`.
#[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
#[crol(explicit_defaults = false, defaults)]
pub struct Relaxed {
    pub name: String,
    #[serde(default)]
    pub port: u16,
    pub note: Option<String>,
}

/// Fields left out take their defaults only with `..`, whatever
/// `require_complete` says.
#[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
#[crol(explicit_defaults)]
pub struct Strict {
    pub name: String,
    #[serde(default)]
    pub port: u16,
}

/// Neither: `require_complete` says.
#[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
pub struct Plain {
    pub name: String,
    #[serde(default)]
    pub port: u16,
}

/// Every field has a default, from the struct's.
#[derive(Debug, Default, PartialEq, Deserialize, Serialize, Schema)]
#[serde(default)]
#[crol(explicit_defaults = false)]
pub struct AllDefault {
    pub cpu: u32,
    pub label: Option<String>,
}

#[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
#[crol(explicit_defaults = false)]
pub enum Task {
    Run {
        #[serde(default)]
        retries: u32,
    },
    #[crol(explicit_defaults)]
    Stop {
        #[serde(default)]
        force: bool,
    },
    Wait(u32),
}

#[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
pub enum Model {
    Relaxed(Relaxed),
    Strict(Strict),
    Plain(Plain),
    AllDefault(AllDefault),
    Task(Task),
}

/// Composes `text`, with `require_complete` as given, with the schema or
/// without, as a `.crol` layer.
fn model_data(text: &str, require_complete: bool, with_schema: bool) -> crol::CrolData {
    let mut composer = crol::CrolComposer::with_cfg(crol::CrolConfig {
        require_complete,
        ..Default::default()
    })
    .load_layer(crol::CrolLayer::from_sources("base", [("a.crol", text)]));
    if with_schema {
        composer = composer.schema(schema::build::<Model>());
    }
    composer.compose_data().unwrap()
}

/// What deserializing the key `a` says, and what the check says.
fn both(text: &str, require_complete: bool) -> (Option<String>, Option<String>) {
    let data = model_data(text, require_complete, true);
    let mut options = crol::CheckOptions::default();
    options.require_complete = require_complete;
    let found = crol::check(&data, &schema::build::<Model>(), None, &options);
    assert!(found.len() < 2, "{found:?}");
    (
        data.get::<Model>("a").err().map(|e| e.message().to_owned()),
        found.first().map(|d| d.message.clone()),
    )
}

#[track_caller]
fn agree_ok(text: &str, require_complete: bool) {
    assert_eq!(both(text, require_complete), (None, None), "{text}");
}

#[track_caller]
fn agree_error(text: &str, require_complete: bool, message: &str) {
    let (deserialized, checked) = both(text, require_complete);
    let deserialized = deserialized.unwrap_or_else(|| panic!("{text}: no error"));
    assert_eq!(Some(&deserialized), checked.as_ref(), "{text}");
    assert!(deserialized.contains(message), "{deserialized}");
}

#[test]
fn explicit_defaults_are_recorded_in_the_schema() {
    let schema = schema::build::<Model>();
    let setting = |path: &str| match schema.get(path) {
        Some(TypeDef::Struct {
            explicit_defaults, ..
        }) => *explicit_defaults,
        other => panic!("{other:?}"),
    };
    assert_eq!(setting("schema::Relaxed"), Some(false));
    assert_eq!(setting("schema::Strict"), Some(true));
    assert_eq!(setting("schema::Plain"), None);
    let Some(TypeDef::Enum { variants, .. }) = schema.get("schema::Task") else {
        panic!()
    };
    // The enum's setting is its struct variants', unless they have their own.
    assert_eq!(variants["Run"].explicit_defaults, Some(false));
    assert_eq!(variants["Stop"].explicit_defaults, Some(true));
    assert_eq!(variants["Wait"].explicit_defaults, None);

    let text = schema.to_text().unwrap();
    assert!(text.contains("explicit_defaults: Some(false)"), "{text}");
    assert_eq!(SchemaFile::from_text(&text).unwrap(), schema);
    // A schema without the attribute does not change.
    let text = schema::build::<Plain>().to_text().unwrap();
    assert!(!text.contains("explicit_defaults"), "{text}");
}

#[test]
fn a_type_can_take_its_defaults_without_the_marker() {
    let text = "+a: Relaxed { name: \"x\" },";
    agree_ok(text, true);
    assert_eq!(
        model_data(text, true, true).get::<Model>("a").unwrap(),
        Model::Relaxed(Relaxed {
            name: "x".into(),
            port: 0,
            note: None,
        })
    );
    // `..` is allowed, and changes nothing.
    agree_ok("+a: Relaxed { name: \"x\", .. },", true);
    agree_ok("+a: Relaxed { name: \"x\", port: 1, note: None },", true);
    // A field without a default is still an error, as with `..`.
    agree_error("+a: Relaxed { port: 1 },", true, "missing field `name`");
    agree_error("+a: Relaxed { port: 1, .. },", true, "missing field `name`");
    // Other types are as they were.
    agree_error(
        "+a: Plain { name: \"x\" },",
        true,
        "`Plain` is missing the field `port`; write it, or end the block with `..`",
    );
    agree_ok("+a: Plain { name: \"x\", .. },", true);
}

#[test]
fn the_setting_applies_to_struct_variants() {
    agree_ok("+a: Task(Run { }),", true);
    agree_error(
        "+a: Task(Stop { }),",
        true,
        "`Stop` is missing the field `force`",
    );
    agree_ok("+a: Task(Stop { .. }),", true);
    agree_error(
        "+a: Task(Stop { }),",
        false,
        "`Stop` is missing the field `force`",
    );
}

#[test]
fn the_setting_comes_before_require_complete() {
    agree_error(
        "+a: Strict { name: \"x\" },",
        false,
        "`Strict` is missing the field `port`",
    );
    agree_ok("+a: Strict { name: \"x\", .. },", false);
    agree_ok("+a: Relaxed { name: \"x\" },", false);
    // Without a setting, the option says.
    agree_ok("+a: Plain { name: \"x\" },", false);
    agree_error("+a: Plain { port: 1 },", false, "missing field `name`");
}

#[test]
fn the_deserializer_needs_the_schema_for_the_setting() {
    // Serde does not tell it, so without a schema the option says: the
    // strict side by default, which fails rather than load other values.
    let text = "+a: Relaxed { name: \"x\" },";
    let error = model_data(text, true, false).get::<Model>("a").unwrap_err();
    assert!(
        error
            .message()
            .contains("`Relaxed` is missing the fields `port`, `note`"),
        "{error}"
    );
    let text = "+a: Strict { name: \"x\" },";
    assert!(model_data(text, false, false).get::<Model>("a").is_ok());

    // Value mode: `from_str` has no schema, `from_str_with_schema` has one.
    let text = "Relaxed { name: \"x\" }";
    let error = crol::from_str::<Relaxed>(text).unwrap_err();
    assert!(error.message().contains("is missing the fields"), "{error}");
    let schema = schema::build::<Relaxed>();
    assert_eq!(
        crol::from_str_with_schema::<Relaxed>(text, &schema).unwrap(),
        Relaxed {
            name: "x".into(),
            port: 0,
            note: None,
        }
    );
    let error = crol::from_str_with_schema::<Relaxed>("Relaxed { port: 1 }", &schema).unwrap_err();
    assert!(error.message().contains("missing field `name`"), "{error}");
    // The value-mode check says the same.
    let value = crol_eval_value(text);
    let checked = crol::check_value(&value, &schema, None, &Default::default());
    assert!(checked.is_empty(), "{checked:?}");
}

fn crol_eval_value(text: &str) -> crol::Value {
    let mut sources = crol::Sources::new();
    let file = sources.add("<string>", text);
    crol_eval::evaluate_value(&sources, file).value.unwrap()
}

#[test]
fn fields_of_a_struct_with_serde_default_have_a_default() {
    let schema = schema::build::<Model>();
    let Some(TypeDef::Struct { fields, .. }) = schema.get("schema::AllDefault") else {
        panic!()
    };
    assert_eq!(fields["cpu"].default, FieldDefault::Unrecorded);
    assert_eq!(fields["label"].default, FieldDefault::Unrecorded);
    agree_ok("+a: AllDefault { },", true);
    agree_ok("+a: AllDefault { .. },", true);
    // The check used to take them for required, and differ from serde.
    let data = crol::CrolComposer::new()
        .load_layer(crol::CrolLayer::from_sources(
            "base",
            [("a.crol", "+a: Limits { .. },")],
        ))
        .compose_data()
        .unwrap();
    assert_eq!(
        data.get::<Limits>("a").unwrap(),
        Limits {
            cpu: 1.0,
            memory_mb: 512,
        }
    );
    let checked = crol::check(&data, &schema::build::<Limits>(), None, &Default::default());
    assert!(checked.is_empty(), "{checked:?}");
}

#[test]
fn defaults_left_out_without_the_marker_are_filled_in() {
    let data = model_data(
        "+a: Relaxed { name: \"x\" },\n+b: Plain { name: \"x\", port: 1 },",
        true,
        true,
    );
    let mut sources = crol::Sources::new();
    let filled = crol::Span {
        file: sources.add("<default>", ""),
        range: Default::default(),
    };
    let root = crol::fill_defaults(data.view().base, &schema::build::<Model>(), None, filled);
    let text = crol_eval::print::to_document_noted(&root, &Default::default(), &|v| {
        (v.span == filled).then(|| "default".to_owned())
    });
    assert_eq!(
        text,
        "a: Relaxed {\n    name: \"x\",\n    port: 0, // default\n    note: None, // default\n}\n\
         b: Plain { name: \"x\", port: 1 }\n"
    );
}

#[test]
fn the_setting_does_not_change_the_fingerprint() {
    let text = "+a: Relaxed { name: \"x\" },";
    assert_eq!(
        model_data(text, true, true).fingerprint(),
        model_data(text, true, false).fingerprint()
    );
}

#[test]
fn the_value_mode_check_follows_the_setting() {
    let check = |text: &str, schema: &SchemaFile| {
        crol::check_value(&crol_eval_value(text), schema, None, &Default::default())
    };
    let strict = schema::build::<Strict>();
    let found = check("Strict { name: \"x\" }", &strict);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].message.contains("missing the field `port`"));
    assert!(check("Strict { name: \"x\", .. }", &strict).is_empty());
    let relaxed = schema::build::<Relaxed>();
    let found = check("Relaxed { port: 1 }", &relaxed);
    assert_eq!(found[0].message, "missing field `name`");
}

/// The root of data, deserialized as a struct: its fields are root keys.
#[derive(Debug, PartialEq, Deserialize, Schema)]
#[crol(explicit_defaults = false)]
pub struct RelaxedRoot {
    pub name: String,
    #[serde(default)]
    pub port: u16,
}

#[derive(Debug, PartialEq, Deserialize, Schema)]
#[crol(explicit_defaults)]
pub struct StrictRoot {
    pub name: String,
    #[serde(default)]
    pub port: u16,
}

#[test]
fn the_root_of_data_cannot_write_the_marker() {
    let composer = |text: &str| {
        crol::CrolComposer::new()
            .load_layer(crol::CrolLayer::from_sources("base", [("a.crol", text)]))
    };
    // `..` is an error there, so a root struct that needs it must write
    // every field; `explicit_defaults = false` is how it can leave some out.
    let error = composer("+name: \"x\",\n..,")
        .compose::<StrictRoot>()
        .unwrap_err();
    assert!(
        error.to_string().contains("cannot be at the root"),
        "{error}"
    );
    let error = composer("+name: \"x\",")
        .schema(schema::build::<StrictRoot>())
        .compose::<StrictRoot>()
        .unwrap_err();
    assert!(
        error.message().contains("missing the field `port`"),
        "{error}"
    );
    let root = composer("+name: \"x\",\n+port: 1,")
        .schema(schema::build::<StrictRoot>())
        .compose::<StrictRoot>()
        .unwrap();
    assert_eq!(root.port, 1);
    let root = composer("+name: \"x\",")
        .schema(schema::build::<RelaxedRoot>())
        .compose::<RelaxedRoot>()
        .unwrap();
    assert_eq!(root.port, 0);
}

#[test]
fn the_marker_may_come_from_any_layer_or_spread() {
    let check = |layers: &[&str]| {
        let mut composer = crol::CrolComposer::new();
        for (i, text) in layers.iter().enumerate() {
            composer = composer.load_layer(crol::CrolLayer::from_sources(
                format!("l{i}"),
                [("a.crol", *text)],
            ));
        }
        let data = composer
            .schema(schema::build::<Model>())
            .compose_data()
            .unwrap();
        let found = crol::check(&data, &schema::build::<Model>(), None, &Default::default());
        assert_eq!(
            found.is_empty(),
            data.get::<Model>("a").is_ok(),
            "{found:?}"
        );
        found.is_empty()
    };
    let bare = "+a: Strict { name: \"x\" },";
    assert!(!check(&[bare]));
    // A later patch, or an earlier definition the patch keeps.
    assert!(check(&[bare, "a: & ~ { .. },"]));
    assert!(check(&[
        "+a: Strict { name: \"x\", .. },",
        "a: & ~ { name: \"y\" },"
    ]));
    // A spread brings it along.
    assert!(check(&["t := { .. },\n+a: Strict { ..&t, name: \"x\" },"]));
    // Replacing the whole value starts again.
    assert!(!check(&[
        "+a: Strict { name: \"x\", .. },",
        "a: Strict { name: \"y\" },"
    ]));
}

#[test]
fn written_values_pass_their_own_check() {
    #[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
    #[crol(explicit_defaults)]
    struct Skips {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    }
    #[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
    #[crol(explicit_defaults)]
    struct SkipsAlways {
        name: String,
        #[serde(default, skip_serializing)]
        note: Option<String>,
    }
    let check = |text: &str, schema: &SchemaFile| {
        crol::check_value(&crol_eval_value(text), schema, None, &Default::default())
    };

    // Every field written, or `..` for the ones left out.
    let value = Strict {
        name: "x".into(),
        port: 1,
    };
    let text = crol::to_string(&value).unwrap();
    assert!(
        check(&text, &schema::build::<Strict>()).is_empty(),
        "{text}"
    );
    let value = Skips {
        name: "x".into(),
        note: None,
    };
    let text = crol::to_string(&value).unwrap();
    assert!(text.contains(".."), "{text}");
    let schema = schema::build::<Skips>();
    assert!(check(&text, &schema).is_empty(), "{text}");
    assert_eq!(
        crol::from_str_with_schema::<Skips>(&text, &schema).unwrap(),
        value
    );
    let value = Relaxed {
        name: "x".into(),
        port: 1,
        note: None,
    };
    let text = crol::to_string(&value).unwrap();
    assert!(
        check(&text, &schema::build::<Relaxed>()).is_empty(),
        "{text}"
    );

    // serde does not tell the serializer about `skip_serializing`.
    let value = SkipsAlways {
        name: "x".into(),
        note: None,
    };
    let text = crol::to_string(&value).unwrap();
    assert!(!text.contains(".."), "{text}");
    assert_eq!(
        check(&text, &schema::build::<SkipsAlways>()).len(),
        1,
        "{text}"
    );
}
