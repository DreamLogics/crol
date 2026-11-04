//! Constraints, `schema.crod` and sealed fields (`SPEC.md` 10.12, 10.13).

#![allow(missing_docs)]

use std::collections::BTreeMap;

use crol::schema::{self, Constraint, SchemaFile};
use crol::{CrolComposer, CrolConfig, CrolLayer, CrolVariants};
use crol_derive::Schema;
use serde_derive::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
#[crol(constraint = "percentage")]
pub struct Percent(pub i32);

#[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
#[crol(constraints(positive = Min(0), percentage = Range(0, 100), resource = Refers(Resource)))]
#[crol(open_constraints(channel = OneOf(["voice", "music"])))]
pub struct Unit {
    #[crol(constraint = "positive")]
    pub hp: i32,
    #[serde(default)]
    #[crol(constraint = "channel")]
    pub channels: Vec<String>,
    #[serde(default)]
    #[crol(key_constraint = "resource")]
    pub costs: BTreeMap<String, i32>,
    #[serde(default)]
    pub armour: Option<Percent>,
    #[serde(default)]
    #[crol(sealed)]
    pub original: u32,
    #[serde(default)]
    pub details: Details,
}

#[derive(Debug, Default, PartialEq, Deserialize, Serialize, Schema)]
pub struct Details {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    #[crol(sealed)]
    pub slot: u32,
}

#[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
pub struct Resource {
    pub start: i32,
}

#[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
#[crol(sealed)]
pub struct Fixed {
    pub a: i32,
}

#[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
pub enum Definition {
    Unit(Unit),
    Resource(Resource),
    Fixed(Fixed),
}

fn composer(layers: &[(&str, &[(&str, &str)])]) -> CrolComposer {
    let mut composer = CrolComposer::with_cfg(CrolConfig {
        variants: CrolVariants::new().add_grouped(["empire", "rebel"]),
        ..Default::default()
    })
    .schema(schema::build::<Definition>());
    for (name, documents) in layers {
        composer = composer.load_layer(CrolLayer::from_sources(*name, documents.iter().copied()));
    }
    composer
}

fn error(layers: &[(&str, &[(&str, &str)])]) -> String {
    composer(layers).compose_data().unwrap_err().to_string()
}

fn loads(layers: &[(&str, &[(&str, &str)])]) {
    composer(layers).compose_data().unwrap();
}

const ORE: &str = "+ore: Resource { start: 100 }\n";

#[test]
fn values_within_their_constraints_load() {
    let text = format!(
        "{ORE}+x: Unit {{ hp: 5, channels: [\"voice\"], costs: {{ ore: 3 }}, armour: 50, .. }}"
    );
    loads(&[("base", &[("a.crod", &text)])]);
}

#[test]
fn a_minimum_is_reported_where_the_value_is_written() {
    let message = error(&[("base", &[("a.crod", "+x: Unit { hp: -1, .. }")])]);
    assert!(
        message.contains("-1 is below 0, the minimum of `positive`"),
        "{message}"
    );
    assert!(message.contains("a.crod:1:"), "{message}");
}

#[test]
fn a_constraint_on_a_list_is_on_what_it_holds() {
    let message = error(&[(
        "base",
        &[(
            "a.crod",
            "+x: Unit { hp: 1, channels: [\"voice\", \"x\"], .. }",
        )],
    )]);
    assert!(
        message.contains("\"x\" is not one of \"voice\", \"music\" (`channel`)"),
        "{message}"
    );
}

#[test]
fn a_constraint_on_a_type_holds_wherever_it_is_used() {
    let message = error(&[(
        "base",
        &[("a.crod", "+x: Unit { hp: 1, armour: Percent(150), .. }")],
    )]);
    assert!(message.contains("150 is above 100"), "{message}");
}

#[test]
fn a_key_constraint_refers_to_root_values_of_a_type() {
    let text = format!("{ORE}+x: Unit {{ hp: 1, costs: {{ ore: 1, wood: 2 }}, .. }}");
    let message = error(&[("base", &[("a.crod", &text)])]);
    assert!(message.contains("`wood` names nothing"), "{message}");
    let text = format!("{ORE}+x: Unit {{ hp: 1, costs: {{ x: 2 }}, .. }}");
    let message = error(&[("base", &[("a.crod", &text)])]);
    assert!(message.contains("`x` is not a Resource"), "{message}");
}

#[test]
fn a_constraint_holds_in_every_context() {
    let message = error(&[(
        "base",
        &[("a.crod", "+x: Unit { hp: 1, .. }\nx<rebel>.hp: -2")],
    )]);
    assert!(message.contains("in {rebel}: -2 is below"), "{message}");
}

#[test]
fn a_constraint_reads_the_final_value() {
    let layers: &[(&str, &[(&str, &str)])] = &[
        ("base", &[("a.crod", "+x: Unit { hp: -1, .. }")]),
        ("mod", &[("a.crod", "x.hp: 1")]),
    ];
    loads(layers);
}

#[test]
fn a_schema_document_extends_an_open_constraint() {
    let text = "+x: Unit { hp: 1, channels: [\"ambience\"], .. }";
    let patch = "constraints.channel: OneOf(& + [\"ambience\"])";
    loads(&[
        ("base", &[("a.crod", text)]),
        ("mod", &[("schema.crod", patch)]),
    ]);
}

#[test]
fn a_schema_document_extends_a_constraint_inside_a_block() {
    let text = "+x: Unit { hp: 1, channels: [\"ambience\", \"voice\"], .. }";
    let patch = "constraints: { channel: OneOf(& + [\"ambience\"]) }";
    loads(&[
        ("base", &[("a.crod", text)]),
        ("mod", &[("schema.crod", patch)]),
    ]);
}

#[test]
fn a_block_in_a_schema_document_merges_with_the_layers_below() {
    let schema = schema::build::<Definition>();
    let mut sources = crol::Sources::new();
    let base = sources.add("base/schema.crod", "constraints: { first: Min(1) }");
    let top = sources.add("top/schema.crod", "constraints: { second: Max(2) }");
    let layers = [
        crol_eval::Layer {
            name: "base".to_owned(),
            documents: vec![("schema.crod".to_owned(), base)],
        },
        crol_eval::Layer {
            name: "top".to_owned(),
            documents: vec![("schema.crod".to_owned(), top)],
        },
    ];
    let (patched, found) = schema.patched(&mut sources, &layers);
    assert!(found.is_empty(), "{found:?}");
    assert_eq!(
        patched.constraints["first"].constraint,
        Constraint::Min(1.0)
    );
    assert_eq!(
        patched.constraints["second"].constraint,
        Constraint::Max(2.0)
    );
    assert!(patched.constraints["positive"].sealed);
    assert!(!patched.constraints["channel"].sealed);
}

#[test]
fn a_schema_document_is_not_data() {
    let data = composer(&[(
        "base",
        &[
            ("schema.crod", "constraints: { a: Min(1) }"),
            ("a.crod", ORE),
        ],
    )])
    .compose_data()
    .unwrap();
    assert_eq!(data.keys(), ["ore"]);
}

#[test]
fn only_a_layers_root_holds_a_schema_document() {
    let data = composer(&[("base", &[("deeper/schema.crod", ORE)])])
        .compose_data()
        .unwrap();
    assert_eq!(data.keys(), ["ore"]);
}

#[test]
fn a_sealed_constraint_cannot_be_changed() {
    let message = error(&[("base", &[("schema.crod", "constraints.positive: Min(-5)")])]);
    assert!(message.contains("`positive` is sealed"), "{message}");
}

#[test]
fn a_schema_document_holds_only_constraints_without_variants() {
    let message = error(&[("base", &[("schema.crod", "+other: 1")])]);
    assert!(message.contains("holds only `constraints`"), "{message}");
    let message = error(&[(
        "base",
        &[("schema.crod", "constraints<rebel>.channel: Min(1)")],
    )]);
    assert!(message.contains("cannot use variants"), "{message}");
}

#[test]
fn a_bad_constraint_is_an_error() {
    let message = error(&[(
        "base",
        &[("schema.crod", "constraints: { a: Nonsense(1) }")],
    )]);
    assert!(message.contains("`a`:"), "{message}");
}

#[derive(Debug, Deserialize, Serialize, Schema)]
pub struct Undefined {
    #[crol(constraint = "nowhere")]
    pub a: i32,
}

#[test]
fn a_constraint_nothing_defines_is_an_error() {
    let error = CrolComposer::<String>::new()
        .schema(schema::build::<Undefined>())
        .load_layer(CrolLayer::from_sources("base", [("a.crod", "a: 1")]))
        .compose_data()
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("nothing defines the constraint `nowhere`"),
        "{error}"
    );
}

#[test]
fn a_schema_document_needs_a_schema() {
    let error = CrolComposer::<String>::new()
        .load_layer(CrolLayer::from_sources(
            "base",
            [("schema.crod", "constraints: {}")],
        ))
        .compose_data()
        .unwrap_err();
    assert!(error.to_string().contains("needs a schema"), "{error}");
}

#[test]
fn the_schema_file_round_trips_with_constraints() {
    let schema = schema::build::<Definition>();
    assert_eq!(
        SchemaFile::from_text(&schema.to_text().unwrap()).unwrap(),
        schema
    );
}

const SEALED: &str = "+x: Unit { hp: 1, original: 7, details: { name: \"a\", slot: 2 }, .. }\n";

#[test]
fn the_layer_that_defines_a_value_sets_sealed_fields() {
    loads(&[(
        "base",
        &[(
            "a.crod",
            &format!("{SEALED}x.original: 8\nx<rebel>.original: 9"),
        )],
    )]);
}

#[test]
fn a_higher_layer_can_set_what_is_not_sealed() {
    loads(&[
        ("base", &[("a.crod", SEALED)]),
        ("mod", &[("a.crod", "x.hp: 5\nx.details.name: \"b\"")]),
    ]);
}

#[test]
fn a_path_cannot_change_a_sealed_field() {
    let message = error(&[
        ("base", &[("a.crod", SEALED)]),
        ("mod", &[("b.crod", "x.original: 8")]),
    ]);
    assert!(
        message.contains(
            "`x.original` is sealed: only the layer that defines `x` (`base`) can set it"
        ),
        "{message}"
    );
    assert!(message.contains("b.crod:1:"), "{message}");
}

#[test]
fn a_nested_sealed_field_is_found() {
    let message = error(&[
        ("base", &[("a.crod", SEALED)]),
        ("mod", &[("a.crod", "x: & ~ { details: { slot: 3 } }")]),
    ]);
    assert!(message.contains("`x.details.slot` is sealed"), "{message}");
}

#[test]
fn replacing_a_definition_cannot_change_a_sealed_field() {
    let message = error(&[
        ("base", &[("a.crod", SEALED)]),
        ("mod", &[("a.crod", "x: Unit { hp: 1, original: 1, .. }")]),
    ]);
    assert!(message.contains("`x.original` is sealed"), "{message}");
}

#[test]
fn removing_a_sealed_field_is_changing_it() {
    let message = error(&[
        ("base", &[("a.crod", SEALED)]),
        ("mod", &[("a.crod", "-x.original")]),
    ]);
    assert!(message.contains("`x.original` is sealed"), "{message}");
}

#[test]
fn a_sealed_field_cannot_change_through_a_late_reference() {
    let base =
        "+source: Resource { start: 7 }\n+x: Unit { hp: 1, original: &root.source.start, .. }\n";
    let message = error(&[
        ("base", &[("a.crod", base)]),
        ("mod", &[("a.crod", "source.start: 8")]),
    ]);
    assert!(message.contains("`x.original` is sealed"), "{message}");
}

#[test]
fn a_layer_that_does_not_evaluate_alone_has_nothing_to_compare_with() {
    let base = "+x: Unit { hp: &root.y.hp, .. }\n";
    loads(&[
        ("base", &[("a.crod", base)]),
        ("mod", &[("a.crod", "+y: Unit { hp: 1, .. }")]),
    ]);
}

#[test]
fn a_sealed_field_cannot_change_through_a_variant() {
    let base = "+x: Unit { hp: 1, original: 7, .. }\n";
    let message = error(&[
        ("base", &[("a.crod", base)]),
        ("mod", &[("a.crod", "x<rebel>.original: 8")]),
    ]);
    assert!(message.contains("`x.original` is sealed"), "{message}");
}

#[test]
fn setting_a_sealed_field_to_its_value_is_harmless() {
    loads(&[
        ("base", &[("a.crod", SEALED)]),
        ("mod", &[("a.crod", "x.original: 7")]),
    ]);
}

#[test]
fn a_mod_sets_sealed_fields_on_what_it_defines() {
    loads(&[
        ("base", &[("a.crod", SEALED)]),
        ("mod", &[("a.crod", "+y: Unit { hp: 1, original: 3, .. }")]),
    ]);
}

#[test]
fn sealed_on_a_type_seals_its_fields() {
    let message = error(&[
        ("base", &[("a.crod", "+f: Fixed { a: 1 }")]),
        ("mod", &[("a.crod", "f.a: 2")]),
    ]);
    assert!(message.contains("`f.a` is sealed"), "{message}");
}
