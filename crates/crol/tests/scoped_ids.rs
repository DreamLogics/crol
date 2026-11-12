//! Scoped ids (`SPEC.md` 3.2, 10.5): what an id names, through scopes from
//! `#[scope]` and `#[crol(scope)]`.

#![allow(missing_docs, dead_code)]

use std::collections::BTreeMap;

use crol::{CrolComposer, CrolConfig, CrolData, CrolLayer, DataPath, DocumentChange, Id, IdUse};
use crol_derive::Schema;
use serde_derive::Deserialize;

#[derive(Debug, Deserialize, Schema)]
enum Size {
    Length(u16),
    Fill(u16),
}

#[derive(Debug, Deserialize, Schema)]
struct Stack {
    children: Vec<(Size, Id)>,
}

/// A component. Under `#[serde(flatten)]` serde reads it through `any`,
/// which drops type names, so it is a struct rather than an enum.
#[derive(Debug, Deserialize, Schema)]
struct Component {
    #[serde(default)]
    placeholder: Option<String>,
    #[serde(default)]
    vim_keys: Option<bool>,
}

/// A screen of a presentation file: a scope for the components it places.
#[derive(Debug, Deserialize, Schema)]
#[crol(scope)]
struct Screen {
    layout: Stack,
    #[serde(flatten)]
    components: BTreeMap<String, Component>,
}

const SCREENS: &str = r#"
dashboard: Screen {
    layout: Stack { children: [(Length(1), @filter), (Fill(1), @list)] },
    +filter: TextInput { placeholder: "Filter workers" },
    +list: List { vim_keys: true },
}

settings: Screen {
    layout: Stack { children: [(Length(1), @filter), (Fill(1), @list)] },
    +filter: TextInput { placeholder: "Search settings" },
    +list: List { vim_keys: false },
}
"#;

fn compose(documents: &[(&str, &str)], schema: bool) -> Result<CrolData, crol::Error> {
    compose_with(documents, schema, CrolConfig::default())
}

fn compose_with(
    documents: &[(&str, &str)],
    schema: bool,
    config: CrolConfig,
) -> Result<CrolData, crol::Error> {
    let layer = CrolLayer::from_sources("base", documents.iter().copied());
    let mut composer = CrolComposer::with_cfg(config);
    if schema {
        composer = composer.schema(crol::schema::build::<BTreeMap<String, Screen>>());
    }
    composer.load_layer(layer).compose_data()
}

fn targets(ids: &[IdUse]) -> Vec<String> {
    ids.iter()
        .map(|id| {
            let target = id
                .target
                .as_ref()
                .map_or("nothing".to_owned(), DataPath::to_string);
            format!("{} -> {target}", id.id)
        })
        .collect()
}

#[test]
fn each_screen_names_its_own_components() {
    let data = compose(&[("ui.crod", SCREENS)], true).unwrap();
    for screen in ["dashboard", "settings"] {
        let (_, ids): (Screen, _) = data.get_with_ids(screen).unwrap();
        assert_eq!(
            targets(&ids),
            [
                format!("@filter -> {screen}.filter"),
                format!("@list -> {screen}.list")
            ]
        );
        assert!(
            ids.iter()
                .all(|id| data.contains_path(id.target.as_ref().unwrap()))
        );
    }
    assert!(data.warnings().is_empty(), "{:?}", data.warnings());
}

#[test]
fn without_the_schema_ids_name_root_keys() {
    // The loader needs the schema for `#[crol(scope)]` (`SPEC.md` 10.5).
    let data = compose(&[("ui.crod", SCREENS)], false).unwrap();
    let (_, ids): (Screen, _) = data.get_with_ids("dashboard").unwrap();
    assert_eq!(targets(&ids), ["@filter -> nothing", "@list -> nothing"]);
    assert_eq!(data.warnings().len(), 4, "{:?}", data.warnings());
}

#[test]
fn scope_in_the_data() {
    let text = r#"
        db: { host: "prod-db" }
        services: {
            #[scope]
            staging: {
                db: { host: "staging-db" },
                web: { depends_on: [@db, @::db, @services.staging.db, @cache] },
            },
            web: { depends_on: [@db] },
        }
    "#;
    let data = compose(&[("a.crod", text)], false).unwrap();
    #[derive(Deserialize)]
    struct Web {
        depends_on: Vec<Id>,
    }
    #[derive(Deserialize)]
    struct Host {
        host: String,
    }
    #[derive(Deserialize)]
    struct Staging {
        db: Host,
        web: Web,
    }
    #[derive(Deserialize)]
    struct Services {
        staging: Staging,
        web: Web,
    }
    let (_, ids): (Services, _) = data.get_with_ids("services").unwrap();
    assert_eq!(
        targets(&ids),
        [
            "@db -> services.staging.db",
            "@::db -> db",
            "@services.staging.db -> services.staging.db",
            "@cache -> nothing",
            "@db -> db",
        ]
    );
    assert_eq!(
        data.warnings(),
        [
            "warning: base/a.crod:7:71: the id `@cache` names nothing: no `cache` in `services.staging` or the root"
        ]
    );
}

#[test]
fn shadowing_and_path_ids_across_scopes() {
    let text = r#"
        filter: { root: true }
        dashboard: Screen {
            layout: Stack { children: [(Length(1), @filter), (Fill(1), @settings.filter)] },
            +filter: TextInput { placeholder: "a" },
        }
        settings: Screen {
            layout: Stack { children: [(Length(1), @::filter), (Fill(1), @dashboard.nope)] },
            +filter: TextInput { placeholder: "b" },
        }
    "#;
    let data = compose(&[("a.crod", text)], true).unwrap();
    let (_, ids): (Screen, _) = data.get_with_ids("dashboard").unwrap();
    assert_eq!(
        targets(&ids),
        [
            "@filter -> dashboard.filter",
            "@settings.filter -> settings.filter"
        ]
    );
    let (_, ids): (Screen, _) = data.get_with_ids("settings").unwrap();
    assert_eq!(
        targets(&ids),
        ["@::filter -> filter", "@dashboard.nope -> nothing"]
    );
    assert_eq!(data.type_name_at(ids[0].target.as_ref().unwrap()), None);
}

#[test]
fn ids_resolve_where_a_template_puts_them() {
    let text = r#"
        pane := { layout: Stack { children: [(Fill(1), @filter)] } }
        dashboard: Screen { ..&pane, +filter: TextInput { placeholder: "a" } }
        settings: Screen { ..&pane, +filter: TextInput { placeholder: "b" } }
    "#;
    let data = compose(&[("a.crod", text)], true).unwrap();
    let (_, a): (Screen, _) = data.get_with_ids("dashboard").unwrap();
    let (_, b): (Screen, _) = data.get_with_ids("settings").unwrap();
    assert_eq!(targets(&a), ["@filter -> dashboard.filter"]);
    assert_eq!(targets(&b), ["@filter -> settings.filter"]);
    // Both were written once, in the template.
    assert_eq!(a[0].span, b[0].span);
}

#[test]
fn strict_ids_are_errors_and_a_reload_keeps_the_data() {
    let config = CrolConfig {
        strict_ids: true,
        ..CrolConfig::default()
    };
    let error = compose_with(&[("a.crod", "web: { depends_on: [@db] }")], false, config)
        .unwrap_err()
        .to_string();
    assert!(error.contains("the id `@db` names no root key"), "{error}");

    let config = CrolConfig {
        strict_ids: true,
        ..CrolConfig::default()
    };
    let documents = [
        ("a.crod", "web: { depends_on: [@db] }"),
        ("b.crod", "db: {}"),
    ];
    let mut data = compose_with(&documents, false, config).unwrap();
    let removed = data.reload(&[DocumentChange {
        layer: "base",
        path: "b.crod",
        text: Some("cache: {}"),
    }]);
    assert!(removed.is_err());
    // The last good data stays.
    assert!(data.contains("db"));
}

#[test]
fn a_reload_resolves_every_id_again() {
    let documents = [
        ("a.crod", "web: { depends_on: [@db] }"),
        ("b.crod", "cache: {}"),
    ];
    let mut data = compose(&documents, false).unwrap();
    assert_eq!(data.warnings().len(), 1);
    // `web` neither changes nor reads `db`, but what `@db` names does.
    data.reload(&[DocumentChange {
        layer: "base",
        path: "b.crod",
        text: Some("db: {}"),
    }])
    .unwrap();
    assert!(data.warnings().is_empty(), "{:?}", data.warnings());
    #[derive(Deserialize)]
    struct Web {
        depends_on: Vec<Id>,
    }
    let (_, ids): (Web, _) = data.get_with_ids("web").unwrap();
    assert_eq!(targets(&ids), ["@db -> db"]);
}

#[test]
fn map_keys_resolve_at_the_map() {
    #[derive(Debug, Deserialize, Schema)]
    struct Empty {}
    #[derive(Debug, Deserialize, Schema)]
    struct Weights {
        by: BTreeMap<Id, u32>,
    }
    #[derive(Debug, Deserialize, Schema)]
    struct Root {
        db: Empty,
        weights: Weights,
    }
    let load = |text: &str| {
        CrolComposer::new()
            .schema(crol::schema::build::<Root>())
            .load_layer(CrolLayer::from_sources("base", [("a.crod", text)]))
            .compose_data()
    };
    let data = load("db: {}\nweights: { by: { @db: 1 } }").unwrap();
    assert!(data.warnings().is_empty(), "{:?}", data.warnings());
    let (_, ids): (Weights, _) = data.get_with_ids("weights").unwrap();
    assert_eq!(targets(&ids), ["@db -> db"]);
    // An id key has to name an entry: keys are compared by what they name
    // (`SPEC.md` 3.2).
    let error = load("db: {}\nweights: { by: { @db: 1, @gone: 2 } }").unwrap_err();
    assert_eq!(
        error.to_string(),
        "base/a.crod:2:26: the id key `@gone` names no root key; an id key has to name an \
         entry, and a key that names nothing is written as a string (while evaluating \
         weights.by)"
    );
}

#[test]
fn value_mode_has_scopes_from_the_data() {
    #[derive(Deserialize)]
    struct Env {
        web: Id,
        #[serde(default)]
        db: Option<BTreeMap<String, u8>>,
    }
    #[derive(Deserialize)]
    struct Doc {
        db: BTreeMap<String, u8>,
        envs: BTreeMap<String, Env>,
    }
    let text = r#"{
        db: {},
        envs: {
            #[scope]
            staging: { web: @db, db: {} },
            prod: { web: @db, db: None },
        },
    }"#;
    let config = crol::from_str_with_ids::<Doc>(text);
    let (_, ids) = config.unwrap();
    assert_eq!(targets(&ids), ["@db -> envs.staging.db", "@db -> db"]);
}

#[test]
fn check_resolves_through_scopes() {
    let text = r#"
        dashboard: Screen {
            layout: Stack { children: [(Length(1), @filter), (Fill(1), @settings.nope)] },
            +filter: { placeholder: "a" },
        }
        settings: Screen {
            layout: Stack { children: [(Length(1), @list)] },
        }
    "#;
    let data = compose(&[("a.crod", text)], true).unwrap();
    let schema = crol::schema::build::<BTreeMap<String, Screen>>();
    let messages: Vec<String> = crol::check(&data, &schema, None, &Default::default())
        .into_iter()
        .map(|d| d.message)
        .collect();
    assert_eq!(
        messages,
        [
            "the id `@settings.nope` names nothing: `settings` has no key `nope`",
            "the id `@list` names nothing: no `list` in `settings` or the root",
        ]
    );
}

#[derive(Debug, Deserialize, Schema)]
enum Node {
    Stack { children: Vec<(Size, Id)> },
    TextInput { placeholder: String },
    List { vim_keys: bool },
}

/// A screen as a map of nodes, `layout` among them: enum components keep
/// their names, which `#[serde(flatten)]` would drop.
#[derive(Debug, Deserialize, Schema)]
#[serde(transparent)]
#[crol(scope)]
struct NodeScreen(BTreeMap<String, Node>);

/// The same as a newtype.
#[derive(Debug, Deserialize, Schema)]
#[crol(scope)]
struct NewtypeScreen(BTreeMap<String, Node>);

#[test]
fn newtypes_and_transparent_structs_can_be_scopes() {
    let text = r#"
        dashboard: Screen {
            layout: Stack { children: [(Length(1), @filter), (Fill(1), @list)] },
            +filter: TextInput { placeholder: "Filter workers" },
            +list: List { vim_keys: true },
        }
        settings: {
            layout: Stack { children: [(Length(1), @filter)] },
            +filter: TextInput { placeholder: "Search settings" },
        }
    "#;
    fn check<S: crol::schema::Schema + for<'de> serde::Deserialize<'de>>(text: &str) {
        let layer = CrolLayer::from_sources("base", [("a.crod", text)]);
        let data = CrolComposer::new()
            .schema(crol::schema::build::<BTreeMap<String, S>>())
            .load_layer(layer)
            .compose_data()
            .unwrap();
        assert!(data.warnings().is_empty(), "{:?}", data.warnings());
        let (_, ids): (S, _) = data.get_with_ids("dashboard").unwrap();
        assert_eq!(
            targets(&ids),
            ["@filter -> dashboard.filter", "@list -> dashboard.list"]
        );
        // Without a type name, the root type says it is a screen.
        let (_, ids): (S, _) = data.get_with_ids("settings").unwrap();
        assert_eq!(targets(&ids), ["@filter -> settings.filter"]);
    }
    check::<NodeScreen>(text);
    check::<NewtypeScreen>(&text.replace("Screen {", "NewtypeScreen {"));
}
