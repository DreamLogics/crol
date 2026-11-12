//! `crol::IdOr` (`SPEC.md` 10.5, 10.8): an id, or a value written in place.

#![allow(missing_docs, dead_code)]

use std::collections::{BTreeMap, HashMap};

use crol::schema::TypeExpr;
use crol::{Id, IdOr, PrettyConfig};
use serde::Deserialize as _;
use serde::de::IntoDeserializer;
use serde_derive::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Deserialize, Serialize)]
enum Size {
    Length(u16),
    Fill(u16),
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
enum Node {
    Stack {
        children: Vec<(Size, IdOr<Box<Node>>)>,
    },
    Label {
        text: String,
        #[serde(default)]
        muted: bool,
    },
    Panel {
        title: String,
        child: IdOr<Box<Node>>,
    },
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
struct Screen {
    layout: Node,
}

const SCREEN: &str = r#"
layout: Stack {
    children: [
        (Fill(1), @body),
        (Length(1), Label { text: "q: quit", muted: true }),
        (Fill(1), Panel {
            title: "Workers",
            child: Stack { children: [(Length(1), @filter), (Fill(1), Label { text: "x", .. })] },
        }),
    ],
}
"#;

fn id(text: &str) -> Id {
    text.parse().unwrap()
}

#[test]
fn ids_and_values_in_place() {
    let (screen, ids): (Screen, _) = crol::from_str_with_ids(SCREEN).unwrap();
    let Node::Stack { children } = &screen.layout else {
        panic!("{screen:?}")
    };
    assert_eq!(children[0].1, IdOr::Id(id("@body")));
    assert_eq!(
        children[1].1,
        IdOr::Value(Box::new(Node::Label {
            text: "q: quit".into(),
            muted: true
        }))
    );
    // A value written in place keeps its type name and defaults, and the
    // ids inside it are reported with their place.
    let IdOr::Value(panel) = &children[2].1 else {
        panic!("{children:?}")
    };
    let Node::Panel { child, .. } = &**panel else {
        panic!("{panel:?}")
    };
    let IdOr::Value(stack) = child else {
        panic!("{child:?}")
    };
    let Node::Stack { children: inner } = &**stack else {
        panic!("{stack:?}")
    };
    assert_eq!(inner[0].1, IdOr::Id(id("@filter")));
    assert_eq!(
        inner[1].1,
        IdOr::Value(Box::new(Node::Label {
            text: "x".into(),
            muted: false
        }))
    );
    let used: Vec<_> = ids
        .iter()
        .map(|used| format!("{} at {}", used.id, used.rust_path))
        .collect();
    assert_eq!(
        used,
        [
            "@body at layout.children[0][1]",
            "@filter at layout.children[2][1].child.children[0][1]",
        ]
    );
}

#[test]
fn an_error_inside_a_value_has_its_place() {
    let text = "layout: Panel { title: \"x\", child: Stak { children: [] } }";
    let error = crol::from_str::<Screen>(text).unwrap_err().to_string();
    assert!(error.contains("Stak"), "{error}");
    assert!(error.contains("1:36"), "{error}");
    assert!(error.contains("layout.child"), "{error}");
}

#[test]
fn an_id_is_always_the_id_side() {
    #[derive(Debug, Deserialize)]
    struct Volumes {
        volumes: Vec<IdOr<String>>,
    }
    let volumes: Volumes = crol::from_str(r#"volumes: [@web, "web", "@web"]"#).unwrap();
    assert_eq!(
        volumes.volumes,
        [
            IdOr::Id(id("@web")),
            IdOr::Value("web".into()),
            IdOr::Value("@web".into()),
        ]
    );
    // Read from CROL directly, a string that is no id is just a string.
    let volumes: Volumes = crol::from_str(r#"volumes: ["@ hi", "@"]"#).unwrap();
    assert_eq!(
        volumes.volumes,
        [IdOr::Value("@ hi".into()), IdOr::Value("@".into())]
    );
}

#[test]
fn optional() {
    #[derive(Debug, Deserialize)]
    struct Unit {
        weapon: Option<IdOr<String>>,
    }
    let unit: Unit = crol::from_str("weapon: Some(@rifle)").unwrap();
    assert_eq!(unit.weapon, Some(IdOr::Id(id("@rifle"))));
    let unit: Unit = crol::from_str("weapon: Some(\"stick\")").unwrap();
    assert_eq!(unit.weapon, Some(IdOr::Value("stick".into())));
    let unit: Unit = crol::from_str("weapon: None").unwrap();
    assert_eq!(unit.weapon, None);
}

#[test]
fn writes_back_what_was_read() {
    let screen: Screen = crol::from_str(SCREEN).unwrap();
    let text = crol::to_string(&screen).unwrap();
    assert!(!text.contains("IdOr"), "{text}");
    assert!(text.contains("@body"), "{text}");
    assert!(text.contains("Label {"), "{text}");
    assert_eq!(crol::from_str::<Screen>(&text).unwrap(), screen);

    // With type names off, nothing around either side.
    let pair = (IdOr::<Size>::Id(id("@web")), IdOr::Value(Size::Fill(1)));
    let text =
        crol::to_string_pretty(&pair, PrettyConfig::default().with_struct_names(false)).unwrap();
    assert_eq!(text.trim(), "(@web, Fill(1))");
}

#[test]
fn not_a_map_key() {
    let message = "`IdOr` cannot be a map key; a map takes id keys (`crol::Id`) or it does not";

    let map = HashMap::from([(IdOr::<String>::Id(id("@web")), 1u32)]);
    let error = crol::to_string(&map).unwrap_err().to_string();
    assert!(error.contains(message), "{error}");

    // An id key has to name an entry, so `@web` comes with a `web`.
    for text in ["web: 1", "web: 1\n@web: 2"] {
        let error = crol::from_str::<HashMap<IdOr<String>, u32>>(text)
            .unwrap_err()
            .to_string();
        assert!(error.contains(message), "{text}: {error}");
    }

    // `Id` keys work as before.
    let map = BTreeMap::from([(id("@web"), 1u32)]);
    let text = crol::to_string(&map).unwrap();
    assert!(text.contains("@web: 1"), "{text}");
}

#[test]
fn json_has_the_plain_form() {
    #[derive(Debug, PartialEq, Deserialize, Serialize)]
    struct Limits {
        cpu: u32,
    }
    let values = vec![IdOr::Id(id("@web")), IdOr::Value(Limits { cpu: 1 })];
    let json = serde_json::to_string(&values).unwrap();
    assert_eq!(json, r#"["@web",{"cpu":1}]"#);
    let back: Vec<IdOr<Limits>> = serde_json::from_str(&json).unwrap();
    assert_eq!(back, values);

    let enums: Vec<IdOr<Size>> = serde_json::from_str(r#"["@a.b", {"Fill": 2}]"#).unwrap();
    assert_eq!(enums, [IdOr::Id(id("@a.b")), IdOr::Value(Size::Fill(2))]);

    // A string that starts with `@` is the id: `Value("@web")` reads back
    // as one.
    let strings = vec![IdOr::Value("@web".to_owned()), IdOr::Id(id("@web"))];
    let json = serde_json::to_string(&strings).unwrap();
    assert_eq!(json, r#"["@web","@web"]"#);
    let back: Vec<IdOr<String>> = serde_json::from_str(&json).unwrap();
    assert_eq!(back, [IdOr::Id(id("@web")), IdOr::Id(id("@web"))]);

    // And an error if it is no id, rather than a string.
    for text in [r#""@ hi""#, r#""@""#] {
        let error = serde_json::from_str::<IdOr<String>>(text)
            .unwrap_err()
            .to_string();
        assert!(error.contains("is not an id"), "{text}: {error}");
    }

    // A `T` that is an `Option` reads as it does alone.
    let options: Vec<IdOr<Option<u32>>> = serde_json::from_str("[null, 3, \"@x\"]").unwrap();
    assert_eq!(
        options,
        [IdOr::Value(None), IdOr::Value(Some(3)), IdOr::Id(id("@x"))]
    );
    let options: Vec<IdOr<Option<Limits>>> = serde_json::from_str(r#"[{"cpu": 2}]"#).unwrap();
    assert_eq!(options, [IdOr::Value(Some(Limits { cpu: 2 }))]);
}

#[test]
fn from_deserializers_that_forward_a_newtype_struct() {
    type E = serde::de::value::Error;
    let read =
        |text: &str| IdOr::<String>::deserialize(serde::de::value::StrDeserializer::<E>::new(text));
    assert_eq!(read("@web").unwrap(), IdOr::Id(id("@web")));
    assert_eq!(read("web").unwrap(), IdOr::Value("web".into()));
    assert!(
        read("@ hi")
            .unwrap_err()
            .to_string()
            .contains("is not an id")
    );

    let number = IdOr::<u32>::deserialize(IntoDeserializer::<E>::into_deserializer(5u32));
    assert_eq!(number.unwrap(), IdOr::Value(5));
}

#[test]
fn under_flatten() {
    #[derive(Debug, PartialEq, Deserialize)]
    struct Limits {
        cpu: u32,
    }
    #[derive(Debug, Deserialize)]
    struct Inner<T> {
        volume: IdOr<T>,
    }
    #[derive(Debug, Deserialize)]
    struct Outer<T> {
        name: String,
        #[serde(flatten)]
        inner: Inner<T>,
    }

    let (outer, ids): (Outer<Limits>, _) =
        crol::from_str_with_ids("name: \"x\"\nvolume: @shared").unwrap();
    assert_eq!(outer.inner.volume, IdOr::Id(id("@shared")));
    assert_eq!(ids.len(), 1);

    let outer: Outer<Limits> = crol::from_str("name: \"x\"\nvolume: { cpu: 1 }").unwrap();
    assert_eq!(outer.inner.volume, IdOr::Value(Limits { cpu: 1 }));

    let outer: Outer<Vec<u32>> = crol::from_str("name: \"x\"\nvolume: [1, 2]").unwrap();
    assert_eq!(outer.inner.volume, IdOr::Value(vec![1, 2]));

    // A string that starts with `@` reads as an id through the buffer.
    let (outer, ids): (Outer<String>, _) =
        crol::from_str_with_ids("name: \"x\"\nvolume: \"@text\"").unwrap();
    assert_eq!(outer.inner.volume, IdOr::Id(id("@text")));
    assert!(ids.is_empty());
}

// ---- With a schema: checks, scopes and `refers` ----

#[allow(unreachable_pub)]
mod ui {
    use std::collections::BTreeMap;

    use crol::IdOr;
    use crol_derive::Schema;
    use serde_derive::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
    pub enum Size {
        Length(u16),
        Fill(u16),
    }

    #[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
    pub enum Node {
        Stack {
            children: Vec<(Size, IdOr<Box<Node>>)>,
        },
        Label {
            text: String,
            #[serde(default)]
            muted: bool,
        },
        Panel {
            title: String,
            child: IdOr<Box<Node>>,
        },
        TextInput {
            placeholder: String,
        },
        List {},
    }

    /// A screen is a scope for the nodes it places.
    #[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
    #[crol(scope)]
    pub struct Screen(pub BTreeMap<String, Node>);

    #[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
    pub struct Theme {
        pub accent: String,
    }

    #[derive(Debug, PartialEq, Deserialize, Serialize, Schema)]
    pub struct App {
        pub screens: BTreeMap<String, Screen>,
        #[serde(default)]
        pub themes: BTreeMap<String, Theme>,
        #[crol(refers(Theme))]
        #[serde(default)]
        pub theme: Option<IdOr<Theme>>,
    }

    /// `App`'s screens, as written in the proposal.
    pub const APP: &str = r#"
screens: {
    dashboard: {
        layout: Stack {
            children: [
                (Fill(1), @sidebar),
                (Length(1), Label { text: "q: quit", muted: true }),
            ],
        },
        +sidebar: Panel {
            title: " Workers ",
            child: Stack { children: [(Length(1), @filter), (Fill(1), @list)] },
        },
        +filter: TextInput { placeholder: "Filter workers…" },
        +list: List {},
    },
}
themes: { dark: Theme { accent: "blue" } }
theme: Some(@themes.dark)
"#;
}

fn load_app(text: &str) -> Result<crol::CrolData, crol::Error> {
    let layer = crol::CrolLayer::from_sources("base", [("ui.crod", text)]);
    crol::CrolComposer::new()
        .schema(crol::schema::build::<ui::App>())
        .load_layer(layer)
        .compose_data()
}

fn check_app(text: &str) -> Vec<String> {
    let data = load_app(text).unwrap();
    let schema = crol::schema::build::<ui::App>();
    crol::check(&data, &schema, None, &Default::default())
        .into_iter()
        .map(|d| d.message)
        .collect()
}

#[test]
fn checks_take_ids_and_values() {
    let data = load_app(ui::APP).unwrap();
    let (screens, ids): (BTreeMap<String, ui::Screen>, _) = data.get_with_ids("screens").unwrap();
    assert_eq!(screens["dashboard"].0.len(), 4);
    // Ids resolve through the screen's scope, also inside values written in
    // place.
    let targets: Vec<String> = ids
        .iter()
        .map(|used| {
            let target = used
                .target
                .as_ref()
                .map_or("nothing".into(), |t| t.to_string());
            format!("{} -> {target}", used.id)
        })
        .collect();
    assert_eq!(
        targets,
        [
            "@sidebar -> screens.dashboard.sidebar",
            "@filter -> screens.dashboard.filter",
            "@list -> screens.dashboard.list",
        ]
    );
    // And `crol::check` takes both sides: no "found an id".
    assert_eq!(check_app(ui::APP), Vec::<String>::new());

    let theme: Option<IdOr<ui::Theme>> = data.get("theme").unwrap();
    assert_eq!(theme, Some(IdOr::Id(id("@themes.dark"))));
    let text = ui::APP.replace("Some(@themes.dark)", "Some({ accent: \"red\" })");
    assert_eq!(check_app(&text), Vec::<String>::new());
}

#[test]
fn checks_see_into_values_written_in_place() {
    let misspelt = ui::APP.replace("Label { text", "Lable { text");
    assert_eq!(
        check_app(&misspelt),
        [
            "`Node` has no variant `Lable`; its variants are `Stack`, `Label`, `Panel`, \
             `TextInput`, `List`"
        ]
    );

    // `explicit_defaults` holds inside a value written in place.
    let missing = ui::APP.replace("muted: true ", "");
    assert_eq!(
        check_app(&missing),
        [
            "`Label` is missing the field `muted`; write it, or end the block with `..` to \
             leave out fields that have defaults"
        ]
    );

    let wrong = ui::APP.replace("placeholder: \"Filter workers…\"", "placeholder: 3");
    assert_eq!(check_app(&wrong), ["expected a string, found 3"]);
}

#[test]
fn checks_report_ids_as_ids() {
    let misspelt = ui::APP.replace("@filter)", "@filtr)");
    assert_eq!(
        check_app(&misspelt),
        ["the id `@filtr` names nothing: no `filtr` in `screens.dashboard` or the root"]
    );

    // `refers` on an `IdOr` field: the id has to name a `Theme`.
    let wrong = ui::APP.replace("Some(@themes.dark)", "Some(@screens.dashboard)");
    assert_eq!(
        check_app(&wrong),
        [
            "the id `@screens.dashboard` names a value without a type name, but it may only name \
          `Theme`"
        ]
    );
}

#[test]
fn an_id_is_no_value_where_only_a_value_is() {
    // A screen's own entry is a `Node`, not an `IdOr`: no alias.
    let text = ui::APP.replace("+list: List {},", "+list: @filter,");
    assert_eq!(check_app(&text), ["expected a `Node` variant, found an id"]);
}

#[test]
fn the_schema_writes_id_or() {
    let schema = crol::schema::build::<ui::App>();
    let text = schema.to_text().unwrap();
    // `IdOr(refers, ty)`, and `refers` reaches it through `Option`.
    assert!(
        text.contains(r#"IdOr([], Named("id_or::ui::Node"))"#),
        "{text}"
    );
    let theme = &schema.get("id_or::ui::App").unwrap();
    let crol::schema::TypeDef::Struct { fields, .. } = theme else {
        panic!("{theme:?}")
    };
    assert_eq!(
        fields["theme"].ty,
        TypeExpr::Option(Box::new(TypeExpr::IdOr(
            vec!["id_or::ui::Theme".into()],
            Box::new(TypeExpr::Named("id_or::ui::Theme".into())),
        )))
    );
    // And reads back.
    assert_eq!(crol::schema::SchemaFile::from_text(&text).unwrap(), schema);
}
