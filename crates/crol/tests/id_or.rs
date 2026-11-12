//! `crol::IdOr` (`SPEC.md` 10.5, 10.8): an id, or a value written in place.

#![allow(missing_docs, dead_code)]

use std::collections::{BTreeMap, HashMap};

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
    Stack { children: Vec<(Size, IdOr<Box<Node>>)> },
    Label {
        text: String,
        #[serde(default)]
        muted: bool,
    },
    Panel { title: String, child: IdOr<Box<Node>> },
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
    let read = |text: &str| {
        IdOr::<String>::deserialize(serde::de::value::StrDeserializer::<E>::new(text))
    };
    assert_eq!(read("@web").unwrap(), IdOr::Id(id("@web")));
    assert_eq!(read("web").unwrap(), IdOr::Value("web".into()));
    assert!(read("@ hi").unwrap_err().to_string().contains("is not an id"));

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
