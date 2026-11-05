//! `init_defaults`: paths through fields left out (`SPEC.md` 5.1).

#![allow(missing_docs)]

use std::collections::BTreeMap;

use crol::{CrolComposer, CrolConfig, CrolLayer, schema};
use crol_derive::Schema;
use serde_derive::{Deserialize, Serialize};

#[derive(Debug, Default, PartialEq, Deserialize, Serialize, Schema)]
#[crol(defaults)]
#[serde(default)]
pub struct Traits {
    pub stealth: bool,
    pub level: u32,
}

#[derive(Debug, Default, PartialEq, Deserialize, Serialize, Schema)]
#[crol(defaults)]
#[serde(default)]
pub struct Behaviour {
    pub traits: Traits,
    pub optional: Option<Traits>,
    pub mode: u32,
}

#[derive(Debug, Default, PartialEq, Deserialize, Serialize, Schema)]
#[crol(defaults)]
#[serde(default)]
pub struct Unit {
    pub hp: u32,
    pub behaviour: Behaviour,
}

#[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
pub enum Definition {
    Unit(Unit),
}

fn load(init: bool, text: &str) -> Result<BTreeMap<String, Definition>, crol::Error> {
    CrolComposer::with_cfg(CrolConfig::<String> {
        init_defaults: init,
        ..Default::default()
    })
    .schema(schema::build::<Definition>())
    .load_layer(CrolLayer::from_sources("base", [("a.crod", text)]))
    .compose()
}

fn unit(text: &str) -> Unit {
    let mut units = load(true, text).unwrap();
    let Some(Definition::Unit(unit)) = units.remove("x") else {
        panic!("no unit");
    };
    unit
}

#[test]
fn a_path_is_an_error_without_the_option() {
    let error = load(
        false,
        "+x: Unit { hp: 1, .. }\nx.behaviour.traits.stealth: true",
    )
    .unwrap_err();
    assert!(error.to_string().contains("is not set"), "{error}");
}

#[test]
fn a_path_starts_from_the_defaults_of_what_is_left_out() {
    let unit = unit("+x: Unit { hp: 1, .. }\nx.behaviour.traits.stealth: true");
    assert!(unit.behaviour.traits.stealth);
    assert_eq!(unit.hp, 1);
}

#[test]
fn a_path_in_a_block_starts_there_too() {
    let unit = unit("+x: Unit { hp: 1, .. }\nx: & ~ { behaviour.traits.level: 2 }");
    assert_eq!(unit.behaviour.traits.level, 2);
}

#[test]
fn a_path_in_a_whole_value_block_starts_there_too() {
    let unit = unit("+x: Unit { hp: 1, behaviour.mode: 4, .. }");
    assert_eq!(unit.behaviour.mode, 4);
}

#[test]
fn a_nested_block_with_no_name_finds_its_type() {
    let unit = unit("+x: Unit { hp: 1, .. }\nx: & ~ { behaviour: { traits.level: 2 } }");
    assert_eq!(unit.behaviour.traits.level, 2);
}

#[test]
fn a_block_merges_with_the_default() {
    let unit = unit("+x: Unit { hp: 1, .. }\nx: & ~ { behaviour: { mode: 3 } }");
    assert_eq!(unit.behaviour.mode, 3);
    assert_eq!(unit.behaviour.traits, Traits::default());
}

#[test]
fn the_value_below_is_the_default() {
    let unit = unit("+x: Unit { hp: 1, .. }\nx.behaviour.traits.level: & + 2");
    assert_eq!(unit.behaviour.traits.level, 2);
}

#[test]
fn an_option_starts_from_the_default_of_what_it_holds() {
    let unit = unit("+x: Unit { hp: 1, .. }\nx.behaviour.optional.level: 2");
    assert_eq!(unit.behaviour.optional.map(|t| t.level), Some(2));
}

#[test]
fn a_none_counts_as_left_out() {
    let unit =
        unit("+x: Unit { hp: 1, .. }\nx.behaviour.mode: 1\nx.behaviour.optional.level: & + 2");
    assert_eq!(unit.behaviour.optional.map(|t| t.level), Some(2));
}

#[test]
fn a_condition_starts_from_the_default_in_its_context_only() {
    let text = "+x: Unit { hp: 1, .. }\nx<prod>.behaviour.mode: 2";
    let data = CrolComposer::with_cfg(CrolConfig::<String> {
        init_defaults: true,
        ..Default::default()
    })
    .schema(schema::build::<Definition>())
    .load_layer(CrolLayer::from_sources("base", [("a.crod", text)]))
    .compose_data()
    .unwrap();
    let base: Definition = data.get("x").unwrap();
    let prod = data.context(["prod"]).unwrap();
    let in_prod: Definition = data.get_in("x", &prod).unwrap();
    let (Definition::Unit(base), Definition::Unit(in_prod)) = (base, in_prod);
    assert_eq!(base.behaviour.mode, 0);
    assert_eq!(in_prod.behaviour.mode, 2);
}

#[test]
fn a_field_with_no_default_stays_an_error() {
    let error = load(
        true,
        "+x: Unit { hp: 1, .. }\nx.behaviour.nothing.at_all: 1",
    )
    .unwrap_err();
    assert!(error.to_string().contains("is not set"), "{error}");
}
