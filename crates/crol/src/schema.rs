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
use crate::raw::RawValue;

/// The format version of schema files (`SPEC.md` 10.10).
pub const FORMAT: u32 = 1;

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

    /// Describes `T`.
    pub fn describe<T: Schema + ?Sized>(&mut self) -> TypeRef {
        T::describe(self)
    }
}

/// A schema: its root type and the named types it uses (`SPEC.md` 10.10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "Schema")]
pub struct SchemaFile {
    /// The format version, [`FORMAT`].
    pub format: u32,
    /// The path of the root type.
    pub root: String,
    /// Each named type's path and definition, sorted by path.
    pub types: BTreeMap<String, TypeDef>,
}

/// A named type's definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TypeDef {
    /// A struct with named fields.
    Struct {
        /// The serde name.
        name: String,
        /// The doc comment.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        doc: String,
        /// The file, relative to the Cargo workspace root, and line.
        source: (String, u32),
        /// The fields, by serde name.
        fields: IndexMap<String, Field>,
        /// `#[serde(deny_unknown_fields)]`.
        #[serde(default, skip_serializing_if = "is_false")]
        deny_unknown_fields: bool,
        /// The struct's recorded default value, with `#[crol(defaults)]` and
        /// `#[serde(default)]` on the struct.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<RawValue>,
    },
    /// `struct P(A, B)`
    TupleStruct {
        /// The serde name.
        name: String,
        /// The doc comment.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        doc: String,
        /// The file and line.
        source: (String, u32),
        /// The elements' types.
        fields: Vec<TypeExpr>,
    },
    /// `struct N(A)`: data writes `N(x)` or `x`.
    Newtype {
        /// The serde name.
        name: String,
        /// The doc comment.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        doc: String,
        /// The file and line.
        source: (String, u32),
        /// The wrapped type.
        inner: TypeExpr,
    },
    /// `#[serde(transparent)]`: data writes only `x`.
    Transparent {
        /// The serde name.
        name: String,
        /// The doc comment.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        doc: String,
        /// The file and line.
        source: (String, u32),
        /// The wrapped type.
        inner: TypeExpr,
    },
    /// `struct U;`
    UnitStruct {
        /// The serde name.
        name: String,
        /// The doc comment.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        doc: String,
        /// The file and line.
        source: (String, u32),
    },
    /// An enum.
    Enum {
        /// The serde name.
        name: String,
        /// The doc comment.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        doc: String,
        /// The file and line.
        source: (String, u32),
        /// How serde represents it.
        #[serde(default, skip_serializing_if = "Repr::is_external")]
        repr: Repr,
        /// The variants, by serde name.
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
    /// The default: `V`, `V(x)`, `V { ... }`.
    #[default]
    External,
    /// `#[serde(tag = "...")]`
    Internal {
        /// The tag field.
        tag: String,
    },
    /// `#[serde(tag = "...", content = "...")]`
    Adjacent {
        /// The tag field.
        tag: String,
        /// The content field.
        content: String,
    },
    /// `#[serde(untagged)]`
    Untagged,
}

impl Repr {
    fn is_external(&self) -> bool {
        *self == Repr::External
    }
}

/// An enum variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "")]
pub struct VariantDef {
    /// Its payload.
    pub shape: VariantShape,
    /// The doc comment.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub doc: String,
    /// serde aliases.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
}

/// A variant's payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum VariantShape {
    /// `V`
    Unit,
    /// `V(x)`
    Newtype(TypeExpr),
    /// `V(a, b)`
    Tuple(Vec<TypeExpr>),
    /// `V { ... }`
    Struct(IndexMap<String, Field>),
}

/// A field of a struct or struct variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "")]
pub struct Field {
    /// Its type.
    pub ty: TypeExpr,
    /// The doc comment.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub doc: String,
    /// serde aliases.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Its default.
    #[serde(default, skip_serializing_if = "FieldDefault::is_required")]
    pub default: FieldDefault,
    /// `#[serde(flatten)]`
    #[serde(default, skip_serializing_if = "is_false")]
    pub flatten: bool,
    /// For an id field, the root types an id may name (`#[crol(refers(...))]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refers: Vec<String>,
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
        }
    }
}

/// A field's default (`SPEC.md` 10.10).
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
#[allow(missing_docs)]
pub enum TypeExpr {
    Bool,
    Char,
    String,
    Unit,
    I8,
    I16,
    I32,
    I64,
    I128,
    U8,
    U16,
    U32,
    U64,
    U128,
    F32,
    F64,
    /// `serde_bytes` and the like.
    Bytes,
    /// `crol::Id`, with the types it may refer to (`[]`: any root key).
    Id(Vec<String>),
    Option(Box<TypeExpr>),
    List(Box<TypeExpr>),
    /// A set: its elements are unique.
    Set(Box<TypeExpr>),
    /// `[T; n]`
    Array(Box<TypeExpr>, usize),
    Tuple(Vec<TypeExpr>),
    Map(Box<TypeExpr>, Box<TypeExpr>),
    /// A type in the schema's `types`.
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

    /// Reads a schema file.
    pub fn read(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::new(format!("cannot read {}: {e}", path.display())))?;
        Self::from_text(&text)
    }

    /// Parses a schema file's text.
    pub fn from_text(text: &str) -> Result<Self, Error> {
        let schema: SchemaFile = crate::from_str(text)?;
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
/// path is resolved with the document's directory as the root. URLs and
/// crates need the `remote-schemas` feature; with it, `fetch` allows
/// fetching them, and without `fetch` only cached copies and local crates
/// are used.
pub fn attribute_path(
    text: &str,
    document: &Path,
    fetch: bool,
) -> Option<(crol_syntax::TextRange, Result<PathBuf, Error>)> {
    use crol_syntax::ast::{AstNode, AttributeKind};
    let parse = crol_syntax::parse(text);
    let attr = parse.document().attributes().find_map(|a| match a.kind() {
        Some(AttributeKind::Schema(attr)) => Some(attr),
        _ => None,
    })?;
    let range = attr.syntax().text_range();
    let dir = document.parent().unwrap_or(Path::new("."));
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
        crate::resolve_module_path(&module_path, dir, Some(document))
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

/// The URL or crate a document's `#![schema("...")]` names, if it names
/// one: what [`attribute_path`] resolves, for fetching it beforehand.
pub fn attribute_remote(text: &str) -> Option<String> {
    use crol_syntax::ast::AttributeKind;
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
    let root = T::describe(&mut registry);
    let root = match root {
        TypeExpr::Named(path) => path,
        other => format!("{other:?}"),
    };
    SchemaFile {
        format: FORMAT,
        root,
        types: registry.types,
    }
}

/// Writes the schema of `T` to `path`, for an application's own
/// `--export-schema`.
pub fn write<T: Schema + ?Sized>(path: impl AsRef<Path>) -> Result<(), Error> {
    let path = path.as_ref();
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
pub(crate) struct SchemaDefaults(pub(crate) SchemaFile);

impl SchemaDefaults {
    /// The default at `path` in the type at `type_path`.
    fn resolve(&self, type_path: &str, path: &[String], depth: usize) -> Option<crate::Value> {
        if depth > 64 {
            return None;
        }
        match self.0.get(type_path)? {
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
                    return own;
                }
                if let Some(inner) = named_type(&field.ty)
                    && let Some(value) = self.resolve(inner, rest, depth + 1)
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
            TypeDef::Newtype { inner, .. } | TypeDef::Transparent { inner, .. } => {
                self.resolve(named_type(inner)?, path, depth + 1)
            }
            _ => None,
        }
    }
}

/// The named type a type expression stands for, through `Option`.
fn named_type(expr: &TypeExpr) -> Option<&str> {
    match expr {
        TypeExpr::Named(path) => Some(path),
        TypeExpr::Option(inner) => named_type(inner),
        _ => None,
    }
}

impl crol_eval::DefaultsProvider for SchemaDefaults {
    fn default_of(&self, type_name: Option<&str>, path: &[String]) -> Option<crate::Value> {
        let type_path = match type_name {
            // A type name must name one type; an ambiguous one needs `use`,
            // which tools resolve.
            Some(name) => {
                let mut found = self.0.by_name(name);
                let (path, _) = found.next()?;
                if found.next().is_some() {
                    return None;
                }
                path.to_owned()
            }
            None => self.0.root.clone(),
        };
        self.resolve(&type_path, path, 0)
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
        crate::ValueKind::UnitName("None".to_owned()),
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
        })
    }
}

/// The schema of schema files, so that editors check them too.
impl Schema for SchemaFile {
    fn describe(registry: &mut Registry) -> TypeRef {
        // Schema files are validated by reading them; their own schema is
        // not described in detail.
        registry.add("crol::schema::Schema", |_| TypeDef::Struct {
            name: "Schema".to_owned(),
            doc: "A CROL schema file (SPEC.md 10.10).".to_owned(),
            source: (file!().replace('\\', "/"), line!()),
            fields: [
                ("format".to_owned(), Field::new(TypeExpr::U32)),
                ("root".to_owned(), Field::new(TypeExpr::String)),
                (
                    "types".to_owned(),
                    Field::new(TypeExpr::Map(
                        Box::new(TypeExpr::String),
                        Box::new(TypeExpr::Any),
                    )),
                ),
            ]
            .into_iter()
            .collect(),
            deny_unknown_fields: false,
            default: None,
        })
    }
}
