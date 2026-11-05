//! The serde mapping (`SPEC.md` 10).

use std::collections::{BTreeMap, HashMap};

use serde_derive::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Deserialize, Serialize)]
struct Limits {
    cpu: f64,
    memory_mb: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            cpu: 1.0,
            memory_mb: 512,
        }
    }
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
struct Service {
    image: String,
    replicas: u16,
    port: u16,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    limits: Limits,
    depends_on: Vec<crol::Id>,
    url: Option<String>,
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
enum Definition {
    Service(Service),
    Volume { size_gb: u32 },
    Marker,
    Pair(u8, u8),
}

fn err<T: for<'de> serde::Deserialize<'de> + std::fmt::Debug>(text: &str) -> String {
    crol::from_str::<T>(text).unwrap_err().to_string()
}

#[test]
fn structs_with_templates_and_defaults() {
    let text = r#"
        #![type(Service)]
        defaults := { replicas: 1, port: 8080 },
        ..&defaults,
        image: "web:1.4",
        depends_on: [@database],
        url: "http://web:" + &self.port,
        ..
    "#;
    let (service, ids): (Service, _) = crol::from_str_with_ids(text).unwrap();
    assert_eq!(
        service,
        Service {
            image: "web:1.4".into(),
            replicas: 1,
            port: 8080,
            tags: vec![],
            limits: Limits::default(),
            depends_on: vec!["database".into()],
            url: Some("http://web:8080".into()),
        }
    );
    assert_eq!(ids.len(), 1);
    assert_eq!(ids[0].id, "database");
    assert_eq!(ids[0].rust_path, "depends_on[0]");
}

#[test]
fn completeness_needs_every_field_or_defaults() {
    assert_eq!(
        err::<Limits>("Limits { cpu: 1.0 }"),
        "`Limits` is missing the field `memory_mb`; write it, or end the block with `..` to leave out fields that have defaults (<string>:1:1)"
    );
    // With `..`, serde decides: `Limits` has no default for `memory_mb`.
    assert_eq!(
        err::<Limits>("Limits { cpu: 1.0, .. }"),
        "missing field `memory_mb` (<string>:1:1)"
    );
    // A removed field takes its default even without `..`.
    #[derive(Debug, Deserialize)]
    struct WithDefault {
        #[serde(default)]
        a: u8,
        b: u8,
    }
    let value: WithDefault = crol::from_str("{ a: 1, b: 2, -a }").unwrap();
    assert_eq!((value.a, value.b), (0, 2));
}

#[test]
fn type_names_and_unknown_fields_are_checked() {
    assert_eq!(
        err::<Limits>("Service { cpu: 1.0, memory_mb: 1 }"),
        "expected a `Limits`, found a `Service` (<string>:1:1)"
    );
    assert_eq!(
        err::<Limits>("{ cpu: 1.0, memory_mb: 1, gpu: 2 }"),
        "`Limits` has no field `gpu`; its fields are `cpu`, `memory_mb` (<string>:1:32, while deserializing gpu)"
    );
}

#[test]
fn errors_point_at_the_value() {
    assert_eq!(
        err::<BTreeMap<String, Limits>>("web: Limits { cpu: 1.0, memory_mb: -1 },"),
        "expected u32, found -1 (<string>:1:36, while deserializing web.memory_mb)"
    );
    assert_eq!(
        err::<Limits>("{ cpu: \"fast\", memory_mb: 1 }"),
        "expected a float, found \"fast\" (<string>:1:8, while deserializing cpu)"
    );
}

#[test]
fn evaluation_errors_list_every_diagnostic() {
    let message = err::<BTreeMap<String, u8>>("a: 1 / 0, b: [1] + 2");
    assert_eq!(
        message,
        "<string>:1:4: /: division by zero (while evaluating a)\n<string>:1:14: +: cannot add an integer to a list (while evaluating b)"
    );
}

#[test]
fn enums() {
    let text = r#"
        web: Service { image: "w", replicas: 2, port: 80, depends_on: [], url: None, .. },
        data: Volume { size_gb: 10 },
        m: Marker,
        p: Pair(1, 2),
    "#;
    let all: BTreeMap<String, Definition> = crol::from_str(text).unwrap();
    assert!(matches!(&all["web"], Definition::Service(s) if s.replicas == 2));
    assert_eq!(all["data"], Definition::Volume { size_gb: 10 });
    assert_eq!(all["m"], Definition::Marker);
    assert_eq!(all["p"], Definition::Pair(1, 2));
}

#[test]
fn options_and_implicit_some() {
    #[derive(Debug, PartialEq, Deserialize)]
    struct O {
        a: Option<u8>,
        b: Option<u8>,
        c: Option<u8>,
    }
    let o: O = crol::from_str("{ a: Some(1), b: None, c: 3 }").unwrap();
    assert_eq!(
        o,
        O {
            a: Some(1),
            b: None,
            c: Some(3)
        }
    );
}

#[test]
fn ids_need_the_at_sign() {
    assert_eq!(
        err::<Vec<crol::Id>>(r#"["web"]"#),
        "expected an id (`@name`), found \"web\" (<string>:1:2, while deserializing [0])"
    );
    // A `String` accepts an id too, and a map keyed by ids takes string keys.
    let names: Vec<String> = crol::from_str("[@web, \"db\"]").unwrap();
    assert_eq!(names, ["web", "db"]);
    let (map, ids): (HashMap<crol::Id, u32>, _) = crol::from_str_with_ids("{ web: 1 }").unwrap();
    assert_eq!(map[&crol::Id::from("web")], 1);
    assert_eq!(ids[0].id, "web");
}

#[test]
fn maps_with_integer_and_enum_keys() {
    #[derive(Debug, PartialEq, Eq, Hash, Deserialize)]
    enum Color {
        Red,
        Green,
    }
    let by_number: BTreeMap<i32, String> = crol::from_str(r#"{ 1: "a", [-3]: "b" }"#).unwrap();
    assert_eq!(by_number[&-3], "b");
    let by_color: HashMap<Color, u8> = crol::from_str("{ Red: 1, Green: 2 }").unwrap();
    assert_eq!(by_color[&Color::Green], 2);
}

#[test]
fn tuples_newtypes_and_units() {
    #[derive(Debug, PartialEq, Deserialize)]
    struct Meters(f64);
    #[derive(Debug, PartialEq, Deserialize)]
    struct Rgb(u8, u8, u8);
    #[derive(Debug, PartialEq, Deserialize)]
    struct Unit;
    let value: (Meters, Meters, Rgb, Rgb, Unit, (), Vec<u8>) =
        crol::from_str("(Meters(1.5), 2, Rgb(1, 2, 3), (4, 5, 6), Unit, (), [7])").unwrap();
    assert_eq!(value.0, Meters(1.5));
    assert_eq!(value.1, Meters(2.0));
    assert_eq!(value.3, Rgb(4, 5, 6));
}

#[test]
fn any_gives_the_natural_form() {
    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(untagged)]
    enum Any {
        Int(i64),
        Str(String),
        List(Vec<Any>),
        Map(BTreeMap<String, Any>),
    }
    let value: Any = crol::from_str(r#"{ a: [1, "x"], b: Some(2), c: @web }"#).unwrap();
    let Any::Map(map) = value else { panic!() };
    assert_eq!(map["a"], Any::List(vec![Any::Int(1), Any::Str("x".into())]));
    assert_eq!(
        map["b"],
        Any::Map(BTreeMap::from([("Some".into(), Any::Int(2))]))
    );
    assert_eq!(map["c"], Any::Str("web".into()));
}

#[test]
fn serializer_writes_plain_data() {
    let service = Service {
        image: "web:1.4".into(),
        replicas: 2,
        port: 80,
        tags: vec!["a b".into()],
        limits: Limits::default(),
        depends_on: vec!["database".into(), "odd id".into()],
        url: None,
    };
    let text =
        crol::to_string_pretty(&service, crol::PrettyConfig::default().with_max_width(60)).unwrap();
    expect_test::expect![[r#"
        Service {
            image: "web:1.4",
            replicas: 2,
            port: 80,
            tags: ["a b"],
            limits: Limits { cpu: 1.0, memory_mb: 512 },
            depends_on: [@database, @"odd id"],
            url: None,
        }
    "#]]
    .assert_eq(&text);
    assert_eq!(crol::from_str::<Service>(&text).unwrap(), service);

    let mut map = BTreeMap::new();
    map.insert("cache url".to_owned(), 1);
    map.insert("port".to_owned(), 2);
    assert_eq!(
        crol::to_string(&map).unwrap(),
        r#"{"cache url": 1, port: 2}"#
    );
    assert_eq!(
        crol::to_string(&Definition::Volume { size_gb: 3 }).unwrap(),
        "Volume {size_gb: 3}"
    );
    assert!(crol::to_string(&f64::NAN).is_err());
}

#[test]
fn skipped_fields_get_defaults_marker() {
    #[derive(Debug, PartialEq, Deserialize, Serialize)]
    struct S {
        a: u8,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        b: Option<u8>,
    }
    let text = crol::to_string(&S { a: 1, b: None }).unwrap();
    assert_eq!(text, "S {a: 1, ..}");
    assert_eq!(crol::from_str::<S>(&text).unwrap(), S { a: 1, b: None });
}

mod round_trip {
    use super::*;
    use proptest::prelude::*;

    #[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
    enum Shape {
        Circle(f64),
        Rect { w: i64, h: i64 },
        Empty,
        Tagged(String, char),
    }

    #[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
    struct Model {
        name: String,
        count: u64,
        signed: i128,
        ratio: f64,
        flag: bool,
        letter: char,
        maybe: Option<Option<u8>>,
        list: Vec<Shape>,
        map: BTreeMap<String, i32>,
        numbered: BTreeMap<i16, String>,
        pair: (u8, String),
    }

    fn shape() -> impl Strategy<Value = Shape> {
        prop_oneof![
            any::<f64>()
                .prop_filter("finite", |f| f.is_finite())
                .prop_map(Shape::Circle),
            (any::<i64>(), any::<i64>()).prop_map(|(w, h)| Shape::Rect { w, h }),
            Just(Shape::Empty),
            (any::<String>(), any::<char>()).prop_map(|(s, c)| Shape::Tagged(s, c)),
        ]
    }

    prop_compose! {
        fn model()(
            name in any::<String>(),
            count in any::<u64>(),
            signed in any::<i128>(),
            ratio in any::<f64>().prop_filter("finite", |f| f.is_finite()),
            flag in any::<bool>(),
            letter in any::<char>(),
            maybe in any::<Option<Option<u8>>>(),
            list in proptest::collection::vec(shape(), 0..4),
            map in proptest::collection::btree_map(any::<String>(), any::<i32>(), 0..4),
            numbered in proptest::collection::btree_map(any::<i16>(), any::<String>(), 0..4),
            pair in (any::<u8>(), any::<String>()),
        ) -> Model {
            Model { name, count, signed, ratio, flag, letter, maybe, list, map, numbered, pair }
        }
    }

    proptest! {
        #[test]
        fn to_string_then_from_str_gives_the_value_back(model in model()) {
            let text = crol::to_string(&model).unwrap();
            let back: Model = crol::from_str(&text).map_err(|e| TestCaseError::fail(format!("{e}\n{text}")))?;
            prop_assert_eq!(&back, &model);
            let pretty = crol::to_string_pretty(&model, crol::PrettyConfig::default()).unwrap();
            let back: Model = crol::from_str(&pretty).map_err(|e| TestCaseError::fail(format!("{e}\n{pretty}")))?;
            prop_assert_eq!(back, model);
        }
    }
}

#[test]
fn an_f32_is_written_short_and_reads_back_the_same() {
    assert_eq!(crol::to_string(&0.1f32).unwrap(), "0.1");
    assert_eq!(crol::to_string(&1.5f32).unwrap(), "1.5");
    assert_eq!(crol::to_string(&16_777_216.0f32).unwrap(), "16777216.0");
    // Across the exponents, with bit patterns that step through the mantissas.
    let mut bits = 1u32;
    while bits < 0x7f80_0000 {
        let value = f32::from_bits(bits);
        let text = crol::to_string(&value).unwrap();
        let back: f32 = crol::from_str(&text).unwrap();
        assert_eq!(back.to_bits(), bits, "{value} as {text}");
        bits += 0x0000_1fff;
    }
}
