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

// ---- Where typed templates are used ----

/// The warnings about where the templates of `text` are used, with `root`
/// as the place everything is found from.
fn use_warnings(schema: &SchemaFile, text: &str, root: typed::TypeRoot<'_>) -> Vec<String> {
    let parse = crol_syntax::parse(text);
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());
    let mut sources = crol_eval::Sources::new();
    let file = sources.add("a.crod", text);
    let document = parse.document();
    let templates = typed::typed_templates([(file, &document)], schema);
    typed::check_template_uses(file, &document, schema, &templates, root)
        .into_iter()
        .map(|d| d.message)
        .collect()
}

#[test]
fn templates_used_where_another_type_goes_are_warned_about() {
    let schema = schema();
    let text = r#"
#[type(Limits)]
l := { cpu: 1.0 }
#[type(Item)]
item := { name: "a" }
#[type(String)]
s := "s"
#[type(f64)]
r := 1.0

limits: &l
bar: &l
collections: { a: [&item, &l] }
tuple: (1, &s, "b", true)
shape: Circle { radius: &r }
pair: { ..&l, first: 1 }
baz: [&s + 1]

#[type(Foo)]
f := { limits: & ~ &l, bar: &l }
"#;
    let root = typed::TypeRoot::Data {
        root: None,
        value: None,
    };
    expect_test::expect![[r#"
        [
            "the template `l` has the type `Limits`, but it is used where `Map<String, i32>` goes",
            "the template `l` has the type `Limits`, but it is used where `Item` goes",
            "the template `l` has the type `Limits`, but it is used where `Pair` goes",
            "the template `l` has the type `Limits`, but it is used where `Map<String, i32>` goes",
        ]
    "#]]
    .assert_debug_eq(&use_warnings(&schema, text, root));
}

#[derive(Deserialize, Serialize, Schema)]
pub enum Definition {
    Item(Item),
    Limits(Limits),
}

#[test]
fn the_variant_of_a_root_value_comes_from_its_evaluated_value() {
    let schema = schema::build::<Definition>();
    let text = "#[type(Limits)]\nl := { cpu: 1.0 }\n\n+a: Item { name: \"a\", value: 1 }\n+b: Limits { cpu: 2.0 }\na: & ~ &l\nb: & ~ &l\n";
    let mut sources = crol_eval::Sources::new();
    let file = sources.add("a.crod", text);
    let layers = [crol_eval::Layer {
        name: "base".to_owned(),
        documents: vec![("a.crod".to_owned(), file)],
    }];
    let evaluation = crol_eval::evaluate_data(&sources, &layers, &Default::default());
    let value = evaluation.value.as_ref().expect("it evaluates");
    let root = typed::TypeRoot::Data {
        root: None,
        value: Some(value),
    };
    assert_eq!(
        use_warnings(&schema, text, root),
        ["the template `l` has the type `Limits`, but it is used where `Item` goes"]
    );
    // Without the evaluation, the variant is not known, and nothing is said.
    let root = typed::TypeRoot::Data {
        root: None,
        value: None,
    };
    assert_eq!(use_warnings(&schema, text, root), Vec::<String>::new());
}

/// A newtype around a map: its name fits a block that is one.
#[derive(Deserialize, Serialize, Schema)]
pub struct Counts(BTreeMap<String, u8>);

/// A transparent struct around a struct.
#[derive(Deserialize, Serialize, Schema)]
#[serde(transparent)]
pub struct Budget(Limits);

#[derive(Deserialize, Serialize, Schema)]
pub struct Wrapped {
    pub counts: Counts,
    pub budget: Budget,
    pub plain: BTreeMap<String, u8>,
}

/// Type names through newtypes and transparent structs (`SPEC.md` 10.2).
#[test]
fn a_block_has_the_names_of_what_wraps_it() {
    let schema = schema::build::<Wrapped>();
    let problems = |text: &str| -> Vec<String> {
        let parse = crol_syntax::parse(text);
        assert!(parse.errors().is_empty(), "{:?}", parse.errors());
        let mut sources = crol_eval::Sources::new();
        let file: FileId = sources.add("a.crod", text);
        typed::check_document(file, &parse.document(), &schema, &CheckOptions::default())
            .into_iter()
            .map(|d| d.message)
            .collect()
    };
    let none = Vec::<String>::new();
    assert_eq!(problems("#[type(Counts)] x := Counts { a: 1 }"), none);
    assert_eq!(problems("#[type(Counts)] x := { a: 1 }"), none);
    assert_eq!(
        problems("#[type(Counts)] x := Count { a: 1 }"),
        ["`Count { ... }` is not a `Counts`"]
    );
    assert_eq!(
        problems("#[type(Counts)] x := Counts { a: -1 }"),
        ["expected u8, found -1"]
    );
    assert_eq!(problems("#[type(Budget)] x := Budget { cpu: 1.0 }"), none);
    assert_eq!(problems("#[type(Budget)] x := Limits { cpu: 1.0 }"), none);
    assert_eq!(
        problems("#[type(Budget)] x := Budgte { cpu: 1.0 }"),
        ["expected a `Limits`, found a `Budgte`"]
    );
    // A name on a map that nothing names is a warning where the value is
    // used, and its entries are checked all the same.
    assert_eq!(problems("#[type(Wrapped.plain)] x := Plain { a: 1 }"), none);
    assert_eq!(
        problems("#[type(Wrapped.plain)] x := Plain { a: -1 }"),
        ["expected u8, found -1"]
    );
}

#[test]
fn id_or_takes_an_id_or_a_value() {
    let limits = named(&path_of("Limits"));
    assert_eq!(
        resolved("#[type(crol::IdOr<Limits>)] x := @a"),
        Ok(TypeExpr::IdOr(Vec::new(), Box::new(limits)))
    );
    assert!(
        resolved("#[type(crol::IdOr)] x := @a")
            .unwrap_err()
            .contains("takes one type argument")
    );
    // An id fits, and so does a value that fits the type.
    let none = Vec::<String>::new();
    assert_eq!(problems("#[type(crol::IdOr<Limits>)] x := @a"), none);
    assert_eq!(
        problems("#[type(crol::IdOr<Limits>)] x := Limits { cpu: 1.0 }"),
        none
    );
    // Anything else is checked as the value.
    assert_eq!(
        problems("#[type(crol::IdOr<Limits>)] x := Limits { cpu: \"x\" }"),
        ["expected a float, found \"x\""]
    );
    assert_eq!(
        problems("#[type(crol::IdOr<u8>)] x := \"x\""),
        ["expected u8, found \"x\""]
    );
}
