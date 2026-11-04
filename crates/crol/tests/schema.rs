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
