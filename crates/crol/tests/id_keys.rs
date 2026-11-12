//! Ids as map keys (`SPEC.md` 3.1, 3.2, 10.5): `@name: value`.

#![allow(missing_docs, dead_code)]

use std::collections::BTreeMap;

use crol::{CrolComposer, CrolData, CrolLayer, DataPath, Id, IdUse};
use crol_derive::Schema;
use serde_derive::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
struct Tag {}

#[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
struct Damage {
    amount: f64,
    #[crol(refers(Tag))]
    bonus: BTreeMap<Id, f64>,
}

#[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
struct Weapon {
    damage: Damage,
}

/// A unit is a scope: its own tags shadow the root's.
#[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
#[crol(scope)]
struct Unit {
    #[serde(default)]
    weapon: Option<Weapon>,
    #[serde(flatten)]
    tags: BTreeMap<String, Tag>,
}

#[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
enum Entry {
    Tag(Tag),
    Weapon(Weapon),
    Unit(Unit),
}

fn schema() -> crol::schema::SchemaFile {
    crol::schema::build::<BTreeMap<String, Entry>>()
}

fn load(layers: &[&str], with_schema: bool) -> Result<CrolData, crol::Error> {
    let mut composer = CrolComposer::new();
    if with_schema {
        composer = composer.schema(schema());
    }
    for (i, text) in layers.iter().enumerate() {
        let layer = CrolLayer::from_sources(format!("layer{i}"), [("a.crod", *text)]);
        composer = composer.load_layer(layer);
    }
    composer.compose_data()
}

fn targets(ids: &[IdUse]) -> Vec<String> {
    ids.iter()
        .map(|id| {
            let target = id
                .target
                .as_ref()
                .map_or("nothing".to_owned(), DataPath::to_string);
            format!("{} at {} -> {target}", id.id, id.rust_path)
        })
        .collect()
}

fn err<T: for<'de> serde::Deserialize<'de> + std::fmt::Debug>(text: &str) -> String {
    crol::from_str::<T>(text).unwrap_err().to_string()
}

const BASE: &str = r#"
+is_biological: Tag {}
+armored: Tag {}
+toxin_grenade: Weapon {
    damage: { amount: 23.0, bonus: { @is_biological: 47.0 } },
}
"#;

#[test]
fn a_map_keyed_by_ids_takes_id_keys() {
    let data = load(&[BASE], true).unwrap();
    let (weapon, ids): (Weapon, _) = data.get_with_ids("toxin_grenade").unwrap();
    assert_eq!(weapon.damage.bonus[&Id::key("is_biological")], 47.0);
    // The key is reported as an id value is, with what it names.
    assert_eq!(
        targets(&ids),
        ["@is_biological at toxin_grenade.damage.bonus[@is_biological] -> is_biological"]
    );
    assert!(crol::check(&data, &schema(), None, &Default::default()).is_empty());
}

#[test]
fn a_map_keyed_by_ids_takes_no_string_keys() {
    let text = BASE.replace("@is_biological: 47.0", "is_biological: 47.0");
    let data = load(&[&text], true).unwrap();
    assert_eq!(
        data.get::<Weapon>("toxin_grenade").unwrap_err().to_string(),
        "expected an id key, found the string key is_biological; write `@is_biological` \
         (layer0/a.crod:5:53, while deserializing toxin_grenade.damage.bonus.is_biological)"
    );
    let messages: Vec<String> = crol::check(&data, &schema(), None, &Default::default())
        .into_iter()
        .map(|d| d.message)
        .collect();
    assert_eq!(
        messages,
        ["expected an id key, found the string key is_biological; write `@is_biological`"]
    );
    assert_eq!(
        err::<BTreeMap<String, BTreeMap<Id, u8>>>("{ a: { 3: 1 } }"),
        "expected an id key, found the integer key 3 (<string>:1:11, while deserializing a[3])"
    );
}

#[test]
fn other_maps_take_no_id_keys() {
    #[derive(Debug, Deserialize, Schema)]
    struct Doc {
        web: u8,
        names: BTreeMap<String, u8>,
    }
    assert_eq!(
        err::<Doc>("{ web: 0, names: { @web: 1 } }"),
        "expected a string key, found the id key `@web`; only a map keyed by `crol::Id` takes \
         id keys (<string>:1:26, while deserializing names[@web])"
    );
    #[derive(Debug, Deserialize)]
    struct Numbers {
        web: u8,
        numbers: BTreeMap<u8, u8>,
    }
    assert_eq!(
        err::<Numbers>("{ web: 0, numbers: { @web: 1 } }"),
        "expected an integer key, found the id key `@web` (<string>:1:28, while deserializing \
         numbers[@web])"
    );
    #[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
    enum Color {
        Red,
    }
    #[derive(Debug, Deserialize)]
    struct Colors {
        web: u8,
        colors: BTreeMap<Color, u8>,
    }
    assert_eq!(
        err::<Colors>("{ web: 0, colors: { @web: 1 } }"),
        "expected a `Color` variant as the key, found the id key `@web`; only a map keyed by \
         `crol::Id` takes id keys (<string>:1:27, while deserializing colors[@web])"
    );
    // A struct's fields are no ids either.
    #[derive(Debug, Deserialize)]
    struct Outer {
        web: u8,
        inner: Inner,
    }
    #[derive(Debug, Deserialize)]
    struct Inner {
        web: u8,
    }
    assert_eq!(
        err::<Outer>("{ web: 0, inner: { @web: 1 } }"),
        "`Inner` has named fields; the id key `@web` is not one: only a map keyed by `crol::Id` \
         takes id keys (<string>:1:26, while deserializing inner[@web])"
    );
    // And the schema check says the same of a map.
    let data = CrolComposer::new()
        .load_layer(CrolLayer::from_sources(
            "base",
            [("a.crod", "web: 0\nnames: { @web: 1 }")],
        ))
        .compose_data()
        .unwrap();
    let schema = crol::schema::build::<Doc>();
    let messages: Vec<String> = crol::check(&data, &schema, None, &Default::default())
        .into_iter()
        .map(|d| d.message)
        .collect();
    assert_eq!(
        messages,
        [
            "the id key `@web` is not a key of this map: only a map keyed by `crol::Id` takes id keys"
        ]
    );
}

#[test]
fn a_later_layer_sets_the_entry_whatever_id_it_writes() {
    let data = load(
        &[
            BASE,
            "toxin_grenade.damage.bonus.@::is_biological: 50.0\n\
             toxin_grenade.damage.bonus[@armored]: 5.0",
        ],
        true,
    )
    .unwrap();
    let weapon: Weapon = data.get("toxin_grenade").unwrap();
    // One entry, under the id first written for it.
    assert_eq!(
        weapon.damage.bonus,
        BTreeMap::from([(Id::key("armored"), 5.0), (Id::key("is_biological"), 50.0)])
    );
}

const SCOPED: &str = r#"
+armored: Tag {}
+marine: Unit {
    armored: Tag {},
    weapon: Weapon {
        damage: { amount: 6.0, bonus: { @armored: 1.0, @::armored: 2.0 } },
    },
}
"#;

#[test]
fn keys_are_compared_by_what_they_name() {
    // With the schema `marine` is a scope, so that `@armored` is its own
    // tag and `@::armored` the root's: two keys.
    let data = load(
        &[SCOPED, "marine.weapon.damage.bonus.@marine.armored: 3.0"],
        true,
    )
    .unwrap();
    let (unit, ids): (Unit, _) = data.get_with_ids("marine").unwrap();
    let bonus = &unit.weapon.as_ref().unwrap().damage.bonus;
    assert_eq!(
        *bonus,
        BTreeMap::from([
            (Id::key("armored"), 3.0),
            ("@::armored".parse().unwrap(), 2.0)
        ])
    );
    assert_eq!(
        targets(&ids),
        [
            "@armored at marine.weapon.damage.bonus[@armored] -> marine.armored",
            "@::armored at marine.weapon.damage.bonus[@::armored] -> armored",
        ]
    );
    // Without it there is no scope: both name the root's tag, and are one
    // key, which the second sets. The loader needs the schema for scopes of
    // `#[crol(scope)]` types (`SPEC.md` 10.5).
    let data = load(&[SCOPED], false).unwrap();
    let unit: Unit = data.get("marine").unwrap();
    assert_eq!(
        unit.weapon.unwrap().damage.bonus,
        BTreeMap::from([(Id::key("armored"), 2.0)])
    );
}

#[test]
fn an_id_key_has_to_name_an_entry() {
    let text = BASE.replace("@is_biological: 47.0", "@is_mechanical: 47.0");
    assert_eq!(
        load(&[&text], true).unwrap_err().to_string(),
        "layer0/a.crod:5:38: the id key `@is_mechanical` names no root key; an id key has to \
         name an entry, and a key that names nothing is written as a string (while evaluating \
         toxin_grenade.damage.bonus)"
    );
    // Also in a value document, where an id value that names nothing is
    // only reported.
    #[derive(Debug, Deserialize)]
    struct Doc {
        tag: Tag,
        bonus: BTreeMap<Id, f64>,
        uses: Vec<Id>,
    }
    let (_, ids) =
        crol::from_str_with_ids::<Doc>("{ tag: {}, bonus: { @tag: 1.0 }, uses: [@nothing] }")
            .unwrap();
    assert_eq!(
        targets(&ids),
        [
            "@tag at bonus[@tag] -> tag",
            "@nothing at uses[0] -> nothing"
        ]
    );
    assert_eq!(
        err::<Doc>("{ tag: {}, bonus: { @nothing: 1.0 }, uses: [] }"),
        "<string>:1:21: the id key `@nothing` names no root key; an id key has to name an \
         entry, and a key that names nothing is written as a string (while evaluating bonus)"
    );
}

#[test]
fn value_documents_have_scopes_from_the_data() {
    #[derive(Debug, Deserialize)]
    struct Group {
        tag: Tag,
        bonus: BTreeMap<Id, u8>,
    }
    #[derive(Debug, Deserialize)]
    struct Doc {
        tag: Tag,
        plain: Group,
        scoped: Group,
    }
    let text = r#"{
        tag: {},
        plain: { tag: {}, bonus: { @tag: 1, @::tag: 2 } },
        #[scope]
        scoped: { tag: {}, bonus: { @tag: 1, @::tag: 2 } },
    }"#;
    let (doc, ids) = crol::from_str_with_ids::<Doc>(text).unwrap();
    assert_eq!(doc.plain.bonus, BTreeMap::from([(Id::key("tag"), 2)]));
    assert_eq!(doc.scoped.bonus.len(), 2);
    assert_eq!(
        targets(&ids),
        [
            "@tag at plain.bonus[@tag] -> tag",
            "@tag at scoped.bonus[@tag] -> scoped.tag",
            "@::tag at scoped.bonus[@::tag] -> tag",
        ]
    );
}

#[test]
fn untyped_deserialization_sees_the_text() {
    // Through `any`, an id key is its text with the `@`, as an id value is
    // (`SPEC.md` 10.5).
    #[derive(Debug, Deserialize)]
    struct Doc {
        web: u8,
        #[serde(flatten)]
        rest: BTreeMap<String, BTreeMap<String, u8>>,
    }
    let doc: Doc = crol::from_str("{ web: 0, more: { @web: 1 } }").unwrap();
    assert_eq!(doc.rest["more"], BTreeMap::from([("@web".to_owned(), 1)]));

    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(untagged)]
    enum Weights {
        ByText(BTreeMap<String, u8>),
    }
    #[derive(Debug, Deserialize)]
    struct Untagged {
        web: u8,
        weights: Weights,
    }
    let doc: Untagged = crol::from_str("{ web: 0, weights: { @web: 1 } }").unwrap();
    assert_eq!(
        doc.weights,
        Weights::ByText(BTreeMap::from([("@web".to_owned(), 1)]))
    );
    // An `Id` there still takes the text, since it starts with `@`.
    #[derive(Debug, Deserialize)]
    struct Flat {
        web: u8,
        #[serde(flatten)]
        ids: BTreeMap<Id, u8>,
    }
    let (doc, ids) = crol::from_str_with_ids::<Flat>("{ web: 0, @web: 1 }").unwrap();
    assert_eq!(doc.ids, BTreeMap::from([(Id::key("web"), 1)]));
    // The root's own id keys name its other keys.
    assert_eq!(targets(&ids), ["@web at [@web] -> web"]);
    assert_eq!(
        err::<Flat>("{ web: 0, @nothing: 1 }"),
        "<string>:1:11: the id key `@nothing` names no root key; an id key has to name an \
         entry, and a key that names nothing is written as a string"
    );
}

#[test]
fn kind_checks_apply_to_keys() {
    let text = format!("{BASE}\n+soldier: Unit {{ .. }}\ntoxin_grenade.damage.bonus.@soldier: 1.0");
    let data = load(&[&text], true).unwrap();
    let messages: Vec<String> = crol::check(&data, &schema(), None, &Default::default())
        .into_iter()
        .map(|d| d.message)
        .collect();
    assert_eq!(
        messages,
        ["the id `@soldier` names a `Unit`, but it may only name `Tag`"]
    );
}

#[test]
fn id_keys_are_written_as_ids() {
    let damage = Damage {
        amount: 1.0,
        bonus: BTreeMap::from([
            (Id::key("armored"), 1.0),
            ("@::armored".parse().unwrap(), 2.0),
            ("@marine.armored".parse().unwrap(), 3.0),
            (Id::key("odd id"), 4.0),
        ]),
    };
    let text = crol::to_string(&damage).unwrap();
    assert_eq!(
        text,
        "Damage {amount: 1.0, bonus: {@armored: 1.0, @::armored: 2.0, @marine.armored: 3.0, \
         @\"odd id\": 4.0}}"
    );
    // It reads back where the keys name entries, as different entries.
    let document = format!(
        "{{ armored: {{}}, \"odd id\": {{}}, #[scope] marine: {{ armored: {{}}, x: {{ armored: \
         {{}}, damage: {text} }} }} }}"
    );
    #[derive(Debug, Deserialize)]
    struct Inner {
        armored: Tag,
        damage: Damage,
    }
    #[derive(Debug, Deserialize)]
    struct Marine {
        armored: Tag,
        x: Inner,
    }
    #[derive(Debug, Deserialize)]
    struct Doc {
        armored: Tag,
        #[serde(rename = "odd id")]
        odd: Tag,
        marine: Marine,
    }
    let doc: Doc = crol::from_str(&document).unwrap();
    // `@armored` and `@marine.armored` name the same entry there.
    assert_eq!(doc.marine.x.damage.bonus.len(), 3);
}

#[test]
fn reload_resolves_keys_again() {
    let mut data = load(&[SCOPED], true).unwrap();
    let before: Unit = data.get("marine").unwrap();
    assert_eq!(before.weapon.unwrap().damage.bonus.len(), 2);
    // Without its own `armored`, both keys of `marine` name the root's.
    let changed = SCOPED.replace("    armored: Tag {},\n", "");
    data.reload(&[crol::DocumentChange {
        layer: "layer0",
        path: "a.crod",
        text: Some(&changed),
    }])
    .unwrap();
    let after: Unit = data.get("marine").unwrap();
    assert_eq!(
        after.weapon.unwrap().damage.bonus,
        BTreeMap::from([(Id::key("armored"), 2.0)])
    );
}
