//! Schemas: descriptions of the Rust types data loads into, for tools and
//! for `&default` (`SPEC.md` 10.9, 10.10).
//!
//! A schema is built from the types themselves, through the [`Schema`]
//! trait, which `#[derive(crol::Schema)]` implements. It is plain data, and
//! a schema file is a value-mode CROL document of the type [`SchemaFile`].
//!
//! The usual way to write the schema file is a test, which `cargo test` and
//! CI run anyway; it fails if the file is out of date:
//!
//! ```no_run
//! # enum Definition {}
//! # impl crol::schema::Schema for Definition {
//! #     fn describe(_: &mut crol::schema::Registry) -> crol::schema::TypeRef { crol::schema::TypeRef::Any }
//! # }
//! // In a test: `#[test] fn crol_schema() { ... }`
//! crol::schema::check_file::<Definition>("crol/schema.crol").unwrap();
//! ```

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::mode::Mode;
use crate::raw::RawValue;

/// The format version of schema files (`SPEC.md` 10.10).
pub const FORMAT: u32 = 1;

/// The path of the type of schema files, in their own schema
/// ([`SchemaFile`]'s [`Schema`] implementation).
pub const SCHEMA_PATH: &str = "crol::schema::Schema";

/// A type that is part of a schema (`SPEC.md` 10.9). Derive it with
/// `#[derive(crol::Schema)]`; `crol` implements it for the scalars, strings,
/// collections, tuples, arrays and its own types.
pub trait Schema {
    /// Describes the type: registers the named types it uses, and returns
    /// its type expression.
    fn describe(registry: &mut Registry) -> TypeRef;
}

/// A type expression (`SPEC.md` 10.10).
pub type TypeRef = TypeExpr;

/// The named types of a schema being built.
#[derive(Debug, Default)]
pub struct Registry {
    types: BTreeMap<String, TypeDef>,
    in_progress: HashSet<String>,
    constraints: BTreeMap<String, ConstraintDef>,
}

impl Registry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers the named type `path`, defined by `define` unless it is
    /// already registered (or being defined, for recursive types), and
    /// returns a reference to it.
    pub fn add(&mut self, path: &str, define: impl FnOnce(&mut Registry) -> TypeDef) -> TypeRef {
        if !self.types.contains_key(path) && self.in_progress.insert(path.to_owned()) {
            let definition = define(self);
            self.in_progress.remove(path);
            self.types.insert(path.to_owned(), definition);
        }
        TypeExpr::Named(path.to_owned())
    }

    /// Defines a constraint (`#[crol(constraints(...))]`). A sealed one
    /// cannot be changed by a `schema.crod`.
    pub fn constraint(&mut self, name: &str, constraint: Constraint, sealed: bool) {
        self.constraints
            .insert(name.to_owned(), ConstraintDef { constraint, sealed });
    }

    /// Describes `T`.
    pub fn describe<T: Schema + ?Sized>(&mut self) -> TypeRef {
        T::describe(self)
    }
}

/// A schema file: the Rust types data loads into, as tools see them. It is
/// generated from those types, so change them rather than this file.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename = "Schema")]
pub struct SchemaFile {
    /// The format version of this file. It changes only when the format of
    /// schema files does.
    pub format: u32,
    /// The path of the root type: the type the schema was built for. In data,
    /// a struct or map is the whole document, and any other type that of
    /// each root value (`SPEC.md` 4.1).
    pub root: String,
    /// Every named type the root type uses, by path.
    pub types: BTreeMap<String, TypeDef>,
    /// The constraints the program defines, by name (`SPEC.md` 10.12).
    #[serde(default)]
    pub constraints: BTreeMap<String, ConstraintDef>,
}

/// By hand, so that a schema without constraints is written as it always
/// was: a field left out would give it `..`.
impl Serialize for SchemaFile {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct as _;
        let fields = if self.constraints.is_empty() { 3 } else { 4 };
        let mut out = serializer.serialize_struct("Schema", fields)?;
        out.serialize_field("format", &self.format)?;
        out.serialize_field("root", &self.root)?;
        out.serialize_field("types", &self.types)?;
        if !self.constraints.is_empty() {
            out.serialize_field("constraints", &self.constraints)?;
        }
        out.end()
    }
}

/// What a field's value must be (`SPEC.md` 10.12).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Constraint {
    /// A number not below this.
    Min(f64),
    /// A number not above this.
    Max(f64),
    /// A number from the first to the second, inclusive.
    Range(f64, f64),
    /// One of these values. A string, an id and a unit name count as their
    /// text.
    OneOf(Vec<RawValue>),
    /// The id of a root value of one of these types.
    Refers(Vec<String>),
}

impl Constraint {
    /// `OneOf` these values, for `#[crol(constraints(...))]`.
    pub fn one_of<T: Serialize>(values: impl IntoIterator<Item = T>) -> Self {
        let values = values
            .into_iter()
            .filter_map(|value| crate::to_value(&value).ok())
            .map(RawValue)
            .collect();
        Constraint::OneOf(values)
    }
}

/// A constraint in a schema, under its name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "")]
pub struct ConstraintDef {
    /// What it says.
    pub constraint: Constraint,
    /// Whether a `schema.crod` may not change it: one the program defines
    /// is, unless it is marked open (`SPEC.md` 10.12).
    #[serde(default, skip_serializing_if = "is_false")]
    pub sealed: bool,
}

/// What a root type is to a data-mode document (`SPEC.md` 4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataRoot {
    /// A struct or a map: it describes the whole document, and a root key
    /// is a field or a key of it. Completeness is that of the composed root.
    Document,
    /// Anything else, such as an enum: it is the type of each root value.
    EachValue,
}

/// How many wrappers are followed to find what a type is; a type that wraps
/// itself stops here.
const MAX_WRAPPERS: usize = 64;

/// The definition of a named type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TypeDef {
    /// A struct with named fields.
    Struct {
        /// The name data writes: the serde name.
        name: String,
        /// The doc comment, which editors show.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        doc: String,
        /// Where it is defined: the file, relative to the Cargo workspace root, and
        /// the line.
        source: (String, u32),
        /// The fields, by the name data writes them with.
        fields: IndexMap<String, Field>,
        /// `#[serde(deny_unknown_fields)]`: whether a field it does not have is
        /// an error even where unknown fields are allowed.
        #[serde(default, skip_serializing_if = "is_false")]
        deny_unknown_fields: bool,
        /// The struct's recorded default value, with `#[crol(defaults)]` and
        /// `#[serde(default)]` on the struct.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<RawValue>,
        /// `#[crol(explicit_defaults)]`: whether fields take their defaults
        /// only with `..` (`SPEC.md` 10.3). `None`: `require_complete` says.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        explicit_defaults: Option<bool>,
        /// `#[crol(scope)]`: whether its values are scopes for the ids in
        /// them (`SPEC.md` 10.9).
        #[serde(default, skip_serializing_if = "is_false")]
        scope: bool,
    },
    /// A tuple struct: `struct P(A, B)`.
    TupleStruct {
        /// The name data writes: the serde name.
        name: String,
        /// The doc comment, which editors show.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        doc: String,
        /// Where it is defined: the file, relative to the Cargo workspace root, and
        /// the line.
        source: (String, u32),
        /// The type of each element.
        fields: Vec<TypeExpr>,
    },
    /// A newtype struct, `struct N(A)`: data writes `N(x)` or `x`.
    Newtype {
        /// The name data writes: the serde name.
        name: String,
        /// The doc comment, which editors show.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        doc: String,
        /// Where it is defined: the file, relative to the Cargo workspace root, and
        /// the line.
        source: (String, u32),
        /// The wrapped type.
        inner: TypeExpr,
        /// The constraint on the value, wherever the type is used.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        constraint: Option<String>,
    },
    /// A struct with `#[serde(transparent)]`: data writes only `x`.
    Transparent {
        /// The name data writes: the serde name.
        name: String,
        /// The doc comment, which editors show.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        doc: String,
        /// Where it is defined: the file, relative to the Cargo workspace root, and
        /// the line.
        source: (String, u32),
        /// The wrapped type.
        inner: TypeExpr,
        /// The constraint on the value, wherever the type is used.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        constraint: Option<String>,
    },
    /// A unit struct: `struct U;`.
    UnitStruct {
        /// The name data writes: the serde name.
        name: String,
        /// The doc comment, which editors show.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        doc: String,
        /// Where it is defined: the file, relative to the Cargo workspace root, and
        /// the line.
        source: (String, u32),
    },
    /// An enum.
    Enum {
        /// The name data writes: the serde name.
        name: String,
        /// The doc comment, which editors show.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        doc: String,
        /// Where it is defined: the file, relative to the Cargo workspace root, and
        /// the line.
        source: (String, u32),
        /// How serde represents it; `External` if left out.
        #[serde(default, skip_serializing_if = "Repr::is_external")]
        repr: Repr,
        /// The variants, by the name data writes them with.
        variants: IndexMap<String, VariantDef>,
    },
}

impl TypeDef {
    /// The serde name.
    pub fn name(&self) -> &str {
        match self {
            TypeDef::Struct { name, .. }
            | TypeDef::TupleStruct { name, .. }
            | TypeDef::Newtype { name, .. }
            | TypeDef::Transparent { name, .. }
            | TypeDef::UnitStruct { name, .. }
            | TypeDef::Enum { name, .. } => name,
        }
    }

    /// The doc comment.
    pub fn doc(&self) -> &str {
        match self {
            TypeDef::Struct { doc, .. }
            | TypeDef::TupleStruct { doc, .. }
            | TypeDef::Newtype { doc, .. }
            | TypeDef::Transparent { doc, .. }
            | TypeDef::UnitStruct { doc, .. }
            | TypeDef::Enum { doc, .. } => doc,
        }
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// How serde represents an enum.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Repr {
    /// The default: data writes `V`, `V(x)` or `V { ... }`.
    #[default]
    External,
    /// `#[serde(tag = "...")]`: the variant is a field of the block.
    Internal {
        /// The name of the field that says the variant.
        tag: String,
    },
    /// `#[serde(tag = "...", content = "...")]`: the variant and its value are
    /// two fields of the block.
    Adjacent {
        /// The name of the field that says the variant.
        tag: String,
        /// The name of the field that holds its value.
        content: String,
    },
    /// `#[serde(untagged)]`: only the shape of the value says the variant.
    Untagged,
}

impl Repr {
    fn is_external(&self) -> bool {
        *self == Repr::External
    }
}

/// A variant of an enum.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "")]
pub struct VariantDef {
    /// What the variant holds.
    pub shape: VariantShape,
    /// The doc comment, which editors show.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub doc: String,
    /// Other names data may write it by: `#[serde(alias = "...")]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// For a struct variant, `#[crol(explicit_defaults)]`, as on a struct.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explicit_defaults: Option<bool>,
    /// For a struct variant, `#[crol(scope)]`, as on a struct.
    #[serde(default, skip_serializing_if = "is_false")]
    pub scope: bool,
}

/// What a variant holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum VariantShape {
    /// No value: data writes `V`.
    Unit,
    /// One value of this type: data writes `V(x)`.
    Newtype(TypeExpr),
    /// Values of these types: data writes `V(a, b)`.
    Tuple(Vec<TypeExpr>),
    /// Named fields: data writes `V { ... }`.
    Struct(IndexMap<String, Field>),
}

/// A field of a struct or struct variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "")]
pub struct Field {
    /// The type of the field.
    pub ty: TypeExpr,
    /// The doc comment, which editors show.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub doc: String,
    /// Other names data may write it by: `#[serde(alias = "...")]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Its default, for when `..` leaves it out.
    #[serde(default, skip_serializing_if = "FieldDefault::is_required")]
    pub default: FieldDefault,
    /// `#[serde(flatten)]`: its fields are written in the block around it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub flatten: bool,
    /// For an id field, the root types an id may name (`#[crol(refers(...))]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refers: Vec<String>,
    /// The constraint on its value, or on what a collection holds
    /// (`#[crol(constraint = "...")]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constraint: Option<String>,
    /// The constraint on a map's keys (`#[crol(key_constraint = "...")]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_constraint: Option<String>,
    /// `#[crol(sealed)]`: only the layer that defines a value can set this
    /// field (`SPEC.md` 10.13).
    #[serde(default, skip_serializing_if = "is_false")]
    pub sealed: bool,
}

impl Field {
    /// A field of type `ty`, required, with no doc.
    pub fn new(ty: TypeExpr) -> Self {
        Self {
            ty,
            doc: String::new(),
            aliases: Vec::new(),
            default: FieldDefault::Required,
            flatten: false,
            refers: Vec::new(),
            constraint: None,
            key_constraint: None,
            sealed: false,
        }
    }
}

/// The default of a field (`SPEC.md` 10.10).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum FieldDefault {
    /// No default: the field must be written, even with `..`.
    #[default]
    Required,
    /// A default whose value is not recorded.
    Unrecorded,
    /// The default value.
    Value(RawValue),
}

impl FieldDefault {
    fn is_required(&self) -> bool {
        *self == FieldDefault::Required
    }
}

/// A type expression (`SPEC.md` 10.10).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TypeExpr {
    /// `bool`.
    Bool,
    /// `char`.
    Char,
    /// `String`, `str` or `PathBuf`.
    String,
    /// `()`.
    Unit,
    /// `i8`.
    I8,
    /// `i16`.
    I16,
    /// `i32`.
    I32,
    /// `i64` or `isize`.
    I64,
    /// `i128`.
    I128,
    /// `u8`.
    U8,
    /// `u16`.
    U16,
    /// `u32`.
    U32,
    /// `u64` or `usize`.
    U64,
    /// `u128`.
    U128,
    /// `f32`.
    F32,
    /// `f64`.
    F64,
    /// `serde_bytes` and the like.
    Bytes,
    /// `crol::Id`, with the types it may refer to (`[]`: any root key).
    Id(Vec<String>),
    /// `Option<T>`.
    Option(Box<TypeExpr>),
    /// `Vec<T>`, `VecDeque<T>` or a slice.
    List(Box<TypeExpr>),
    /// `HashSet<T>` or `BTreeSet<T>`: its elements are unique.
    Set(Box<TypeExpr>),
    /// `[T; n]`: the type of its elements, and how many there are.
    Array(Box<TypeExpr>, usize),
    /// A tuple, with the type of each element.
    Tuple(Vec<TypeExpr>),
    /// `HashMap<K, V>`, `BTreeMap<K, V>` or `IndexMap<K, V>`: the type of the
    /// keys, and of the values.
    Map(Box<TypeExpr>, Box<TypeExpr>),
    /// A type in the schema's `types`, by its path.
    Named(String),
    /// Anything; not checked.
    Any,
}

impl SchemaFile {
    /// The type definition at `path`.
    pub fn get(&self, path: &str) -> Option<&TypeDef> {
        self.types.get(path)
    }

    /// The types whose serde name is `name`.
    pub fn by_name<'a>(
        &'a self,
        name: &'a str,
    ) -> impl Iterator<Item = (&'a str, &'a TypeDef)> + 'a {
        self.types
            .iter()
            .filter(move |(_, def)| def.name() == name)
            .map(|(path, def)| (path.as_str(), def))
    }

    /// The schema path a type name in a project file stands for: a schema
    /// path, or a plain name that one type has (`SPEC.md` 12.4).
    pub fn type_path<'a>(&'a self, name: &'a str) -> Option<&'a str> {
        if self.get(name).is_some() {
            return Some(name);
        }
        let mut found = self.by_name(name).map(|(path, _)| path);
        found.next().filter(|_| found.next().is_none())
    }

    /// Where `&default` finds its values in this schema (`SPEC.md` 6.1.3),
    /// for tools that evaluate with
    /// [`DataOptions::with_defaults`](crol_eval::DataOptions::with_defaults).
    /// [`CrolComposer::schema`](crate::CrolComposer::schema) does this for an
    /// application.
    pub fn defaults(&self) -> crol_eval::Defaults {
        self.defaults_for(None)
    }

    /// [`SchemaFile::defaults`] with the type at `root` as the root type
    /// (`None`: the schema's), as a project's `root` chooses (`SPEC.md` 12.4).
    pub fn defaults_for(&self, root: Option<&str>) -> crol_eval::Defaults {
        crol_eval::Defaults(std::sync::Arc::new(SchemaDefaults {
            schema: self.clone(),
            root: root.map(str::to_owned),
        }))
    }

    /// What the type at `root` (`None`: the schema's root type) is to a
    /// data-mode document (`SPEC.md` 4.1). This is the one place that says
    /// so: the checks, `fill_defaults`, `&default` and the language server
    /// all ask here.
    pub fn data_root(&self, root: Option<&str>) -> DataRoot {
        let path = root.unwrap_or(&self.root);
        if self.has_keys(&TypeExpr::Named(path.to_owned()), 0) {
            DataRoot::Document
        } else {
            DataRoot::EachValue
        }
    }

    /// Whether a value of `ty` is a keyed value of its own: a struct or a
    /// map, through what deserializes as what it wraps. An enum and what the
    /// schema does not describe are not.
    fn has_keys(&self, ty: &TypeExpr, depth: usize) -> bool {
        if depth > MAX_WRAPPERS {
            return false;
        }
        match ty {
            TypeExpr::Map(..) => true,
            TypeExpr::Option(inner) => self.has_keys(inner, depth + 1),
            TypeExpr::Named(path) => match self.get(path) {
                Some(TypeDef::Struct { .. }) => true,
                Some(TypeDef::Newtype { inner, .. } | TypeDef::Transparent { inner, .. }) => {
                    self.has_keys(inner, depth + 1)
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// Reads a schema file. It is a value document, so a `.crod` file is an
    /// error (`SPEC.md` 4).
    pub fn read(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref();
        Mode::Value.check_path(path)?;
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::new(format!("cannot read {}: {e}", path.display())))?;
        let name = path.display().to_string();
        Self::from_named_text(&name, &text).map_err(|e| e.in_file(&name))
    }

    /// Parses a schema file's text.
    pub fn from_text(text: &str) -> Result<Self, Error> {
        Self::from_named_text("<string>", text)
    }

    fn from_named_text(name: &str, text: &str) -> Result<Self, Error> {
        // A schema without constraints leaves them out, with no `..`.
        let options = crate::de::Options {
            require_complete: false,
            ..Default::default()
        };
        let (schema, _): (SchemaFile, _) = crate::from_text(name, text, options)?;
        if schema.format != FORMAT {
            return Err(Error::new(format!(
                "the schema has format {}, and this version of crol reads format {FORMAT}",
                schema.format
            )));
        }
        Ok(schema)
    }

    /// The schema as the text of a schema file.
    pub fn to_text(&self) -> Result<String, Error> {
        let body = crate::to_string_pretty(self, crate::PrettyConfig::default())?;
        Ok(format!(
            "#![schema(\"crate:crol@{}\")]\n\n{body}",
            own_requirement()
        ))
    }
}

/// Whether a document is a schema file: it says so itself, by naming crol's
/// own schema with `#![schema("crate:crol@...")]`, as [`SchemaFile::to_text`]
/// writes it (`SPEC.md` 10.10). That schema is
/// `build::<SchemaFile>()`, which tools use directly: it needs no crate
/// lookup, cache or network.
pub fn is_schema_file(text: &str) -> bool {
    attribute_remote(text).is_some_and(|source| names_own_schema(&source))
}

/// Whether a `#![schema("...")]` names the schema of this version of crol:
/// `crate:crol`, for no version in particular, for what it writes itself, or
/// for this version. Any other requirement goes the way of other crates.
fn names_own_schema(source: &str) -> bool {
    let Some(spec) = source.strip_prefix("crate:") else {
        return false;
    };
    match spec.split_once('@') {
        None => spec == "crol",
        Some((name, version)) => {
            name == "crol" && (version == own_requirement() || version == env!("CARGO_PKG_VERSION"))
        }
    }
}

/// The version requirement schema files name crol by: this version's
/// major, or major and minor before 1.0, as Cargo treats them.
fn own_requirement() -> String {
    let mut parts = env!("CARGO_PKG_VERSION").split('.');
    match (parts.next(), parts.next()) {
        (Some("0"), Some(minor)) => format!("0.{minor}"),
        (Some(major), _) => major.to_owned(),
        _ => unreachable!("crol's version has a major part"),
    }
}

/// The schema file a document names with `#![schema(...)]` (`SPEC.md`
/// 4.1), with the attribute's range: `None` without the attribute. A module
/// path is resolved from the document's directory, and `::` from `root`,
/// the root directory (`SPEC.md` 4.2): for tools, the project file's
/// directory, else the working directory or workspace folder. URLs and
/// crates need the `remote-schemas` feature; with it, `fetch` allows
/// fetching them, and without `fetch` only cached copies and local crates
/// are used.
pub fn attribute_path(
    text: &str,
    document: &Path,
    root: &Path,
    fetch: bool,
) -> Option<(crol_syntax::TextRange, Result<PathBuf, Error>)> {
    use crol_syntax::ast::{AstNode, AttributeKind};
    if !may_have_attributes(text) {
        return None;
    }
    let parse = crol_syntax::parse(text);
    let attr = parse.document().attributes().find_map(|a| match a.kind() {
        Some(AttributeKind::Schema(attr)) => Some(attr),
        _ => None,
    })?;
    let range = attr.syntax().text_range();
    let dir = crate::directory_of(document);
    let Some(module_path) = attr.module_path() else {
        let source = attr
            .string()
            .and_then(|s| crate::from_str::<String>(s.text()).ok());
        let result = match source {
            Some(source) => resolve_remote(&source, dir, fetch),
            None => Err(Error::new("schema: expected a module path or a string")),
        };
        return Some((range, result));
    };
    let module_path: String = module_path
        .syntax()
        .text()
        .to_string()
        .split_whitespace()
        .collect();
    Some((
        range,
        crate::resolve_module_path(&module_path, Mode::Value, root, Some(dir))
            .map_err(|e| Error::new(format!("schema: {e}"))),
    ))
}

/// The schema file a URL or a crate names (`SPEC.md` 4.1), for a document
/// or project in `dir`, cached. With the `remote-schemas` feature it is
/// fetched over HTTPS if `fetch` allows, blocking; without it, or without
/// `fetch`, only cached copies and local crates are used. Without any
/// `remote-schemas` feature, it is an error.
pub fn resolve_remote(source: &str, dir: &Path, fetch: bool) -> Result<PathBuf, Error> {
    #[cfg(feature = "_remote")]
    return crate::remote::resolve(source, dir, fetch);
    #[cfg(not(feature = "_remote"))]
    {
        let _ = (dir, fetch);
        Err(Error::new(format!(
            "schema: `{source}`: schemas from URLs and crates need the `remote-schemas` feature of crol"
        )))
    }
}

/// [`resolve_remote`] without blocking on the network, fetching if `fetch`
/// allows. It must run in a Tokio runtime.
#[cfg(feature = "remote-schemas-async")]
pub async fn resolve_remote_async(source: &str, dir: &Path, fetch: bool) -> Result<PathBuf, Error> {
    crate::remote::resolve_async(source, dir, fetch).await
}

/// For a document checked against the schema file `schema` (a project's,
/// or one a tool was given): a warning at its `#![schema(...)]` when that
/// names another file (`SPEC.md` 4.1), resolved as [`attribute_path`] does. `used` says which schema is used
/// instead: "the project's".
pub fn attribute_disagrees(
    text: &str,
    document: &Path,
    root: &Path,
    schema: &Path,
    used: &str,
    fetch: bool,
) -> Option<(crol_syntax::TextRange, String)> {
    let (range, named) = attribute_path(text, document, root, fetch)?;
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    match named {
        Ok(named) if canonical(&named) == canonical(schema) => None,
        Ok(_) => Some((
            range,
            format!("this document names another schema than {used}, which is used instead"),
        )),
        Err(e) => Some((range, format!("{e}; {used} is used instead"))),
    }
}

/// Whether a document can have attributes: most have none, and need not be
/// parsed to know that.
pub(crate) fn may_have_attributes(text: &str) -> bool {
    text.contains("#!")
}

/// The URL or crate a document's `#![schema("...")]` names, if it names
/// one: what [`attribute_path`] resolves, for fetching it beforehand.
pub fn attribute_remote(text: &str) -> Option<String> {
    use crol_syntax::ast::AttributeKind;
    if !may_have_attributes(text) {
        return None;
    }
    let parse = crol_syntax::parse(text);
    parse.document().attributes().find_map(|a| match a.kind() {
        Some(AttributeKind::Schema(attr)) => attr
            .string()
            .and_then(|s| crate::from_str::<String>(s.text()).ok()),
        _ => None,
    })
}

/// Builds the schema of `T` and every type it uses.
pub fn build<T: Schema + ?Sized>() -> SchemaFile {
    let mut registry = Registry::new();
    let root = match T::describe(&mut registry) {
        TypeExpr::Named(path) => path,
        // `root` is the path of a named type, so a type with no name of its
        // own, such as a map, gets one that data never writes.
        other => {
            let path = type_name(&other);
            let wrapper = TypeDef::Transparent {
                name: String::new(),
                doc: String::new(),
                source: (String::new(), 0),
                inner: other,
                constraint: None,
            };
            registry.types.insert(path.clone(), wrapper);
            path
        }
    };
    SchemaFile {
        format: FORMAT,
        root,
        types: registry.types,
        constraints: registry.constraints,
    }
}

/// Writes the schema of `T` to `path`, for an application's own
/// `--export-schema`.
pub fn write<T: Schema + ?Sized>(path: impl AsRef<Path>) -> Result<(), Error> {
    let path = path.as_ref();
    Mode::Value.check_path(path)?;
    let text = build::<T>().to_text()?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::new(format!("cannot create {}: {e}", parent.display())))?;
    }
    std::fs::write(path, text)
        .map_err(|e| Error::new(format!("cannot write {}: {e}", path.display())))
}

/// Checks that the schema file at `path` is the schema of `T`: fails if it
/// differs, and writes it instead when `CROL_UPDATE_SCHEMA=1` is set or the
/// file does not exist (`SPEC.md` 10.9). Run it from a test, so the file is
/// never silently out of date in CI. A relative path is from the crate's
/// directory when run by `cargo test`.
pub fn check_file<T: Schema + ?Sized>(path: impl AsRef<Path>) -> Result<(), Error> {
    let mut path = path.as_ref().to_path_buf();
    if path.is_relative()
        && let Ok(dir) = std::env::var("CARGO_MANIFEST_DIR")
    {
        path = PathBuf::from(dir).join(path);
    }
    Mode::Value.check_path(&path)?;
    let expected = build::<T>().to_text()?;
    let update = std::env::var("CROL_UPDATE_SCHEMA").is_ok_and(|v| v == "1");
    match std::fs::read_to_string(&path) {
        Ok(actual) if actual == expected => Ok(()),
        Ok(_) if !update => Err(Error::new(format!(
            "{} is not the schema of the types; run the test with CROL_UPDATE_SCHEMA=1 to update it",
            path.display()
        ))),
        _ => write::<T>(&path),
    }
}

// ---- Defaults for `&default` ----

/// The defaults a schema records, for `&default` (`SPEC.md` 6.1.3).
#[derive(Debug, Clone)]
pub(crate) struct SchemaDefaults {
    pub(crate) schema: SchemaFile,
    /// The path of the root type, if not the schema's.
    pub(crate) root: Option<String>,
}

impl SchemaDefaults {
    /// The default at `path` in the type at `type_path`. With `start`, an
    /// `Option` field with no default of its own is the default of what it
    /// holds.
    fn resolve(
        &self,
        type_path: &str,
        path: &[String],
        start: bool,
        depth: usize,
    ) -> Option<crate::Value> {
        if depth > 64 {
            return None;
        }
        match self.schema.get(type_path)? {
            TypeDef::Struct {
                fields, default, ..
            } => {
                let Some((first, rest)) = path.split_first() else {
                    return default.as_ref().map(|d| d.0.clone());
                };
                let field = fields.get(first)?;
                // A field's default is its own, or its value in the
                // struct's default.
                let own = match &field.default {
                    FieldDefault::Value(value) => Some(value.0.clone()),
                    _ => None,
                }
                .or_else(|| {
                    let default = default.as_ref()?;
                    crol_eval::value_at_path(
                        &default.0,
                        &[crol_eval::PathStep::Field(first.clone())],
                    )
                    .cloned()
                });
                if rest.is_empty() {
                    let empty = own.as_ref().is_none_or(is_none);
                    return match named_type(&field.ty) {
                        // `None` is no value to start from: a path goes
                        // through it to nothing.
                        Some(inner) if start && empty => self
                            .resolve(inner, &[], start, depth + 1)
                            .or(own.filter(|value| !is_none(value))),
                        _ => own,
                    };
                }
                if let Some(inner) = named_type(&field.ty)
                    && let Some(value) = self.resolve(inner, rest, start, depth + 1)
                {
                    return Some(value);
                }
                let steps: Vec<crol_eval::PathStep> = rest
                    .iter()
                    .cloned()
                    .map(crol_eval::PathStep::Field)
                    .collect();
                own.and_then(|value| crol_eval::value_at_path(&value, &steps).cloned())
            }
            TypeDef::Newtype { inner, .. } | TypeDef::Transparent { inner, .. } => match inner {
                // A key is no field: what is below it is a field of the value.
                TypeExpr::Map(_, value) => {
                    self.resolve(named_type(value)?, path.get(1..)?, start, depth + 1)
                }
                _ => self.resolve(named_type(inner)?, path, start, depth + 1),
            },
            _ => None,
        }
    }
}

/// Whether a value is an `Option`'s `None`.
fn is_none(value: &crate::Value) -> bool {
    matches!(&value.kind, crate::ValueKind::UnitName(name) if &**name == "None")
}

/// The named type a type expression stands for, through `Option`.
fn named_type(expr: &TypeExpr) -> Option<&str> {
    match expr {
        TypeExpr::Named(path) => Some(path),
        TypeExpr::Option(inner) => named_type(inner),
        _ => None,
    }
}

impl SchemaDefaults {
    /// The type a default's path starts from.
    fn start_type(&self, type_name: Option<&str>) -> Option<String> {
        Some(match type_name {
            // A type name must name one type; an ambiguous one needs `use`,
            // which tools resolve.
            Some(name) => {
                let mut found = self.schema.by_name(name);
                let (path, _) = found.next()?;
                if found.next().is_some() {
                    return None;
                }
                path.to_owned()
            }
            None => self.root.clone().unwrap_or(self.schema.root.clone()),
        })
    }
}

impl crol_eval::DefaultsProvider for SchemaDefaults {
    fn default_of(&self, type_name: Option<&str>, path: &[String]) -> Option<crate::Value> {
        self.resolve(&self.start_type(type_name)?, path, false, 0)
    }

    fn start_of(&self, type_name: Option<&str>, path: &[String]) -> Option<crate::Value> {
        self.resolve(&self.start_type(type_name)?, path, true, 0)
    }

    fn root_is_document(&self) -> bool {
        self.schema.data_root(self.root.as_deref()) == DataRoot::Document
    }

    fn has_constraints(&self) -> bool {
        self.any_constraints()
    }

    fn broken_constraints(
        &self,
        key: &str,
        value: &crate::Value,
        root: &dyn Fn(&str) -> Option<crate::Value>,
    ) -> Vec<(crate::Span, String)> {
        self.broken(key, value, root)
    }

    fn has_sealed(&self) -> bool {
        self.any_sealed()
    }

    fn changed_sealed(
        &self,
        key: &str,
        before: &crate::Value,
        after: &crate::Value,
    ) -> Vec<Vec<String>> {
        self.sealed(key, before, after)
    }
}

// ---- Helpers for `#[derive(Schema)]` ----

/// A type expression's text, as schema paths name generic arguments:
/// `U32`, `String`, `app::Limits`, `List(String)`.
pub fn type_name(expr: &TypeExpr) -> String {
    match expr {
        TypeExpr::Named(path) => path.clone(),
        TypeExpr::Option(inner) => format!("Option({})", type_name(inner)),
        TypeExpr::List(inner) => format!("List({})", type_name(inner)),
        TypeExpr::Set(inner) => format!("Set({})", type_name(inner)),
        TypeExpr::Array(inner, n) => format!("Array({}, {n})", type_name(inner)),
        TypeExpr::Map(k, v) => format!("Map({}, {})", type_name(k), type_name(v)),
        TypeExpr::Tuple(items) => {
            let items: Vec<String> = items.iter().map(type_name).collect();
            format!("Tuple([{}])", items.join(", "))
        }
        TypeExpr::Id(refers) => format!("Id({refers:?})"),
        other => format!("{other:?}"),
    }
}

/// `expr` with the ids in it restricted to naming `refers`
/// (`#[crol(refers(...))]`): an `Id`, or ids inside an option, list, set or
/// map of them.
pub fn with_refers(expr: TypeExpr, refers: Vec<String>) -> TypeExpr {
    match expr {
        TypeExpr::Id(_) => TypeExpr::Id(refers),
        TypeExpr::Option(inner) => TypeExpr::Option(Box::new(with_refers(*inner, refers))),
        TypeExpr::List(inner) => TypeExpr::List(Box::new(with_refers(*inner, refers))),
        TypeExpr::Set(inner) => TypeExpr::Set(Box::new(with_refers(*inner, refers))),
        TypeExpr::Array(inner, n) => TypeExpr::Array(Box::new(with_refers(*inner, refers)), n),
        TypeExpr::Map(k, v) => TypeExpr::Map(
            Box::new(with_refers(*k, refers.clone())),
            Box::new(with_refers(*v, refers)),
        ),
        other => other,
    }
}

/// A value, serialized as a recorded default; `None` if it cannot be
/// written as CROL (a NaN float, say).
pub fn default_value<T: Serialize + ?Sized>(value: &T) -> Option<RawValue> {
    crate::to_value(value).ok().map(RawValue)
}

/// A field's recorded default: its value, or `Unrecorded` if it cannot be
/// written as CROL.
pub fn recorded_default<T: Serialize + ?Sized>(value: &T) -> FieldDefault {
    default_value(value).map_or(FieldDefault::Unrecorded, FieldDefault::Value)
}

/// The default of an `Option` field without `#[serde(default)]`: `None`.
pub fn none_value() -> FieldDefault {
    FieldDefault::Value(RawValue(crate::Value::new(
        crate::ValueKind::UnitName("None".into()),
        crate::Span::detached(),
    )))
}

// ---- Implementations for std types ----

macro_rules! scalar {
    ($($ty:ty => $expr:ident),* $(,)?) => {
        $(
            impl Schema for $ty {
                fn describe(_: &mut Registry) -> TypeRef {
                    TypeExpr::$expr
                }
            }
        )*
    };
}

scalar! {
    bool => Bool, char => Char, String => String, str => String, PathBuf => String, Path => String,
    () => Unit, i8 => I8, i16 => I16, i32 => I32, i64 => I64, i128 => I128, isize => I64,
    u8 => U8, u16 => U16, u32 => U32, u64 => U64, u128 => U128, usize => U64, f32 => F32, f64 => F64,
}

impl Schema for crate::Id {
    fn describe(_: &mut Registry) -> TypeRef {
        TypeExpr::Id(Vec::new())
    }
}

impl Schema for crate::Value {
    fn describe(_: &mut Registry) -> TypeRef {
        TypeExpr::Any
    }
}

impl<T: Schema> Schema for Option<T> {
    fn describe(registry: &mut Registry) -> TypeRef {
        TypeExpr::Option(Box::new(T::describe(registry)))
    }
}

// Wrappers that serde makes transparent.
macro_rules! transparent {
    ($($ty:ident),* $(,)?) => {
        $(
            impl<T: Schema + ?Sized> Schema for $ty<T> {
                fn describe(registry: &mut Registry) -> TypeRef {
                    T::describe(registry)
                }
            }
        )*
    };
}

use std::rc::Rc;
use std::sync::Arc;
transparent!(Box, Rc, Arc);

impl<T: Schema + ?Sized> Schema for &T {
    fn describe(registry: &mut Registry) -> TypeRef {
        T::describe(registry)
    }
}

impl<T: Schema + ToOwned + ?Sized> Schema for std::borrow::Cow<'_, T> {
    fn describe(registry: &mut Registry) -> TypeRef {
        T::describe(registry)
    }
}

impl<T: Schema> Schema for Vec<T> {
    fn describe(registry: &mut Registry) -> TypeRef {
        TypeExpr::List(Box::new(T::describe(registry)))
    }
}

impl<T: Schema> Schema for [T] {
    fn describe(registry: &mut Registry) -> TypeRef {
        TypeExpr::List(Box::new(T::describe(registry)))
    }
}

impl<T: Schema> Schema for VecDeque<T> {
    fn describe(registry: &mut Registry) -> TypeRef {
        TypeExpr::List(Box::new(T::describe(registry)))
    }
}

impl<T: Schema, S> Schema for HashSet<T, S> {
    fn describe(registry: &mut Registry) -> TypeRef {
        TypeExpr::Set(Box::new(T::describe(registry)))
    }
}

impl<T: Schema> Schema for BTreeSet<T> {
    fn describe(registry: &mut Registry) -> TypeRef {
        TypeExpr::Set(Box::new(T::describe(registry)))
    }
}

impl<K: Schema, V: Schema, S> Schema for HashMap<K, V, S> {
    fn describe(registry: &mut Registry) -> TypeRef {
        TypeExpr::Map(
            Box::new(K::describe(registry)),
            Box::new(V::describe(registry)),
        )
    }
}

impl<K: Schema, V: Schema> Schema for BTreeMap<K, V> {
    fn describe(registry: &mut Registry) -> TypeRef {
        TypeExpr::Map(
            Box::new(K::describe(registry)),
            Box::new(V::describe(registry)),
        )
    }
}

impl<K: Schema, V: Schema, S> Schema for IndexMap<K, V, S> {
    fn describe(registry: &mut Registry) -> TypeRef {
        TypeExpr::Map(
            Box::new(K::describe(registry)),
            Box::new(V::describe(registry)),
        )
    }
}

impl<T: Schema, const N: usize> Schema for [T; N] {
    fn describe(registry: &mut Registry) -> TypeRef {
        TypeExpr::Array(Box::new(T::describe(registry)), N)
    }
}

macro_rules! tuple {
    ($($name:ident)+) => {
        impl<$($name: Schema),+> Schema for ($($name,)+) {
            fn describe(registry: &mut Registry) -> TypeRef {
                TypeExpr::Tuple(vec![$($name::describe(registry)),+])
            }
        }
    };
}

tuple!(A);
tuple!(A B);
tuple!(A B C);
tuple!(A B C D);
tuple!(A B C D E);
tuple!(A B C D E F);
tuple!(A B C D E F G);
tuple!(A B C D E F G H);
tuple!(A B C D E F G H I);
tuple!(A B C D E F G H I J);
tuple!(A B C D E F G H I J K);
tuple!(A B C D E F G H I J K L);

/// serde writes a `Duration` as a struct of seconds and nanoseconds.
impl Schema for std::time::Duration {
    fn describe(registry: &mut Registry) -> TypeRef {
        registry.add("core::time::Duration", |_| TypeDef::Struct {
            name: "Duration".to_owned(),
            doc: String::new(),
            source: ("library/core/src/time.rs".to_owned(), 0),
            fields: [
                ("secs".to_owned(), Field::new(TypeExpr::U64)),
                ("nanos".to_owned(), Field::new(TypeExpr::U32)),
            ]
            .into_iter()
            .collect(),
            deny_unknown_fields: false,
            default: None,
            explicit_defaults: None,
            scope: false,
        })
    }
}

impl Schema for crate::RawValue {
    fn describe(_: &mut Registry) -> TypeRef {
        TypeExpr::Any
    }
}

// ---- The schema of schema files (`SPEC.md` 10.10) ----

/// A struct's definition, for hand-written schemas.
#[track_caller]
pub(crate) fn struct_def(
    name: &str,
    doc: &str,
    fields: Vec<(&str, Field)>,
    default: Option<RawValue>,
) -> TypeDef {
    let (file, line) = caller();
    TypeDef::Struct {
        name: name.to_owned(),
        doc: doc.to_owned(),
        source: (file, line),
        fields: named(fields),
        deny_unknown_fields: false,
        default,
        explicit_defaults: None,
        scope: false,
    }
}

/// An enum's definition, for hand-written schemas.
#[track_caller]
pub(crate) fn enum_def(name: &str, doc: &str, variants: Vec<(&str, VariantDef)>) -> TypeDef {
    TypeDef::Enum {
        name: name.to_owned(),
        doc: doc.to_owned(),
        source: caller(),
        repr: Repr::External,
        variants: named(variants),
    }
}

/// The place of the call to a function that tracks its caller, as `source`
/// has it.
#[track_caller]
fn caller() -> (String, u32) {
    let location = std::panic::Location::caller();
    (location.file().replace('\\', "/"), location.line())
}

fn named<T>(items: Vec<(&str, T)>) -> IndexMap<String, T> {
    items
        .into_iter()
        .map(|(name, item)| (name.to_owned(), item))
        .collect()
}

/// A field, for hand-written schemas.
pub(crate) fn field(ty: TypeRef, doc: &str, default: FieldDefault) -> Field {
    let mut field = Field::new(ty);
    field.doc = doc.to_owned();
    field.default = default;
    field
}

/// A variant, for hand-written schemas.
pub(crate) fn variant(shape: VariantShape, doc: &str) -> VariantDef {
    VariantDef {
        shape,
        doc: doc.to_owned(),
        aliases: Vec::new(),
        explicit_defaults: None,
        scope: false,
    }
}

fn list(item: TypeRef) -> TypeRef {
    TypeExpr::List(Box::new(item))
}

fn string_map(value: TypeRef) -> TypeRef {
    TypeExpr::Map(Box::new(TypeExpr::String), Box::new(value))
}

// What a schema file holds, as its types say it: a type here that differs
// from the `Deserialize` of the type is a bug, and the tests of the crate
// check generated files against this. The docs are those of the types.

/// The fields every definition starts with.
fn definition_fields(fields: Vec<(&'static str, Field)>) -> Vec<(&'static str, Field)> {
    let mut all = vec![
        (
            "name",
            field(
                TypeExpr::String,
                "The name data writes: the serde name.",
                FieldDefault::Required,
            ),
        ),
        ("doc", doc_field()),
        (
            "source",
            field(
                TypeExpr::Tuple(vec![TypeExpr::String, TypeExpr::U32]),
                "Where it is defined: the file, relative to the Cargo workspace root, and the line.",
                FieldDefault::Required,
            ),
        ),
    ];
    all.extend(fields);
    all
}

fn doc_field() -> Field {
    field(
        TypeExpr::String,
        "The doc comment, which editors show.",
        recorded_default(""),
    )
}

fn aliases_field() -> Field {
    field(
        list(TypeExpr::String),
        "Other names data may write it by: `#[serde(alias = \"...\")]`.",
        recorded_default(&Vec::<String>::new()),
    )
}

fn scope_doc(doc: &str) -> Field {
    field(TypeExpr::Bool, doc, recorded_default(&false))
}

fn explicit_defaults_doc() -> Field {
    field(
        TypeExpr::Option(Box::new(TypeExpr::Bool)),
        "`#[crol(explicit_defaults)]`: whether fields take their defaults only with `..` (`SPEC.md` 10.3). `None`: `require_complete` says.",
        none_value(),
    )
}

impl Schema for SchemaFile {
    fn describe(registry: &mut Registry) -> TypeRef {
        registry.add(SCHEMA_PATH, |registry| {
            let types = string_map(registry.describe::<TypeDef>());
            let constraints = string_map(registry.describe::<ConstraintDef>());
            let mut definition = struct_def(
                "Schema",
                "A schema file: the Rust types data loads into, as tools see them. It is generated from those types, so change them rather than this file.",
                vec![
                    (
                        "format",
                        field(
                            TypeExpr::U32,
                            "The format version of this file. It changes only when the format of schema files does.",
                            FieldDefault::Required,
                        ),
                    ),
                    (
                        "root",
                        field(
                            TypeExpr::String,
                            "The path of the root type: the type the schema was built for. In data, a struct or map is the whole document, and any other type that of each root value (`SPEC.md` 4.1).",
                            FieldDefault::Required,
                        ),
                    ),
                    (
                        "types",
                        field(
                            types,
                            "Every named type the root type uses, by path.",
                            FieldDefault::Required,
                        ),
                    ),
                    (
                        "constraints",
                        field(
                            constraints,
                            "The constraints the program defines, by name (`SPEC.md` 10.12).",
                            recorded_default(&BTreeMap::<String, String>::new()),
                        ),
                    ),
                ],
                None,
            );
            if let TypeDef::Struct {
                explicit_defaults, ..
            } = &mut definition
            {
                *explicit_defaults = Some(false);
            }
            definition
        })
    }
}

impl Schema for TypeDef {
    fn describe(registry: &mut Registry) -> TypeRef {
        registry.add("crol::schema::TypeDef", |registry| {
            let expr = registry.describe::<TypeExpr>();
            let fields = string_map(registry.describe::<Field>());
            let variants = string_map(registry.describe::<VariantDef>());
            let repr = registry.describe::<Repr>();
            let wrapped = || {
                definition_fields(vec![
                    (
                        "inner",
                        field(expr.clone(), "The wrapped type.", FieldDefault::Required),
                    ),
                    (
                        "constraint",
                        field(
                            TypeExpr::Option(Box::new(TypeExpr::String)),
                            "The constraint on the value, wherever the type is used.",
                            none_value(),
                        ),
                    ),
                ])
            };
            let shape = |fields| VariantShape::Struct(named(fields));
            enum_def(
                "TypeDef",
                "The definition of a named type.",
                vec![
                    (
                        "Struct",
                        variant(
                            shape(definition_fields(vec![
                                (
                                    "fields",
                                    field(
                                        fields,
                                        "The fields, by the name data writes them with.",
                                        FieldDefault::Required,
                                    ),
                                ),
                                (
                                    "deny_unknown_fields",
                                    field(
                                        TypeExpr::Bool,
                                        "`#[serde(deny_unknown_fields)]`: whether a field it does not have is an error even where unknown fields are allowed.",
                                        recorded_default(&false),
                                    ),
                                ),
                                (
                                    "default",
                                    field(
                                        TypeExpr::Option(Box::new(TypeExpr::Any)),
                                        "The struct's recorded default value, with `#[crol(defaults)]` and `#[serde(default)]` on the struct.",
                                        none_value(),
                                    ),
                                ),
                                ("explicit_defaults", explicit_defaults_doc()),
                                ("scope", scope_doc("`#[crol(scope)]`: whether its values are scopes for the ids in them (`SPEC.md` 10.9).")),
                            ])),
                            "A struct with named fields.",
                        ),
                    ),
                    (
                        "TupleStruct",
                        variant(
                            shape(definition_fields(vec![(
                                "fields",
                                field(
                                    list(expr.clone()),
                                    "The type of each element.",
                                    FieldDefault::Required,
                                ),
                            )])),
                            "A tuple struct: `struct P(A, B)`.",
                        ),
                    ),
                    (
                        "Newtype",
                        variant(
                            shape(wrapped()),
                            "A newtype struct, `struct N(A)`: data writes `N(x)` or `x`.",
                        ),
                    ),
                    (
                        "Transparent",
                        variant(
                            shape(wrapped()),
                            "A struct with `#[serde(transparent)]`: data writes only `x`.",
                        ),
                    ),
                    (
                        "UnitStruct",
                        variant(
                            shape(definition_fields(Vec::new())),
                            "A unit struct: `struct U;`.",
                        ),
                    ),
                    (
                        "Enum",
                        variant(
                            shape(definition_fields(vec![
                                (
                                    "repr",
                                    field(
                                        repr,
                                        "How serde represents it; `External` if left out.",
                                        recorded_default(&Repr::External),
                                    ),
                                ),
                                (
                                    "variants",
                                    field(
                                        variants,
                                        "The variants, by the name data writes them with.",
                                        FieldDefault::Required,
                                    ),
                                ),
                            ])),
                            "An enum.",
                        ),
                    ),
                ],
            )
        })
    }
}

impl Schema for Repr {
    fn describe(registry: &mut Registry) -> TypeRef {
        registry.add("crol::schema::Repr", |_| {
            let name = |name, doc| (name, field(TypeExpr::String, doc, FieldDefault::Required));
            let tag = "The name of the field that says the variant.";
            enum_def(
                "Repr",
                "How serde represents an enum.",
                vec![
                    (
                        "External",
                        variant(
                            VariantShape::Unit,
                            "The default: data writes `V`, `V(x)` or `V { ... }`.",
                        ),
                    ),
                    (
                        "Internal",
                        variant(
                            VariantShape::Struct(named(vec![name("tag", tag)])),
                            "`#[serde(tag = \"...\")]`: the variant is a field of the block.",
                        ),
                    ),
                    (
                        "Adjacent",
                        variant(
                            VariantShape::Struct(named(vec![
                                name("tag", tag),
                                name("content", "The name of the field that holds its value."),
                            ])),
                            "`#[serde(tag = \"...\", content = \"...\")]`: the variant and its value are two fields of the block.",
                        ),
                    ),
                    (
                        "Untagged",
                        variant(
                            VariantShape::Unit,
                            "`#[serde(untagged)]`: only the shape of the value says the variant.",
                        ),
                    ),
                ],
            )
        })
    }
}

impl Schema for VariantDef {
    fn describe(registry: &mut Registry) -> TypeRef {
        registry.add("crol::schema::VariantDef", |registry| {
            let shape = registry.describe::<VariantShape>();
            let mut explicit_defaults = explicit_defaults_doc();
            explicit_defaults.doc =
                "For a struct variant, `#[crol(explicit_defaults)]`, as on a struct.".to_owned();
            struct_def(
                "",
                "A variant of an enum.",
                vec![
                    (
                        "shape",
                        field(shape, "What the variant holds.", FieldDefault::Required),
                    ),
                    ("doc", doc_field()),
                    ("aliases", aliases_field()),
                    ("explicit_defaults", explicit_defaults),
                    (
                        "scope",
                        scope_doc("For a struct variant, `#[crol(scope)]`, as on a struct."),
                    ),
                ],
                None,
            )
        })
    }
}

impl Schema for VariantShape {
    fn describe(registry: &mut Registry) -> TypeRef {
        registry.add("crol::schema::VariantShape", |registry| {
            let expr = registry.describe::<TypeExpr>();
            let fields = string_map(registry.describe::<Field>());
            enum_def(
                "VariantShape",
                "What a variant holds.",
                vec![
                    (
                        "Unit",
                        variant(VariantShape::Unit, "No value: data writes `V`."),
                    ),
                    (
                        "Newtype",
                        variant(
                            VariantShape::Newtype(expr.clone()),
                            "One value of this type: data writes `V(x)`.",
                        ),
                    ),
                    (
                        "Tuple",
                        variant(
                            VariantShape::Newtype(list(expr)),
                            "Values of these types: data writes `V(a, b)`.",
                        ),
                    ),
                    (
                        "Struct",
                        variant(
                            VariantShape::Newtype(fields),
                            "Named fields: data writes `V { ... }`.",
                        ),
                    ),
                ],
            )
        })
    }
}

impl Schema for Field {
    fn describe(registry: &mut Registry) -> TypeRef {
        registry.add("crol::schema::Field", |registry| {
            let ty = registry.describe::<TypeExpr>();
            let default = registry.describe::<FieldDefault>();
            struct_def(
                "",
                "A field of a struct or struct variant.",
                vec![
                    (
                        "ty",
                        field(ty, "The type of the field.", FieldDefault::Required),
                    ),
                    ("doc", doc_field()),
                    ("aliases", aliases_field()),
                    (
                        "default",
                        field(
                            default,
                            "Its default, for when `..` leaves it out.",
                            recorded_default(&FieldDefault::Required),
                        ),
                    ),
                    (
                        "flatten",
                        field(
                            TypeExpr::Bool,
                            "`#[serde(flatten)]`: its fields are written in the block around it.",
                            recorded_default(&false),
                        ),
                    ),
                    (
                        "refers",
                        field(
                            list(TypeExpr::String),
                            "For an id field, the root types an id may name (`#[crol(refers(...))]`).",
                            recorded_default(&Vec::<String>::new()),
                        ),
                    ),
                    (
                        "constraint",
                        field(
                            TypeExpr::Option(Box::new(TypeExpr::String)),
                            "The constraint on its value, or on what a collection holds (`#[crol(constraint = \"...\")]`).",
                            none_value(),
                        ),
                    ),
                    (
                        "key_constraint",
                        field(
                            TypeExpr::Option(Box::new(TypeExpr::String)),
                            "The constraint on a map's keys (`#[crol(key_constraint = \"...\")]`).",
                            none_value(),
                        ),
                    ),
                    (
                        "sealed",
                        field(
                            TypeExpr::Bool,
                            "`#[crol(sealed)]`: only the layer that defines a value can set this field (`SPEC.md` 10.13).",
                            recorded_default(&false),
                        ),
                    ),
                ],
                None,
            )
        })
    }
}

impl Schema for ConstraintDef {
    fn describe(registry: &mut Registry) -> TypeRef {
        registry.add("crol::schema::ConstraintDef", |registry| {
            let constraint = registry.describe::<Constraint>();
            struct_def(
                "",
                "A constraint in a schema, under its name.",
                vec![
                    (
                        "constraint",
                        field(constraint, "What it says.", FieldDefault::Required),
                    ),
                    (
                        "sealed",
                        field(
                            TypeExpr::Bool,
                            "Whether a `schema.crod` may not change it: one the program defines is, unless it is marked open (`SPEC.md` 10.12).",
                            recorded_default(&false),
                        ),
                    ),
                ],
                None,
            )
        })
    }
}

impl Schema for Constraint {
    fn describe(registry: &mut Registry) -> TypeRef {
        registry.add("crol::schema::Constraint", |_| {
            let variant = |shape, doc: &str| VariantDef {
                shape,
                doc: doc.to_owned(),
                aliases: Vec::new(),
                explicit_defaults: None,
                scope: false,
            };
            enum_def(
                "Constraint",
                "What a field's value must be (`SPEC.md` 10.12).",
                vec![
                    (
                        "Min",
                        variant(
                            VariantShape::Newtype(TypeExpr::F64),
                            "A number not below this.",
                        ),
                    ),
                    (
                        "Max",
                        variant(
                            VariantShape::Newtype(TypeExpr::F64),
                            "A number not above this.",
                        ),
                    ),
                    (
                        "Range",
                        variant(
                            VariantShape::Tuple(vec![TypeExpr::F64, TypeExpr::F64]),
                            "A number from the first to the second, inclusive.",
                        ),
                    ),
                    (
                        "OneOf",
                        variant(
                            VariantShape::Newtype(list(TypeExpr::Any)),
                            "One of these values. A string, an id and a unit name count as their text.",
                        ),
                    ),
                    (
                        "Refers",
                        variant(
                            VariantShape::Newtype(list(TypeExpr::String)),
                            "The id of a root value of one of these types.",
                        ),
                    ),
                ],
            )
        })
    }
}

impl Schema for FieldDefault {
    fn describe(registry: &mut Registry) -> TypeRef {
        registry.add("crol::schema::FieldDefault", |_| {
            enum_def(
                "FieldDefault",
                "The default of a field (`SPEC.md` 10.10).",
                vec![
                    (
                        "Required",
                        variant(
                            VariantShape::Unit,
                            "No default: the field must be written, even with `..`.",
                        ),
                    ),
                    (
                        "Unrecorded",
                        variant(VariantShape::Unit, "A default whose value is not recorded."),
                    ),
                    (
                        "Value",
                        variant(VariantShape::Newtype(TypeExpr::Any), "The default value."),
                    ),
                ],
            )
        })
    }
}

impl Schema for TypeExpr {
    fn describe(registry: &mut Registry) -> TypeRef {
        let path = "crol::schema::TypeExpr";
        let expr = TypeExpr::Named(path.to_owned());
        registry.add(path, |_| {
            let unit = |name, doc| (name, variant(VariantShape::Unit, doc));
            let holds = |name, ty, doc| (name, variant(VariantShape::Newtype(ty), doc));
            let pair = |name, a, b, doc| (name, variant(VariantShape::Tuple(vec![a, b]), doc));
            enum_def(
                "TypeExpr",
                "A type expression (`SPEC.md` 10.10).",
                vec![
                    unit("Bool", "`bool`."),
                    unit("Char", "`char`."),
                    unit("String", "`String`, `str` or `PathBuf`."),
                    unit("Unit", "`()`."),
                    unit("I8", "`i8`."),
                    unit("I16", "`i16`."),
                    unit("I32", "`i32`."),
                    unit("I64", "`i64` or `isize`."),
                    unit("I128", "`i128`."),
                    unit("U8", "`u8`."),
                    unit("U16", "`u16`."),
                    unit("U32", "`u32`."),
                    unit("U64", "`u64` or `usize`."),
                    unit("U128", "`u128`."),
                    unit("F32", "`f32`."),
                    unit("F64", "`f64`."),
                    unit("Bytes", "`serde_bytes` and the like."),
                    holds(
                        "Id",
                        list(TypeExpr::String),
                        "`crol::Id`, with the types it may refer to (`[]`: any root key).",
                    ),
                    holds("Option", expr.clone(), "`Option<T>`."),
                    holds("List", expr.clone(), "`Vec<T>`, `VecDeque<T>` or a slice."),
                    holds(
                        "Set",
                        expr.clone(),
                        "`HashSet<T>` or `BTreeSet<T>`: its elements are unique.",
                    ),
                    pair(
                        "Array",
                        expr.clone(),
                        TypeExpr::U64,
                        "`[T; n]`: the type of its elements, and how many there are.",
                    ),
                    holds(
                        "Tuple",
                        list(expr.clone()),
                        "A tuple, with the type of each element.",
                    ),
                    pair(
                        "Map",
                        expr.clone(),
                        expr.clone(),
                        "`HashMap<K, V>`, `BTreeMap<K, V>` or `IndexMap<K, V>`: the type of the keys, and of the values.",
                    ),
                    holds(
                        "Named",
                        TypeExpr::String,
                        "A type in the schema's `types`, by its path.",
                    ),
                    unit("Any", "Anything; not checked."),
                ],
            )
        })
    }
}
