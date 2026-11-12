//! Type names on blocks (`SPEC.md` 10.2): on structs, and on the keyed
//! values that become maps, newtypes and transparent structs.

#![allow(missing_docs, dead_code)]

use std::collections::BTreeMap;

use crol::{CrolComposer, CrolData, CrolLayer};
use crol_derive::Schema;
use expect_test::{Expect, expect};
use serde_derive::Deserialize;

#[derive(Debug, Deserialize, Schema)]
enum Node {
    TextInput { placeholder: String },
    List { vim_keys: bool },
}

/// A newtype around a map: serde gives its name.
#[derive(Debug, Deserialize, Schema)]
struct NodeScreen(BTreeMap<String, Node>);

/// The name to compare is the serde name.
#[derive(Debug, Deserialize, Schema)]
#[serde(rename = "Screen")]
struct Renamed(BTreeMap<String, Node>);

/// A transparent struct around a map: serde gives no name, the schema does.
#[derive(Debug, Deserialize, Schema)]
#[serde(transparent)]
struct Panel(BTreeMap<String, Node>);

#[derive(Debug, Deserialize, Schema)]
struct Limits {
    cpu: u8,
}

/// A newtype around a struct.
#[derive(Debug, Deserialize, Schema)]
struct Quota(Limits);

/// A transparent struct around a struct.
#[derive(Debug, Deserialize, Schema)]
#[serde(transparent)]
struct Budget(Limits);

/// A newtype around a transparent struct around a map.
#[derive(Debug, Deserialize, Schema)]
struct Window(Panel);

#[derive(Debug, Deserialize, Schema)]
enum Shape {
    /// A newtype variant whose payload is a map.
    Sizes(BTreeMap<String, u8>),
}

#[derive(Debug, Deserialize, Schema)]
struct App {
    screen: NodeScreen,
    renamed: Renamed,
    panel: Option<Panel>,
    quota: Quota,
    budget: Budget,
    window: Window,
    shape: Shape,
    /// A map and no more: nothing names its type.
    stats: BTreeMap<String, u8>,
}

fn err<T: for<'de> serde::Deserialize<'de> + std::fmt::Debug>(text: &str) -> String {
    crol::from_str::<T>(text).unwrap_err().to_string()
}

fn ok<T: for<'de> serde::Deserialize<'de>>(text: &str) {
    if let Err(error) = crol::from_str::<T>(text) {
        panic!("{text}: {error}");
    }
}

#[test]
fn a_struct_has_its_name() {
    ok::<Limits>("Limits { cpu: 1 }");
    ok::<Limits>("{ cpu: 1 }");
    expect!["expected a `Limits`, found a `Limit` (<string>:1:1)"]
        .assert_eq(&err::<Limits>("Limit { cpu: 1 }"));
}

#[test]
fn a_newtype_around_a_map_has_its_name() {
    let entries = r#"filter: TextInput { placeholder: "Filter" }"#;
    ok::<NodeScreen>(&format!("NodeScreen {{ {entries} }}"));
    ok::<NodeScreen>(&format!("{{ {entries} }}"));
    ok::<NodeScreen>(&format!("NodeScreen({{ {entries} }})"));
    expect!["`Scren { ... }` is not a `NodeScreen` (<string>:1:1)"]
        .assert_eq(&err::<NodeScreen>(&format!("Scren {{ {entries} }}")));
    // Where it is a field, an element or an entry as well.
    let maps = err::<BTreeMap<String, NodeScreen>>("a: NodeScreen {}, b: {}, c: Scren {}");
    expect!["`Scren { ... }` is not a `NodeScreen` (<string>:1:29, while deserializing c)"]
        .assert_eq(&maps);
    // In `N(x)`, `x` is the map itself, which nothing names.
    ok::<NodeScreen>("NodeScreen(Scren {})");
}

#[test]
fn a_renamed_newtype_has_its_serde_name() {
    ok::<Renamed>("Screen {}");
    expect!["`Renamed { ... }` is not a `Screen` (<string>:1:1)"]
        .assert_eq(&err::<Renamed>("Renamed {}"));
}

#[test]
fn a_newtype_around_a_struct_has_both_names() {
    ok::<Quota>("Quota { cpu: 1 }");
    ok::<Quota>("Limits { cpu: 1 }");
    ok::<Quota>("{ cpu: 1 }");
    ok::<Quota>("Quota(Limits { cpu: 1 })");
    expect!["expected a `Limits`, found a `Limit` (<string>:1:1)"]
        .assert_eq(&err::<Quota>("Limit { cpu: 1 }"));
    // An optional one, with and without `Some`.
    ok::<Option<Quota>>("Quota { cpu: 1 }");
    ok::<Option<NodeScreen>>("Some(NodeScreen {})");
    expect!["`Scren { ... }` is not a `NodeScreen` (<string>:1:1)"]
        .assert_eq(&err::<Option<NodeScreen>>("Scren {}"));
}

#[test]
fn a_newtype_variant_names_a_map() {
    ok::<Shape>("Sizes { a: 1 }");
    ok::<Shape>("Sizes({ a: 1 })");
    expect!["`Shape` has no variant `Size`; its variants are `Sizes` (<string>:1:1)"]
        .assert_eq(&err::<Shape>("Size { a: 1 }"));
}

/// Serde forwards a transparent struct to what it wraps, and names nothing
/// for a map: without a schema, any name loads.
#[test]
fn without_a_schema_a_transparent_struct_and_a_map_take_any_name() {
    ok::<Panel>("Panel {}");
    ok::<Panel>("Panle {}");
    ok::<BTreeMap<String, u8>>("Stats { a: 1 }");
    // A struct in a transparent one has its own name only.
    ok::<Budget>("Limits { cpu: 1 }");
    expect!["expected a `Limits`, found a `Budget` (<string>:1:1)"]
        .assert_eq(&err::<Budget>("Budget { cpu: 1 }"));
}

const APP: &str = r#"
screen: NodeScreen { filter: TextInput { placeholder: "Filter" } }
renamed: Screen {}
panel: Panel { list: List { vim_keys: true } }
quota: Quota { cpu: 1 }
budget: Budget { cpu: 1 }
window: Window {}
shape: Sizes { a: 1 }
stats: { a: 1 }
"#;

fn compose(text: &str) -> CrolData {
    let layer = CrolLayer::from_sources("base", [("a.crod", text)]);
    CrolComposer::new()
        .schema(crol::schema::build::<App>())
        .load_layer(layer)
        .compose_data()
        .unwrap()
}

/// What `crol::check` finds, as `severity: message (path)`.
fn checked(data: &CrolData) -> Vec<String> {
    let schema = crol::schema::build::<App>();
    crol::check(data, &schema, None, &Default::default())
        .into_iter()
        .map(|d| {
            let severity = if d.is_error() { "error" } else { "warning" };
            let path = d.context.unwrap_or_default();
            format!("{severity}: {} ({path})", d.message)
        })
        .collect()
}

/// `APP` with one line written differently: the loader and `crol::check`
/// report the same error, at the same place.
fn mismatch(from: &str, to: &str, expected: Expect) {
    assert!(APP.contains(from), "{from}");
    let data = compose(&APP.replace(from, to));
    let error = data.deserialize_root::<App>(None).unwrap_err();
    let found = crol::check_view(
        data.view(),
        &crol::schema::build::<App>(),
        None,
        &Default::default(),
    );
    assert_eq!(found.len(), 1, "{to}: {found:?}");
    assert!(found[0].is_error(), "{to}");
    assert_eq!(found[0].message, error.message(), "{to}");
    assert_eq!(Some(found[0].span), error.span(), "{to}");
    expected.assert_eq(&error.to_string());
}

#[test]
fn with_a_schema_the_names_of_wrappers_fit() {
    let data = compose(APP);
    assert!(data.warnings().is_empty(), "{:?}", data.warnings());
    assert_eq!(checked(&data), [""; 0]);
    data.deserialize_root::<App>(None).unwrap();
    // Each wrapper around a value names it, and so does what it wraps.
    let inner = APP
        .replace("window: Window {}", "window: Panel {}")
        .replace("quota: Quota {", "quota: Limits {")
        .replace("budget: Budget {", "budget: Limits {");
    let data = compose(&inner);
    assert!(data.warnings().is_empty(), "{:?}", data.warnings());
    assert_eq!(checked(&data), [""; 0]);
    data.deserialize_root::<App>(None).unwrap();
}

#[test]
fn with_a_schema_a_transparent_struct_has_its_name() {
    mismatch(
        "panel: Panel {",
        "panel: Panle {",
        expect!["`Panle { ... }` is not a `Panel` (base/a.crod:4:8, while deserializing panel)"],
    );
    // In `Some(x)` as well.
    mismatch(
        "panel: Panel { list: List { vim_keys: true } }",
        "panel: Some(Panle { list: List { vim_keys: true } })",
        expect!["`Panle { ... }` is not a `Panel` (base/a.crod:4:13, while deserializing panel)"],
    );
    mismatch(
        "budget: Budget {",
        "budget: Budgte {",
        expect![
            "expected a `Limits`, found a `Budgte` (base/a.crod:6:9, while deserializing budget)"
        ],
    );
    mismatch(
        "window: Window {",
        "window: Windw {",
        expect!["`Windw { ... }` is not a `Window` (base/a.crod:7:9, while deserializing window)"],
    );
}

#[test]
fn with_a_schema_a_newtype_reports_as_without() {
    mismatch(
        "screen: NodeScreen {",
        "screen: Scren {",
        expect![
            "`Scren { ... }` is not a `NodeScreen` (base/a.crod:2:9, while deserializing screen)"
        ],
    );
    mismatch(
        "renamed: Screen {",
        "renamed: Renamed {",
        expect![
            "`Renamed { ... }` is not a `Screen` (base/a.crod:3:10, while deserializing renamed)"
        ],
    );
    mismatch(
        "quota: Quota {",
        "quota: Quot {",
        expect!["expected a `Limits`, found a `Quot` (base/a.crod:5:8, while deserializing quota)"],
    );
}

/// Every way to deserialize composed data checks the names.
#[test]
fn every_way_to_load_checks_against_the_schema() {
    /// The root type is the type of each root value.
    type Root = BTreeMap<String, Panel>;
    let text = "main: Panel {}\nside: Panle {}";
    let layer = CrolLayer::from_sources("base", [("a.crod", text)]);
    let composer = || {
        CrolComposer::new()
            .schema(crol::schema::build::<Root>())
            .load_layer(layer.clone())
    };
    let data = composer().compose_data().unwrap();
    let expected = "`Panle { ... }` is not a `Panel` (base/a.crod:2:7, while deserializing side)";
    data.get::<Panel>("main").unwrap();
    assert_eq!(data.get::<Panel>("side").unwrap_err().to_string(), expected);
    let base = data.context(Vec::<String>::new()).unwrap();
    data.get_in::<Panel>("main", &base).unwrap();
    let error = data.get_in::<Panel>("side", &base).unwrap_err();
    assert_eq!(error.to_string(), expected);
    let error = data.deserialize_root::<Root>(None).unwrap_err();
    assert_eq!(error.to_string(), expected);
    let error = composer().compose::<Root>().unwrap_err();
    assert_eq!(error.to_string(), expected);
    // Without the schema, serde names nothing here.
    let plain = CrolComposer::new().load_layer(layer.clone());
    plain.compose::<Root>().unwrap();
}

/// A type name on a map that nothing names loads, with a warning.
#[test]
fn a_name_on_a_plain_map_is_a_warning() {
    let data = compose(&APP.replace("stats: {", "stats: Stats {"));
    expect!["warning: base/a.crod:9:8: the type name `Stats` names nothing: this is a map, which has no type name; write `{ ... }`"].assert_eq(&data.warnings().join("\n"));
    expect!["warning: the type name `Stats` names nothing: this is a map, which has no type name; write `{ ... }` (stats)"].assert_eq(&checked(&data).join("\n"));
    let app: App = data.deserialize_root(None).unwrap();
    assert_eq!(app.stats["a"], 1);
    // Without a schema nothing says it is a map and no more.
    let layer = CrolLayer::from_sources("base", [("a.crod", "stats: Stats { a: 1 }")]);
    let data = CrolComposer::new()
        .load_layer(layer)
        .compose_data()
        .unwrap();
    assert!(data.warnings().is_empty(), "{:?}", data.warnings());
}

/// A reload finds the warning again, and loses it with the name.
#[test]
fn a_reload_keeps_the_warnings_current() {
    let mut data = compose(APP);
    let named = APP.replace("stats: {", "stats: Stats {");
    let change = |text| crol::DocumentChange {
        layer: "base",
        path: "a.crod",
        text: Some(text),
    };
    data.reload(&[change(&named)]).unwrap();
    assert_eq!(data.warnings().len(), 1, "{:?}", data.warnings());
    data.reload(&[change(APP)]).unwrap();
    assert!(data.warnings().is_empty(), "{:?}", data.warnings());
}

/// A value document against a schema type.
#[test]
fn check_value_checks_the_names() {
    let schema = crol::schema::build::<App>();
    let check = |text: &str, root: &str| -> Vec<String> {
        let value: crol::RawValue = crol::from_str(text).unwrap();
        let root = schema.type_path(root).unwrap();
        crol::check_value(&value.0, &schema, Some(root), &Default::default())
            .into_iter()
            .map(|d| {
                let severity = if d.is_error() { "error" } else { "warning" };
                format!("{severity}: {}", d.message)
            })
            .collect()
    };
    assert_eq!(check("Panel {}", "Panel"), [""; 0]);
    assert_eq!(
        check("Panle {}", "Panel"),
        ["error: `Panle { ... }` is not a `Panel`"]
    );
    assert_eq!(check("Window {}", "Window"), [""; 0]);
    assert_eq!(check("Panel {}", "Window"), [""; 0]);
    assert_eq!(
        check("Windw {}", "Window"),
        ["error: `Windw { ... }` is not a `Window`"]
    );
}

/// A newtype around a map keyed by ids.
#[derive(Debug, Deserialize, Schema)]
struct Bonus(BTreeMap<crol::Id, u8>);

/// A transparent struct around one.
#[derive(Debug, Deserialize, Schema)]
#[serde(transparent)]
struct Links(BTreeMap<crol::Id, u8>);

#[derive(Debug, Deserialize, Schema)]
struct Keyed {
    bonus: Bonus,
    links: Links,
    plain: BTreeMap<crol::Id, u8>,
}

/// Maps keyed by ids have the names of what wraps them, and their keys are
/// checked as ids all the same (`SPEC.md` 10.2, 10.5).
#[test]
fn maps_keyed_by_ids_have_the_names_of_what_wraps_them() {
    let schema = crol::schema::build::<Keyed>();
    let check = |text: &str| -> Vec<String> {
        let value: crol::RawValue = crol::from_str(text).unwrap();
        crol::check_value(&value.0, &schema, None, &Default::default())
            .into_iter()
            .map(|d| {
                let severity = if d.is_error() { "error" } else { "warning" };
                format!("{severity}: {}", d.message)
            })
            .collect()
    };
    let good =
        "Keyed { bonus: Bonus { @bonus: 1 }, links: Links { @links: 2 }, plain: { @plain: 3 } }";
    assert_eq!(check(good), [""; 0]);
    crol::from_str_with_schema::<Keyed>(good, &schema).unwrap();
    // A name on the plain map is a warning, and the data loads.
    let named = good.replace("plain: {", "plain: Plain {");
    assert_eq!(
        check(&named),
        [
            "warning: the type name `Plain` names nothing: this is a map, which has no type name; write `{ ... }`"
        ]
    );
    crol::from_str::<Keyed>(&named).unwrap();
    // A wrong name on a wrapper is an error, the newtype's without a schema.
    let wrong = good
        .replace("Bonus {", "Bonsu {")
        .replace("Links {", "Linsk {");
    assert_eq!(
        check(&wrong),
        [
            "error: `Bonsu { ... }` is not a `Bonus`",
            "error: `Linsk { ... }` is not a `Links`",
        ]
    );
    expect!["`Bonsu { ... }` is not a `Bonus` (<string>:1:16, while deserializing bonus)"]
        .assert_eq(&err::<Keyed>(&wrong));
    // A key of the wrong kind is reported under a good name as well.
    let string_key = good.replace("@links: 2", "links: 2");
    let found = check(&string_key);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].starts_with("error: "), "{found:?}");
    assert!(!found[0].contains("is not a"), "{found:?}");

    // In layered data the loader has the schema, and with it the name of
    // the transparent struct, as `crol::check` does.
    let layered = "bonus: Bonus { @bonus: 1 }\nlinks: Linsk { @links: 2 }\nplain: { @plain: 3 }\n";
    let layer = CrolLayer::from_sources("base", [("a.crod", layered)]);
    let data = CrolComposer::new()
        .schema(crol::schema::build::<Keyed>())
        .load_layer(layer)
        .compose_data()
        .unwrap();
    let error = data.deserialize_root::<Keyed>(None).unwrap_err();
    let found = crol::check(&data, &schema, None, &Default::default());
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].message, error.message());
    assert_eq!(Some(found[0].span), error.span());
    expect!["`Linsk { ... }` is not a `Links` (base/a.crod:2:8, while deserializing links)"]
        .assert_eq(&error.to_string());
}
