//! Views, `distinct` and queries (`SPEC.md` 10.11).

#![allow(missing_docs)]

use crol::{CrolComposer, CrolConfig, CrolData, CrolLayer, CrolVariants, schema};
use crol_derive::{Schema, View};
use serde_derive::{Deserialize, Serialize};

const TEXT: &str = r#"
+artillery: Unit {
    hp: 1100,
    combat: { range: { max: 12 } },
    tags: ["heavy", "mech"],
    weapon: @cannon,
}
+trooper: Unit {
    hp: 50,
    combat: { range: { max: 5 } },
    tags: ["infantry"],
    weapon: @rifle,
}
+mech: Unit {
    hp: 300,
    combat: { range: { max: 6 } },
    tags: ["mech"],
    weapon: @cannon,
}
+tree: Scenery { hp: 10 }

artillery<empire>.hp: 1200
artillery<rebel>.hp: 1200
trooper<rebel>.tags: & + ["mech"]
"#;

fn data() -> CrolData {
    CrolComposer::with_cfg(CrolConfig {
        variants: CrolVariants::new().add_grouped(["empire", "rebel", "gungans"]),
        ..Default::default()
    })
    .load_layer(CrolLayer::from_sources("base", [("a.crod", TEXT)]))
    .compose_data()
    .unwrap()
}

#[derive(Debug, PartialEq, View)]
struct Stats {
    hp: u32,
    #[crol(path = "combat.range.max")]
    range: u32,
}

#[derive(Debug, PartialEq, View)]
struct Optional {
    armour: Option<u32>,
    #[crol(default)]
    speed: u32,
}

#[test]
fn a_key_no_context_changes_has_one_value() {
    let data = data();
    let distinct = data.distinct("mech");
    assert_eq!(distinct.len(), 1);
    assert_eq!(distinct[0].contexts.len(), 4);
}

#[test]
fn contexts_with_the_same_value_are_together() {
    let data = data();
    let distinct = data.distinct("artillery");
    assert_eq!(distinct.len(), 2);
    let names = |i: usize| -> Vec<Vec<&str>> {
        distinct[i]
            .contexts
            .iter()
            .map(|c| data.context_names(c))
            .collect()
    };
    assert_eq!(names(0), [vec![], vec!["gungans"]]);
    assert_eq!(names(1), [vec!["empire"], vec!["rebel"]]);
}

#[test]
fn a_view_reads_by_path_in_each_variation() {
    let data = data();
    let variations = data.variations::<Stats>("artillery").unwrap();
    assert_eq!(
        variations[0].value,
        Stats {
            hp: 1100,
            range: 12
        }
    );
    assert_eq!(
        variations[1].value,
        Stats {
            hp: 1200,
            range: 12
        }
    );
}

#[test]
fn a_missing_field_is_an_error_unless_the_view_allows_it() {
    let data = data();
    let error = data.variations::<Stats>("tree").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("`tree` has no `combat.range.max`"),
        "{error}"
    );
    let variations = data.variations::<Optional>("tree").unwrap();
    assert_eq!(
        variations[0].value,
        Optional {
            armour: None,
            speed: 0
        }
    );
}

fn three() -> u32 {
    3
}

#[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
#[crol(defaults)]
pub struct Unit {
    pub hp: u32,
    #[serde(default = "three")]
    pub speed: u32,
}

#[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
pub enum Definition {
    Unit(Unit),
}

#[test]
fn a_view_reads_a_default_from_the_schema() {
    #[derive(View)]
    struct Speed {
        speed: u32,
    }
    let data = CrolComposer::<String>::new()
        .schema(schema::build::<Definition>())
        .load_layer(CrolLayer::from_sources(
            "base",
            [("a.crod", "+x: Unit { hp: 1, .. }")],
        ))
        .compose_data()
        .unwrap();
    assert_eq!(data.variations::<Speed>("x").unwrap()[0].value.speed, 3);
}

#[test]
fn a_query_chooses_by_type_and_is_in_key_order() {
    assert_eq!(
        data().query().of_type("Unit").keys().unwrap(),
        ["artillery", "mech", "trooper"]
    );
}

#[test]
fn a_query_chooses_by_what_a_list_holds() {
    let data = data();
    assert_eq!(
        data.query().holding("tags", "mech").keys().unwrap(),
        ["artillery", "mech"]
    );
}

#[test]
fn a_query_reads_in_its_context() {
    let data = data();
    let rebel = data.context(["rebel"]).unwrap();
    let query = data.query().holding("tags", "mech").in_context(&rebel);
    assert_eq!(query.keys().unwrap(), ["artillery", "mech", "trooper"]);
    let views = query.views::<Stats>().unwrap();
    assert_eq!(
        views[0],
        (
            "artillery".to_owned(),
            Stats {
                hp: 1200,
                range: 12
            }
        )
    );
}

#[test]
fn a_query_compares_a_nested_value_and_an_id_as_its_text() {
    let data = data();
    assert_eq!(
        data.query().holding("combat.range.max", &5).keys().unwrap(),
        ["trooper"]
    );
    assert_eq!(
        data.query().holding("weapon", "cannon").keys().unwrap(),
        ["artillery", "mech"]
    );
}
