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

// ---- The scopes an id is looked up in (`IdResolver::scopes`) ----

/// The root that `text`, a data document, evaluates to.
fn evaluate(text: &str) -> crol_eval::Value {
    let mut sources = crol_eval::Sources::new();
    let file = sources.add("a.crod".to_owned(), text);
    let layers = [crol_eval::Layer {
        name: "base".to_owned(),
        documents: vec![("a.crod".to_owned(), file)],
    }];
    let evaluation = crol_eval::evaluate_data(&sources, &layers, &Default::default());
    assert!(!evaluation.has_errors(), "{:?}", evaluation.diagnostics);
    evaluation.value.unwrap()
}

/// A line for each chain: where the id is, then the chain as text.
fn lines(chains: &[crol::ScopeChain]) -> String {
    chains
        .iter()
        .map(|chain| format!("{}: {chain}\n", chain.place()))
        .collect()
}

fn key(name: &str) -> crol::DataStep {
    crol::DataStep::Key(crol_eval::Key::Str(name.into()))
}

const NESTED: &str = r#"
+filter: 1,
+web: { port: 80 },
#[scope]
+dashboard: {
    filter: 2,
    list: 3,
    #[scope]
    panel: { filter: 4, ids: [@filter, @list, @web, @nope] },
    paths: (@web.port, @web.nope, @::filter, @::nope, @::dashboard.panel.filter, @"odd key"),
},
"#;

#[test]
fn the_scopes_of_an_id_innermost_first() {
    let root = evaluate(NESTED);
    let resolver = crol::IdResolver::new(&root);
    let location = DataPath::new([
        key("dashboard"),
        key("panel"),
        key("ids"),
        crol::DataStep::Index(0),
    ]);
    let chain = resolver.scopes(&Id::key("filter"), &location);
    assert_eq!(
        chain.target,
        resolver.resolve(&Id::key("filter"), &location)
    );
    assert_eq!(
        chain.target.as_ref().unwrap().to_string(),
        "dashboard.panel.filter"
    );
    assert_eq!(chain.problem, None);
    let scopes: Vec<_> = chain
        .scopes
        .iter()
        .map(|scope| (scope.path.to_string(), scope.kind, scope.has_key))
        .collect();
    assert_eq!(
        scopes,
        [
            ("dashboard.panel".to_owned(), crol::ScopeKind::Flag, true),
            ("dashboard".to_owned(), crol::ScopeKind::Flag, true),
            (String::new(), crol::ScopeKind::Root, true),
        ]
    );
    assert_eq!(chain.found, Some(0));
    assert_eq!(chain.found().unwrap().path.to_string(), "dashboard.panel");

    // One that only the outer scope has, and one that none has.
    let chain = resolver.scopes(&Id::key("list"), &location);
    assert_eq!(chain.found, Some(1));
    let chain = resolver.scopes(&Id::key("nope"), &location);
    assert_eq!((chain.found, chain.target.clone()), (None, None));
    assert!(chain.problem.is_some());
    assert!(chain.scopes.iter().all(|scope| !scope.has_key));

    // From the root, only the root is asked.
    let rooted: Id = "@::filter".parse().unwrap();
    let chain = resolver.scopes(&rooted, &location);
    assert_eq!(chain.found, Some(2));
    assert_eq!(chain.target.unwrap().to_string(), "filter");
}

#[test]
fn shadowing_unresolved_and_path_ids_as_text() {
    let root = evaluate(NESTED);
    let chains = crol::IdResolver::new(&root).chains();
    expect_test::expect![[r#"
        dashboard.panel.ids[0]: the id `@filter` names `dashboard.panel.filter`; scopes: `dashboard.panel` (`#[scope]`, has `filter`), `dashboard` (`#[scope]`, has `filter`, shadowed), the root (has `filter`, shadowed)
        dashboard.panel.ids[1]: the id `@list` names `dashboard.list`; scopes: `dashboard.panel` (`#[scope]`, no `list`), `dashboard` (`#[scope]`, has `list`), the root (no `list`)
        dashboard.panel.ids[2]: the id `@web` names `web`; scopes: `dashboard.panel` (`#[scope]`, no `web`), `dashboard` (`#[scope]`, no `web`), the root (has `web`)
        dashboard.panel.ids[3]: the id `@nope` names nothing; scopes: `dashboard.panel` (`#[scope]`, no `nope`), `dashboard` (`#[scope]`, no `nope`), the root (no `nope`)
        dashboard.paths.0: the id `@web.port` names `web.port`; scopes: `dashboard` (`#[scope]`, no `web`), the root (has `web`)
        dashboard.paths.1: the id `@web.nope` names nothing: `web` has no key `nope`; scopes: `dashboard` (`#[scope]`, no `web`), the root (has `web`)
        dashboard.paths.2: the id `@::filter` names `filter`; scopes: `dashboard` (`#[scope]`, passed by `::`), the root (has `filter`)
        dashboard.paths.3: the id `@::nope` names nothing; scopes: `dashboard` (`#[scope]`, passed by `::`), the root (no `nope`)
        dashboard.paths.4: the id `@::dashboard.panel.filter` names `dashboard.panel.filter`; scopes: `dashboard` (`#[scope]`, passed by `::`), the root (has `dashboard`)
        dashboard.paths.5: the id `@"odd key"` names nothing; scopes: `dashboard` (`#[scope]`, no `"odd key"`), the root (no `"odd key"`)
    "#]].assert_eq(&lines(&chains));
}

#[test]
fn the_ids_within_a_place() {
    let root = evaluate(NESTED);
    let resolver = crol::IdResolver::new(&root);
    let place = DataPath::new([key("dashboard"), key("panel")]);
    let within = resolver.within(&place);
    let ids: Vec<String> = within.ids().iter().map(|id| id.id.to_string()).collect();
    assert_eq!(ids, ["@filter", "@list", "@web", "@nope"]);
    assert_eq!(within.chains().len(), 4);
    // The place itself, when it is an id; `linked` only starts alike.
    let root = evaluate("+web: 1,\n+link: @web,\n+linked: @web,\n");
    let place = DataPath::new([key("link")]);
    let chains = crol::IdResolver::new(&root).within(&place).chains();
    expect_test::expect![[r#"
        link: the id `@web` names `web`; scopes: the root (has `web`)
    "#]]
    .assert_eq(&lines(&chains));
}

#[test]
fn a_scope_says_whether_the_data_or_the_schema_makes_it_one() {
    // `settings` has the flag as well, which is what counts: a loader needs
    // no schema for it. Without the schema, `dashboard` is no scope.
    let text = r#"
        +filter: { placeholder: "root" },
        dashboard: Screen {
            layout: Stack { children: [(Length(1), @filter), (Fill(1), @list)] },
            +filter: TextInput { placeholder: "Filter" },
        }
        #[scope]
        settings: Screen {
            layout: Stack { children: [(Length(1), @filter)] },
            +filter: TextInput { placeholder: "Search" },
        }
    "#;
    let root = evaluate(text);
    let schema = crol::schema::build::<BTreeMap<String, Screen>>();
    let typed = crol::IdResolver::new(&root).schema(&schema).chains();
    expect_test::expect![[r#"
        dashboard.layout.children[0].1: the id `@filter` names `dashboard.filter`; scopes: `dashboard` (`#[crol(scope)]`, has `filter`), the root (has `filter`, shadowed)
        dashboard.layout.children[1].1: the id `@list` names nothing; scopes: `dashboard` (`#[crol(scope)]`, no `list`), the root (no `list`)
        settings.layout.children[0].1: the id `@filter` names `settings.filter`; scopes: `settings` (`#[scope]`, has `filter`), the root (has `filter`, shadowed)
    "#]].assert_eq(&lines(&typed));
    let plain = crol::IdResolver::new(&root).chains();
    expect_test::expect![[r#"
        dashboard.layout.children[0].1: the id `@filter` names `filter`; scopes: the root (has `filter`)
        dashboard.layout.children[1].1: the id `@list` names nothing; scopes: the root (no `list`)
        settings.layout.children[0].1: the id `@filter` names `settings.filter`; scopes: `settings` (`#[scope]`, has `filter`), the root (has `filter`, shadowed)
    "#]].assert_eq(&lines(&plain));
}

#[test]
fn id_keys_have_their_scopes_at_the_map() {
    // An id key resolves at its map's place, and is written at its entry's.
    let text = r#"
+db: 0,
#[scope]
+screen: {
    db: 1,
    weights: { @db: 2, @::db: 3, inner: [@db] },
},
"#;
    let root = evaluate(text);
    let chains = crol::IdResolver::new(&root).chains();
    expect_test::expect![[r#"
        screen.weights[@db]: the id `@db` names `screen.db`; scopes: `screen` (`#[scope]`, has `db`), the root (has `db`, shadowed)
        screen.weights[@::db]: the id `@::db` names `db`; scopes: `screen` (`#[scope]`, passed by `::`), the root (has `db`)
        screen.weights.inner[0]: the id `@db` names `screen.db`; scopes: `screen` (`#[scope]`, has `db`), the root (has `db`, shadowed)
    "#]].assert_eq(&lines(&chains));
    assert_eq!(
        chains.iter().map(|chain| chain.is_key).collect::<Vec<_>>(),
        [true, true, false]
    );
    // Within the map, its keys; within one entry, its key only.
    let resolver = crol::IdResolver::new(&root);
    let map = DataPath::new([key("screen"), key("weights")]);
    assert_eq!(resolver.within(&map).chains().len(), 3);
    let entry = chains[0].place();
    let found = resolver.within(&entry).chains();
    expect_test::expect![[r#"
        screen.weights[@db]: the id `@db` names `screen.db`; scopes: `screen` (`#[scope]`, has `db`), the root (has `db`, shadowed)
    "#]].assert_eq(&lines(&found));
}
