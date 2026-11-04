//! `DataWriter`: the documents it writes read back (`SPEC.md` 10.8).

#![allow(missing_docs)]

use std::collections::BTreeMap;

use crol::{CrolComposer, CrolLayer, DataWriter, PrettyConfig};
use serde_derive::{Deserialize, Serialize};

fn is_zero(n: &u32) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
struct Combat {
    range: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    reload: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
enum Slot {
    Active(String),
    Conditional { weapon: String, delay: u32 },
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
struct Unit {
    hp: u32,
    combat: Combat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    armour: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    costs: BTreeMap<String, u32>,
    slot: Slot,
}

fn unit() -> Unit {
    Unit {
        hp: 100,
        combat: Combat {
            range: 5,
            reload: 2,
        },
        armour: Some(1),
        tags: vec!["a".to_owned()],
        costs: BTreeMap::from([("ore".to_owned(), 10), ("nova".to_owned(), 5)]),
        slot: Slot::Conditional {
            weapon: "gun".to_owned(),
            delay: 1,
        },
    }
}

fn read(layers: &[String]) -> BTreeMap<String, Unit> {
    let mut composer = CrolComposer::new();
    for (i, text) in layers.iter().enumerate() {
        composer = composer.load_layer(CrolLayer::from_sources(
            format!("layer{i}"),
            [("a.crod", text.as_str())],
        ));
    }
    composer.compose().unwrap()
}

/// `to` written as a diff from `from` over `from`.
fn through_diff(from: &Unit, to: &Unit) -> (String, Unit) {
    let mut base = DataWriter::new();
    base.declare("x", from).unwrap();
    let mut patch = DataWriter::new();
    patch.diff("x", from, to).unwrap();
    let text = patch.finish().unwrap();
    let units = read(&[base.finish().unwrap(), text.clone()]);
    (text, units["x"].clone())
}

#[test]
fn a_declared_value_reads_back() {
    let mut writer = DataWriter::new();
    writer.declare("x", &unit()).unwrap();
    assert_eq!(read(&[writer.finish().unwrap()])["x"], unit());
}

#[test]
fn fields_left_out_get_their_defaults_marked() {
    let mut value = unit();
    value.armour = None;
    value.tags.clear();
    value.combat.reload = 0;
    let mut writer = DataWriter::new();
    writer.declare("x", &value).unwrap();
    let text = writer.finish().unwrap();
    assert!(text.contains(".."), "{text}");
    assert_eq!(read(&[text])["x"], value);
}

#[test]
fn a_patch_changes_only_what_it_holds() {
    #[derive(Serialize)]
    struct Patch {
        hp: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        armour: Option<u32>,
    }
    let mut base = DataWriter::new();
    base.declare("x", &unit()).unwrap();
    let names = PrettyConfig::default().with_struct_names(false);
    let mut patch = DataWriter::with_config(names);
    let value = Patch {
        hp: 7,
        armour: None,
    };
    patch.patch("x", &value).unwrap();
    let text = patch.finish().unwrap();
    assert_eq!(text, "x: & ~ { hp: 7 }\n");
    let units = read(&[base.finish().unwrap(), text]);
    assert_eq!(units["x"].hp, 7);
    assert_eq!(units["x"].armour, Some(1));
}

#[test]
fn a_diff_writes_only_what_differs() {
    let from = unit();
    let mut to = unit();
    to.hp = 150;
    to.combat.range = 6;
    let (text, read) = through_diff(&from, &to);
    assert_eq!(read, to);
    assert!(!text.contains("reload"), "{text}");
    assert!(!text.contains("tags"), "{text}");
}

#[test]
fn a_diff_of_equal_values_writes_nothing() {
    let mut writer = DataWriter::new();
    writer.diff("x", &unit(), &unit()).unwrap();
    assert!(writer.is_empty());
}

#[test]
fn a_diff_removes_what_is_gone() {
    let from = unit();
    let mut to = unit();
    to.armour = None;
    to.costs.remove("ore");
    to.combat.reload = 0;
    let (text, read) = through_diff(&from, &to);
    assert_eq!(read, to);
    assert!(text.contains("-armour"), "{text}");
    assert!(text.contains("-ore"), "{text}");
}

#[test]
fn a_diff_adds_what_is_new() {
    let mut from = unit();
    from.armour = None;
    from.costs.clear();
    let (_, read) = through_diff(&from, &unit());
    assert_eq!(read, unit());
}

#[test]
fn a_diff_replaces_a_list_and_another_variant() {
    let from = unit();
    let mut to = unit();
    to.tags = vec!["b".to_owned(), "c".to_owned()];
    to.slot = Slot::Active("laser".to_owned());
    let (_, read) = through_diff(&from, &to);
    assert_eq!(read, to);
}

#[test]
fn a_variant_holds_its_entries_and_an_empty_one_is_left_out() {
    let mut writer = DataWriter::new();
    writer.declare("port", &80).unwrap();
    writer.variant("prod", |w| w.set("port", &443)).unwrap();
    writer.variant("dev", |_| Ok(())).unwrap();
    let text = writer.finish().unwrap();
    assert_eq!(text, "+port: 80\n<prod> {\n    port: 443,\n}\n");
}

#[test]
fn templates_removes_comments_and_odd_keys() {
    let mut writer = DataWriter::new();
    writer.comment("generated\nby a test");
    writer
        .template("base", &BTreeMap::from([("port", 80)]))
        .unwrap();
    writer.declare("odd key", &1).unwrap();
    writer.ensure("x", &2).unwrap();
    writer.remove("x");
    let text = writer.finish().unwrap();
    assert_eq!(
        text,
        "// generated\n// by a test\nbase := { port: 80 }\n+\"odd key\": 1\n?x: 2\n-x\n"
    );
}

#[test]
fn a_bad_condition_is_an_error() {
    let mut writer = DataWriter::new();
    writer.variant("a &", |w| w.set("x", &1)).unwrap();
    assert!(writer.finish().is_err());
}
