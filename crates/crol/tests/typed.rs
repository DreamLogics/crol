//! `#[type(...)]` on value binds (`SPEC.md` 6.1.2): what a type names, and
//! what the check of a bind's value finds.

#![allow(missing_docs, dead_code)]

use std::collections::{BTreeMap, HashMap};

use crol::CheckOptions;
use crol::schema::{self, SchemaFile, TypeExpr};
use crol::typed::{self, TypeAt, TypeUses};
use crol_derive::Schema;
use crol_eval::FileId;
use serde_derive::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Schema)]
pub struct Item {
    pub name: String,
    pub value: i32,
}

#[derive(Deserialize, Serialize, Schema)]
pub struct Limits {
    /// Cores.
    pub cpu: f64,
}

#[derive(Deserialize, Serialize, Schema)]
pub enum Shape {
    Circle { radius: f64 },
    Rect(f64, f64),
    Dot,
}

#[derive(Deserialize, Serialize, Schema)]
pub struct Pair<A, B> {
    pub first: A,
    pub second: B,
}

#[derive(Deserialize, Serialize, Schema)]
pub struct Foo {
    pub bar: HashMap<String, i32>,
    pub baz: Vec<i16>,
    pub collections: BTreeMap<String, Vec<Item>>,
    pub tuple: (i32, String, String, bool),
    /// The limits.
    pub limits: Option<Limits>,
    pub shape: Shape,
    pub pair: Pair<u32, String>,
}

fn schema() -> SchemaFile {
    schema::build::<Foo>()
}

/// What the `#[type(...)]` of the one bind in `text` names, or what is wrong.
fn resolved(text: &str) -> Result<TypeExpr, String> {
    let schema = schema();
    let document = crol_syntax::parse(text).document();
    let uses = TypeUses::of(&document, &schema);
    let bind = document
        .entries()
        .find_map(|e| match e {
            crol_syntax::ast::AnyEntry::ValueBind(b) => Some(b),
            _ => None,
        })
        .expect("a value bind");
    let type_ref = bind.attr().and_then(|a| a.type_ref()).expect("a type");
    typed::resolve(&schema, &uses, &type_ref)
        .map(|(ty, _)| ty)
        .map_err(|p| p.message)
}

/// The messages of what the check finds in `text`.
fn problems(text: &str) -> Vec<String> {
    let parse = crol_syntax::parse(text);
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());
    let mut sources = crol_eval::Sources::new();
    let file: FileId = sources.add("a.crod", text);
    typed::check_document(file, &parse.document(), &schema(), &CheckOptions::default())
        .into_iter()
        .map(|d| d.message)
        .collect()
}

fn named(path: &str) -> TypeExpr {
    TypeExpr::Named(path.to_owned())
}

fn path_of(name: &str) -> String {
    let schema = schema();
    let (path, _) = schema.by_name(name).next().expect("a type of that name");
    path.to_owned()
}

#[test]
fn paths_step_into_fields_elements_and_values() {
    let item = named(&path_of("Item"));
    assert_eq!(
        resolved("#[type(Foo.bar)] x := {}"),
        Ok(TypeExpr::Map(
            Box::new(TypeExpr::String),
            Box::new(TypeExpr::I32)
        ))
    );
    assert_eq!(resolved("#[type(Foo.baz.[])] x := 1"), Ok(TypeExpr::I16));
    assert_eq!(
        resolved("#[type(Foo.collections.[])] x := []"),
        Ok(TypeExpr::List(Box::new(item.clone())))
    );
    assert_eq!(resolved("#[type(Foo.collections.[].[])] x := {}"), Ok(item));
    assert_eq!(
        resolved("#[type(Foo.tuple.2)] x := \"\""),
        Ok(TypeExpr::String)
    );
    // Through the `Option`.
    assert_eq!(
        resolved("#[type(Foo.limits.cpu)] x := 1.0"),
        Ok(TypeExpr::F64)
    );
    assert_eq!(
        resolved("#[type(Foo.pair.first)] x := 1"),
        Ok(TypeExpr::U32)
    );
}

#[test]
fn variants_are_stepped_into() {
    assert_eq!(
        resolved("#[type(Shape::Circle.radius)] x := 1"),
        Ok(TypeExpr::F64)
    );
    assert_eq!(resolved("#[type(Shape::Rect.1)] x := 1"), Ok(TypeExpr::F64));
    assert!(
        resolved("#[type(Shape::Circle)] x := 1")
            .unwrap_err()
            .contains("is a variant, not a type")
    );
    assert!(
        resolved("#[type(Shape::Square.x)] x := 1")
            .unwrap_err()
            .contains("has no variant `Square`")
    );
}

#[test]
fn rust_names_are_the_built_in_types() {
    let item = named(&path_of("Item"));
    assert_eq!(
        resolved("#[type(HashMap<String, Vec<Item>>)] x := {}"),
        resolved("#[type(BTreeMap<String, Vec<Item>>)] x := {}")
    );
    assert_eq!(
        resolved("#[type(Option<Box<Item>>)] x := None"),
        Ok(TypeExpr::Option(Box::new(item.clone())))
    );
    assert_eq!(
        resolved("#[type([u8; 3])] x := []"),
        Ok(TypeExpr::Array(Box::new(TypeExpr::U8), 3))
    );
    assert_eq!(
        resolved("#[type((u8, String))] x := ()"),
        Ok(TypeExpr::Tuple(vec![TypeExpr::U8, TypeExpr::String]))
    );
    assert_eq!(
        resolved("#[type((u8,))] x := ()"),
        Ok(TypeExpr::Tuple(vec![TypeExpr::U8]))
    );
    assert_eq!(resolved("#[type((u8))] x := 1"), Ok(TypeExpr::U8));
    assert_eq!(resolved("#[type(())] x := ()"), Ok(TypeExpr::Unit));
    assert_eq!(
        resolved("#[type(HashSet<char>)] x := []"),
        Ok(TypeExpr::Set(Box::new(TypeExpr::Char)))
    );
    assert_eq!(
        resolved("#[type(crol::Id)] x := @a"),
        Ok(TypeExpr::Id(Vec::new()))
    );
    assert_eq!(resolved("#[type(crol::Value)] x := 1"), Ok(TypeExpr::Any));
    assert!(
        resolved("#[type(Vec<u8, u8>)] x := []")
            .unwrap_err()
            .contains("takes one type argument")
    );
}

#[test]
fn generic_schema_types_take_their_arguments() {
    let pair = path_of("Pair");
    assert_eq!(
        resolved("#[type(Pair<u32, String>)] x := {}"),
        Ok(named(&pair))
    );
    assert!(
        resolved("#[type(Pair<u8, u8>)] x := {}")
            .unwrap_err()
            .contains("no type `Pair<U8, U8>`")
    );
    // A `use` names it, with or without an alias.
    let aliased = format!(
        "#![use({} as P)]\n#[type(P)] x := {{}}",
        "app::Pair<u32, String>"
    );
    let module = pair.split("::Pair").next().unwrap();
    let aliased = aliased.replace("app", module);
    assert_eq!(resolved(&aliased), Ok(named(&pair)));
}

#[test]
fn what_names_nothing_is_reported() {
    assert!(
        resolved("#[type(Missing)] x := 1")
            .unwrap_err()
            .contains("no type `Missing`")
    );
    assert!(
        resolved("#[type(Foo.nope)] x := 1")
            .unwrap_err()
            .contains("has no field `nope`")
    );
    assert!(
        resolved("#[type(Foo.tuple.7)] x := 1")
            .unwrap_err()
            .contains("no element `.7`")
    );
    assert!(
        resolved("#[type(Foo.bar.0)] x := 1")
            .unwrap_err()
            .contains("no element `.0`")
    );
    assert!(
        resolved("#[type(Foo.limits.[])] x := 1")
            .unwrap_err()
            .contains("`.[]` goes into a list")
    );
}

#[test]
fn values_that_fit_are_accepted() {
    let text = r#"
#[type(Foo.bar)]
a := { bla: 2, <prod> { bla: 3 } }
#[type(Foo.baz.[])]
b := -2
#[type(Foo.collections.[])]
c := [{ name: "item1", value: 10 }, Item { name: "item2", value: & + 1 }]
#[type(Foo.tuple)]
d := (1, "a", "b", true)
#[type(Foo.limits)]
e := Some({ cpu: 1 })
#[type(Shape)]
f := Circle { radius: 1.5 }
#[type(Shape)]
g := Rect(1.0, 2.0)
#[type(Vec<Shape>)]
h := [Dot, &f]
#[type(Item)]
i := { name: "x", ..&c[0] }
#[type(u8)]
j := 0xff
"#;
    assert_eq!(problems(text), Vec::<String>::new());
}

#[test]
fn values_that_do_not_fit_are_reported() {
    let text = r#"
#[type(Foo.bar)]
a := { bla: "two" }
#[type(Foo.baz.[])]
b := 70000
#[type(Item)]
c := { name: "x", valeu: 1 }
#[type(Item)]
d := Limits { cpu: 1.0 }
#[type(Shape)]
e := Triangle
#[type(Foo.tuple)]
f := (1, "a")
#[type(u8)]
g := 1.5
#[type(Missing)]
h := 1
"#;
    expect_test::expect![[r#"
        [
            "expected i32, found \"two\"",
            "expected i16, found 70000",
            "`Item` has no field `valeu`; its fields are `name`, `value`",
            "expected a `Item`, found a `Limits`",
            "`Shape` has no variant `Triangle`; its variants are `Circle`, `Rect`, `Dot`",
            "expected a tuple of 4, found a tuple of 2",
            "expected u8, found 1.5",
            "the schema has no type `Missing`",
        ]
    "#]]
    .assert_debug_eq(&problems(text));
}

#[test]
fn steps_are_offered_for_completion() {
    let schema = schema();
    let at = TypeAt::Type {
        ty: named(&path_of("Foo")),
        doc: None,
    };
    let labels: Vec<String> = typed::next_steps(&schema, &at)
        .into_iter()
        .map(|o| o.label)
        .collect();
    assert_eq!(
        labels,
        [
            "bar",
            "baz",
            "collections",
            "tuple",
            "limits",
            "shape",
            "pair"
        ]
    );
    let tuple = TypeAt::Type {
        ty: TypeExpr::Tuple(vec![TypeExpr::U8, TypeExpr::Bool]),
        doc: None,
    };
    let labels: Vec<String> = typed::next_steps(&schema, &tuple)
        .into_iter()
        .map(|o| o.label)
        .collect();
    assert_eq!(labels, ["0", "1"]);
}
