//! `#[type(...)]` on value binds (`SPEC.md` 6.1.2): the type it names, and
//! whether the value written after it fits that type.
//!
//! A value bind's value is often computed where it lands (`&` in a template
//! reads the value below it there), so it is checked as written, not as
//! evaluated: literals, blocks, lists, tuples and names are checked, and what
//! is computed (references, operators, macro calls) is taken to fit. A value
//! is never reported unless its text alone says it does not fit.

use std::collections::HashMap;

use crol_eval::{Diagnostic, FileId, Span};
use crol_syntax::SyntaxKind::*;
use crol_syntax::ast::{self, AstNode, TypeForm, TypeStepKind};
use crol_syntax::{SyntaxToken, TextRange};
use indexmap::IndexMap;

use crate::CheckOptions;
use crate::de::{NameFit, TypeNames};
use crate::schema::{Field, Repr, SchemaFile, TypeDef, TypeExpr, VariantShape};

/// How deep to follow a type or a value; types can contain themselves.
const MAX_DEPTH: usize = 64;

/// Something wrong with a `#[type(...)]`: where, and what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeProblem {
    /// The part of the document it is about.
    pub range: TextRange,
    /// What is wrong.
    pub message: String,
}

impl TypeProblem {
    fn new(range: TextRange, message: impl Into<String>) -> Self {
        Self {
            range,
            message: message.into(),
        }
    }
}

/// Where a type reference has got to: a type, or a variant of an enum,
/// which only a step can go on from.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeAt {
    /// A type, with the doc of the field stepped into last, if any.
    Type {
        /// The type.
        ty: TypeExpr,
        /// The doc of the last field stepped into.
        doc: Option<String>,
    },
    /// `Shape::Circle`: a variant.
    Variant {
        /// The path of the enum.
        path: String,
        /// The variant's serde name.
        name: String,
    },
}

/// What a step from a [`TypeAt`] can be, for completion.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeStepOffer {
    /// The step as written after the `.`: a field name, an index or `[]`.
    pub label: String,
    /// The type the step leads to.
    pub ty: TypeExpr,
    /// The field's doc, for a field.
    pub doc: Option<String>,
}

/// The type names a document's `#![use(...)]` attributes give, each to the
/// schema path it stands for (`SPEC.md` 4.1).
#[derive(Debug, Clone, Default)]
pub struct TypeUses(HashMap<String, String>);

impl TypeUses {
    /// The names `document`'s attributes give.
    pub fn of(document: &ast::Document, schema: &SchemaFile) -> Self {
        let mut names = HashMap::new();
        let resolver = Resolver {
            schema,
            uses: &TypeUses::default(),
        };
        for attribute in document.attributes() {
            let Some(ast::AttributeKind::Use(attr)) = attribute.kind() else {
                continue;
            };
            for path in attr.paths() {
                let segments: Vec<String> = path.segments().map(|t| t.text().to_owned()).collect();
                let Some(last) = segments.last().cloned() else {
                    continue;
                };
                let mut full = segments.join("::");
                if let Some(args) = path.type_args() {
                    let args: Option<Vec<String>> = args
                        .args()
                        .map(|arg| {
                            resolver
                                .expr(&arg)
                                .ok()
                                .map(|t| crate::schema::type_name(&t))
                        })
                        .collect();
                    let Some(args) = args else { continue };
                    full = format!("{full}<{}>", args.join(", "));
                }
                let name = path.alias().map_or(last, |alias| alias.text().to_owned());
                names.insert(name, full);
            }
        }
        Self(names)
    }
}

/// What `type_ref` names in `schema`, with the names `uses` gives.
pub fn resolve(
    schema: &SchemaFile,
    uses: &TypeUses,
    type_ref: &ast::TypeRef,
) -> Result<(TypeExpr, Option<String>), TypeProblem> {
    match resolve_steps(schema, uses, type_ref, usize::MAX)? {
        TypeAt::Type { ty, doc } => Ok((ty, doc)),
        TypeAt::Variant { name, .. } => Err(TypeProblem::new(
            type_ref.syntax().text_range(),
            format!(
                "`{name}` is a variant, not a type: go on with a step, such as `.field` or `.0`"
            ),
        )),
    }
}

/// Where `type_ref` has got to after its first `steps` steps.
pub fn resolve_steps(
    schema: &SchemaFile,
    uses: &TypeUses,
    type_ref: &ast::TypeRef,
    steps: usize,
) -> Result<TypeAt, TypeProblem> {
    let resolver = Resolver { schema, uses };
    let Some(start) = type_ref.ty() else {
        return Err(TypeProblem::new(
            type_ref.syntax().text_range(),
            "expected a type",
        ));
    };
    let mut at = resolver.start(&start)?;
    for step in type_ref.steps().take(steps) {
        at = resolver.step(at, &step)?;
    }
    Ok(at)
}

/// The steps that can follow `at`: the fields, indices or `[]` of its type.
pub fn next_steps(schema: &SchemaFile, at: &TypeAt) -> Vec<TypeStepOffer> {
    let mut out = Vec::new();
    match at {
        TypeAt::Type { ty, .. } => offers(schema, ty, &mut out, 0),
        TypeAt::Variant { path, name } => {
            if let Some(TypeDef::Enum { variants, .. }) = schema.get(path)
                && let Some(variant) = variants.get(name)
            {
                variant_offers(schema, &variant.shape, &mut out);
            }
        }
    }
    out
}

/// Checks every `#[type(...)]` in `document`: that it names a type, and that
/// the value after it fits that type, as far as its text says.
pub fn check_document(
    file: FileId,
    document: &ast::Document,
    schema: &SchemaFile,
    options: &CheckOptions,
) -> Vec<Diagnostic> {
    let uses = TypeUses::of(document, schema);
    let mut out = Vec::new();
    for bind in document
        .syntax()
        .descendants()
        .filter_map(ast::ValueBind::cast)
    {
        let Some(type_ref) = bind.attr().and_then(|attr| attr.type_ref()) else {
            continue;
        };
        match resolve(schema, &uses, &type_ref) {
            Ok((ty, _)) => {
                if let Some(value) = bind.value() {
                    let mut checker = ValueChecker {
                        schema,
                        options,
                        problems: Vec::new(),
                        names: TypeNames::default(),
                    };
                    checker.value(&value, &ty, 0);
                    out.extend(checker.problems);
                }
            }
            Err(problem) => out.push(problem),
        }
    }
    out.into_iter()
        .map(|p| Diagnostic::error(Span::new(file, p.range), p.message))
        .collect()
}

/// A type as Rust writes it, with serde names: `u32`, `Option<Limits>`.
pub fn rust_type(schema: &SchemaFile, ty: &TypeExpr) -> String {
    let inner = |t: &TypeExpr| rust_type(schema, t);
    match ty {
        TypeExpr::Bool => "bool".to_owned(),
        TypeExpr::Char => "char".to_owned(),
        TypeExpr::String => "String".to_owned(),
        TypeExpr::Unit => "()".to_owned(),
        TypeExpr::Any => "any value".to_owned(),
        TypeExpr::Bytes => "bytes".to_owned(),
        TypeExpr::Id(_) => "Id".to_owned(),
        TypeExpr::IdOr(_, t) => format!("IdOr<{}>", inner(t)),
        TypeExpr::Option(t) => format!("Option<{}>", inner(t)),
        TypeExpr::List(t) => format!("Vec<{}>", inner(t)),
        TypeExpr::Set(t) => format!("Set<{}>", inner(t)),
        TypeExpr::Array(t, n) => format!("[{}; {n}]", inner(t)),
        TypeExpr::Map(k, v) => format!("Map<{}, {}>", inner(k), inner(v)),
        TypeExpr::Tuple(items) => {
            let items: Vec<String> = items.iter().map(inner).collect();
            format!("({})", items.join(", "))
        }
        TypeExpr::Named(path) => match schema.get(path) {
            // What `schema::build` names, for a root type with no name.
            Some(TypeDef::Transparent {
                name,
                inner: wrapped,
                ..
            }) if name.is_empty() => inner(wrapped),
            // A type that data writes with no name.
            Some(def) if def.name().is_empty() => {
                path.rsplit("::").next().unwrap_or(path).to_owned()
            }
            Some(def) => def.name().to_owned(),
            None => path.clone(),
        },
        other => format!("{other:?}").to_lowercase(),
    }
}

// ---- Resolving types ----

struct Resolver<'s> {
    schema: &'s SchemaFile,
    uses: &'s TypeUses,
}

impl Resolver<'_> {
    /// The type a reference starts at: a type, or a variant.
    fn start(&self, expr: &ast::TypeExpr) -> Result<TypeAt, TypeProblem> {
        match self.expr(expr) {
            Ok(ty) => Ok(TypeAt::Type { ty, doc: None }),
            Err(problem) => {
                // `Shape::Circle`: the enum, and a variant of it.
                let segments: Vec<SyntaxToken> = expr.segments().collect();
                if expr.form() == TypeForm::Path
                    && segments.len() >= 2
                    && expr.type_args().is_none()
                {
                    let (variant, enum_segments) = segments.split_last().expect("two or more");
                    let names: Vec<&str> = enum_segments.iter().map(SyntaxToken::text).collect();
                    if let Ok(TypeExpr::Named(path)) =
                        self.named(&names, &[], expr.syntax().text_range())
                        && let Some(TypeDef::Enum { name, variants, .. }) = self.schema.get(&path)
                    {
                        let found = variants.iter().find(|(n, v)| {
                            *n == variant.text() || v.aliases.iter().any(|a| a == variant.text())
                        });
                        return match found {
                            Some((serde_name, _)) => Ok(TypeAt::Variant {
                                path,
                                name: serde_name.clone(),
                            }),
                            None => Err(TypeProblem::new(
                                variant.text_range(),
                                format!(
                                    "`{name}` has no variant `{}`; its variants are `{}`",
                                    variant.text(),
                                    variants
                                        .keys()
                                        .map(String::as_str)
                                        .collect::<Vec<_>>()
                                        .join("`, `")
                                ),
                            )),
                        };
                    }
                }
                Err(problem)
            }
        }
    }

    /// The type a type expression names.
    fn expr(&self, expr: &ast::TypeExpr) -> Result<TypeExpr, TypeProblem> {
        let range = expr.syntax().text_range();
        match expr.form() {
            TypeForm::Array => {
                let element = expr
                    .elements()
                    .next()
                    .ok_or_else(|| TypeProblem::new(range, "expected the type of the elements"))?;
                let element = self.expr(&element)?;
                let length = expr
                    .length()
                    .and_then(|t| parse_int(t.text()))
                    .and_then(|n| usize::try_from(n).ok())
                    .ok_or_else(|| TypeProblem::new(range, "expected an array length"))?;
                Ok(TypeExpr::Array(Box::new(element), length))
            }
            TypeForm::Tuple => {
                let items = expr
                    .elements()
                    .map(|e| self.expr(&e))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(match items.len() {
                    0 => TypeExpr::Unit,
                    1 if !expr.has_comma() => items.into_iter().next().expect("one"),
                    _ => TypeExpr::Tuple(items),
                })
            }
            TypeForm::Path => {
                let segments: Vec<SyntaxToken> = expr.segments().collect();
                let names: Vec<&str> = segments.iter().map(SyntaxToken::text).collect();
                let args = match expr.type_args() {
                    Some(list) => list
                        .args()
                        .map(|a| self.expr(&a))
                        .collect::<Result<Vec<_>, _>>()?,
                    None => Vec::new(),
                };
                if let Some(found) = builtin(&names, &args, range)? {
                    return Ok(found);
                }
                self.named(&names, &args, range)
            }
        }
    }

    /// A schema type, by its path or name and its type arguments.
    fn named(
        &self,
        names: &[&str],
        args: &[TypeExpr],
        range: TextRange,
    ) -> Result<TypeExpr, TypeProblem> {
        let written = names.join("::");
        let args_text = (!args.is_empty()).then(|| {
            let args: Vec<String> = args.iter().map(crate::schema::type_name).collect();
            format!("<{}>", args.join(", "))
        });
        let with_args = |base: &str| match &args_text {
            Some(args) => format!("{base}{args}"),
            None => base.to_owned(),
        };
        // A name a `use` gives.
        if let [name] = names
            && let Some(path) = self.uses.0.get(*name)
        {
            let path = match &args_text {
                // The arguments written replace those of the `use`.
                Some(_) => with_args(path.split('<').next().unwrap_or(path)),
                None => path.clone(),
            };
            return match self.schema.get(&path) {
                Some(_) => Ok(TypeExpr::Named(path)),
                None => Err(TypeProblem::new(
                    range,
                    format!("the schema has no type `{path}`"),
                )),
            };
        }
        if names.len() > 1 {
            let path = with_args(&written);
            return match self.schema.get(&path) {
                Some(_) => Ok(TypeExpr::Named(path)),
                None => Err(TypeProblem::new(
                    range,
                    format!("the schema has no type `{path}`"),
                )),
            };
        }
        let suffix = args_text.clone().unwrap_or_default();
        let found: Vec<&str> = self
            .schema
            .by_name(&written)
            .map(|(path, _)| path)
            .filter(|path| match path.find('<') {
                Some(at) => path[at..] == suffix,
                None => suffix.is_empty(),
            })
            .collect();
        match found.as_slice() {
            [path] => Ok(TypeExpr::Named((*path).to_owned())),
            [] => Err(TypeProblem::new(
                range,
                format!("the schema has no type `{}`", with_args(&written)),
            )),
            paths => Err(TypeProblem::new(
                range,
                format!(
                    "`{written}` names more than one type; `#![use(...)]` the one you mean: `{}`",
                    paths.join("`, `")
                ),
            )),
        }
    }

    /// One step further in.
    fn step(&self, at: TypeAt, step: &ast::TypeStep) -> Result<TypeAt, TypeProblem> {
        let range = step.syntax().text_range();
        let Some(kind) = step.kind() else {
            return Err(TypeProblem::new(
                range,
                "expected a field, an index or `[]` after `.`",
            ));
        };
        let found = match &at {
            TypeAt::Type { ty, .. } => step_type(self.schema, ty, &kind, 0),
            TypeAt::Variant { path, name } => match self.schema.get(path) {
                Some(TypeDef::Enum { variants, .. }) => variants
                    .get(name)
                    .and_then(|v| step_variant(self.schema, &v.shape, &kind)),
                _ => None,
            },
        };
        if let Some((ty, doc)) = found {
            return Ok(TypeAt::Type { ty, doc });
        }
        let what = match &at {
            TypeAt::Type { ty, .. } => format!("`{}`", rust_type(self.schema, ty)),
            TypeAt::Variant { name, .. } => format!("the variant `{name}`"),
        };
        let message = match kind {
            TypeStepKind::Field(name) => format!("{what} has no field `{}`", name.text()),
            TypeStepKind::Index(index) => format!("{what} has no element `.{}`", index.text()),
            TypeStepKind::Element => {
                format!("`.[]` goes into a list, set, array or map; {what} is none of them")
            }
        };
        Err(TypeProblem::new(range, message))
    }
}

/// A type with a Rust name (`SPEC.md` 6.1.2), or `None` for a schema type.
fn builtin(
    names: &[&str],
    args: &[TypeExpr],
    range: TextRange,
) -> Result<Option<TypeExpr>, TypeProblem> {
    let arity = |n: usize, name: &str| {
        if args.len() == n {
            Ok(())
        } else {
            let wanted = match n {
                0 => "no type arguments".to_owned(),
                1 => "one type argument".to_owned(),
                n => format!("{n} type arguments"),
            };
            Err(TypeProblem::new(range, format!("`{name}` takes {wanted}")))
        }
    };
    let arg = |i: usize| Box::new(args[i].clone());
    let scalar = |ty: TypeExpr, name: &str| arity(0, name).map(|()| Some(ty));
    match names {
        ["crol", "Id"] => scalar(TypeExpr::Id(Vec::new()), "crol::Id"),
        ["crol", "Value"] => scalar(TypeExpr::Any, "crol::Value"),
        [name] => {
            let name = *name;
            match name {
                "bool" => scalar(TypeExpr::Bool, name),
                "char" => scalar(TypeExpr::Char, name),
                "String" | "str" | "PathBuf" | "Path" => scalar(TypeExpr::String, name),
                "i8" => scalar(TypeExpr::I8, name),
                "i16" => scalar(TypeExpr::I16, name),
                "i32" => scalar(TypeExpr::I32, name),
                "i64" | "isize" => scalar(TypeExpr::I64, name),
                "i128" => scalar(TypeExpr::I128, name),
                "u8" => scalar(TypeExpr::U8, name),
                "u16" => scalar(TypeExpr::U16, name),
                "u32" => scalar(TypeExpr::U32, name),
                "u64" | "usize" => scalar(TypeExpr::U64, name),
                "u128" => scalar(TypeExpr::U128, name),
                "f32" => scalar(TypeExpr::F32, name),
                "f64" => scalar(TypeExpr::F64, name),
                "Option" => arity(1, name).map(|()| Some(TypeExpr::Option(arg(0)))),
                "Vec" | "VecDeque" => arity(1, name).map(|()| Some(TypeExpr::List(arg(0)))),
                "HashSet" | "BTreeSet" => arity(1, name).map(|()| Some(TypeExpr::Set(arg(0)))),
                "HashMap" | "BTreeMap" | "IndexMap" => {
                    arity(2, name).map(|()| Some(TypeExpr::Map(arg(0), arg(1))))
                }
                "Box" | "Rc" | "Arc" | "Cow" => arity(1, name).map(|()| Some(args[0].clone())),
                _ => Ok(None),
            }
        }
        _ => Ok(None),
    }
}

/// The type one step into `ty`, and the doc of the field stepped into.
fn step_type(
    schema: &SchemaFile,
    ty: &TypeExpr,
    kind: &TypeStepKind,
    depth: usize,
) -> Option<(TypeExpr, Option<String>)> {
    if depth > MAX_DEPTH {
        return None;
    }
    match (ty, kind) {
        // A path goes into a value written in place, never through an id.
        (TypeExpr::Option(inner) | TypeExpr::IdOr(_, inner), _) => {
            step_type(schema, inner, kind, depth + 1)
        }
        (TypeExpr::Named(path), _) => match schema.get(path)? {
            TypeDef::Struct { fields, .. } => step_fields(schema, fields, kind, depth),
            TypeDef::TupleStruct { fields, .. } => match kind {
                TypeStepKind::Index(i) => {
                    let i: usize = i.text().parse().ok()?;
                    fields.get(i).map(|t| (t.clone(), None))
                }
                _ => None,
            },
            TypeDef::Newtype { inner, .. } | TypeDef::Transparent { inner, .. } => {
                step_type(schema, inner, kind, depth + 1)
            }
            _ => None,
        },
        (
            TypeExpr::List(item) | TypeExpr::Set(item) | TypeExpr::Array(item, _),
            TypeStepKind::Element,
        ) => Some(((**item).clone(), None)),
        (TypeExpr::Map(_, value), TypeStepKind::Element) => Some(((**value).clone(), None)),
        (TypeExpr::Tuple(items), TypeStepKind::Index(i)) => {
            let i: usize = i.text().parse().ok()?;
            items.get(i).map(|t| (t.clone(), None))
        }
        _ => None,
    }
}

/// A field of a struct by its name or an alias, in a flattened field if not
/// its own.
fn step_fields(
    schema: &SchemaFile,
    fields: &IndexMap<String, Field>,
    kind: &TypeStepKind,
    depth: usize,
) -> Option<(TypeExpr, Option<String>)> {
    let TypeStepKind::Field(name) = kind else {
        return None;
    };
    if let Some(found) = field(fields, name.text()).filter(|f| !f.flatten) {
        let doc = (!found.doc.trim().is_empty()).then(|| found.doc.trim().to_owned());
        return Some((found.ty.clone(), doc));
    }
    fields
        .values()
        .filter(|f| f.flatten)
        .find_map(|f| step_type(schema, &f.ty, kind, depth + 1))
}

fn step_variant(
    schema: &SchemaFile,
    shape: &VariantShape,
    kind: &TypeStepKind,
) -> Option<(TypeExpr, Option<String>)> {
    match (shape, kind) {
        (VariantShape::Struct(fields), _) => step_fields(schema, fields, kind, 0),
        (VariantShape::Tuple(items), TypeStepKind::Index(i)) => {
            let i: usize = i.text().parse().ok()?;
            items.get(i).map(|t| (t.clone(), None))
        }
        (VariantShape::Newtype(inner), _) => step_type(schema, inner, kind, 0),
        _ => None,
    }
}

fn field<'a>(fields: &'a IndexMap<String, Field>, name: &str) -> Option<&'a Field> {
    fields.get(name).or_else(|| {
        fields
            .values()
            .find(|f| f.aliases.iter().any(|a| a == name))
    })
}

fn offers(schema: &SchemaFile, ty: &TypeExpr, out: &mut Vec<TypeStepOffer>, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    match ty {
        TypeExpr::Option(inner) | TypeExpr::IdOr(_, inner) => offers(schema, inner, out, depth + 1),
        TypeExpr::Named(path) => match schema.get(path) {
            Some(TypeDef::Struct { fields, .. }) => field_offers(schema, fields, out, depth),
            Some(TypeDef::TupleStruct { fields, .. }) => index_offers(fields, out),
            Some(TypeDef::Newtype { inner, .. } | TypeDef::Transparent { inner, .. }) => {
                offers(schema, inner, out, depth + 1);
            }
            _ => {}
        },
        TypeExpr::List(item) | TypeExpr::Set(item) | TypeExpr::Array(item, _) => {
            out.push(TypeStepOffer {
                label: "[]".to_owned(),
                ty: (**item).clone(),
                doc: None,
            });
        }
        TypeExpr::Map(_, value) => out.push(TypeStepOffer {
            label: "[]".to_owned(),
            ty: (**value).clone(),
            doc: None,
        }),
        TypeExpr::Tuple(items) => index_offers(items, out),
        _ => {}
    }
}

fn field_offers(
    schema: &SchemaFile,
    fields: &IndexMap<String, Field>,
    out: &mut Vec<TypeStepOffer>,
    depth: usize,
) {
    for (name, f) in fields {
        if f.flatten {
            offers(schema, &f.ty, out, depth + 1);
        } else {
            out.push(TypeStepOffer {
                label: name.clone(),
                ty: f.ty.clone(),
                doc: (!f.doc.trim().is_empty()).then(|| f.doc.trim().to_owned()),
            });
        }
    }
}

fn index_offers(items: &[TypeExpr], out: &mut Vec<TypeStepOffer>) {
    for (i, ty) in items.iter().enumerate() {
        out.push(TypeStepOffer {
            label: i.to_string(),
            ty: ty.clone(),
            doc: None,
        });
    }
}

fn variant_offers(schema: &SchemaFile, shape: &VariantShape, out: &mut Vec<TypeStepOffer>) {
    match shape {
        VariantShape::Struct(fields) => field_offers(schema, fields, out, 0),
        VariantShape::Tuple(items) => index_offers(items, out),
        VariantShape::Newtype(inner) => offers(schema, inner, out, 0),
        VariantShape::Unit => {}
    }
}

/// An integer literal's value, without a sign: `12`, `0x1f`, `1_000`.
fn parse_int(text: &str) -> Option<i128> {
    let text = text.replace('_', "");
    let (digits, radix) = match text.get(..2) {
        Some("0x") => (&text[2..], 16),
        Some("0o") => (&text[2..], 8),
        Some("0b") => (&text[2..], 2),
        _ => (&text[..], 10),
    };
    i128::from_str_radix(digits, radix).ok()
}

// ---- Checking values ----

struct ValueChecker<'s> {
    schema: &'s SchemaFile,
    options: &'s CheckOptions,
    problems: Vec<TypeProblem>,
    /// What names the type of the value checked next: the newtypes and
    /// transparent structs it was reached through (`SPEC.md` 10.2).
    names: TypeNames<'s>,
}

/// What an expression is, as far as its text says.
enum Written {
    Int(Option<i128>),
    Float,
    Bool,
    String,
    Char,
    Id,
    Unit,
    Block(ast::Block),
    List(ast::ListExpr),
    Tuple(ast::TupleExpr),
    NamedTuple(ast::NamedTuple),
    UnitName(String),
    /// Computed, or not known from the text.
    Unknown,
}

fn written(expr: &ast::Expr) -> Written {
    match expr {
        ast::Expr::Literal(literal) => match literal.token().map(|t| (t.kind(), t)) {
            Some((INT, t)) => Written::Int(parse_int(t.text())),
            Some((FLOAT, _)) => Written::Float,
            Some((STRING | RAW_STRING, _)) => Written::String,
            Some((CHAR, _)) => Written::Char,
            Some((TRUE_KW | FALSE_KW, _)) => Written::Bool,
            _ => Written::Unknown,
        },
        ast::Expr::Id(_) => Written::Id,
        ast::Expr::Block(block) => Written::Block(block.clone()),
        ast::Expr::List(list) => Written::List(list.clone()),
        ast::Expr::Tuple(tuple) if tuple.elements().next().is_none() => Written::Unit,
        ast::Expr::Tuple(tuple) => Written::Tuple(tuple.clone()),
        ast::Expr::NamedTuple(tuple) => Written::NamedTuple(tuple.clone()),
        ast::Expr::UnitName(name) => name
            .name()
            .map_or(Written::Unknown, |n| Written::UnitName(n.text().to_owned())),
        ast::Expr::Paren(paren) => paren.inner().map_or(Written::Unknown, |e| written(&e)),
        // `-1`, `-1.5`
        ast::Expr::Prefix(prefix) => {
            let minus = prefix
                .syntax()
                .first_token()
                .is_some_and(|t| t.kind() == MINUS);
            match prefix.syntax().children().find_map(ast::Expr::cast) {
                Some(operand) if minus => match written(&operand) {
                    Written::Int(i) => Written::Int(i.map(|i| -i)),
                    Written::Float => Written::Float,
                    _ => Written::Unknown,
                },
                _ => Written::Unknown,
            }
        }
        _ => Written::Unknown,
    }
}

fn found(w: &Written, expr: &ast::Expr) -> String {
    match w {
        Written::UnitName(n) => format!("`{n}`"),
        Written::NamedTuple(t) => format!(
            "`{}(...)`",
            t.name().map(|n| n.text().to_owned()).unwrap_or_default()
        ),
        Written::Block(b) if b.type_name().is_some() => format!(
            "`{} {{ ... }}`",
            b.type_name()
                .map(|n| n.text().to_owned())
                .unwrap_or_default()
        ),
        Written::Block(_) => "a keyed value".to_owned(),
        Written::List(_) => "a list".to_owned(),
        Written::Tuple(t) => format!("a tuple of {}", t.elements().count()),
        _ => expr.syntax().text().to_string().trim().to_owned(),
    }
}

impl<'s> ValueChecker<'s> {
    fn expected(&mut self, expr: &ast::Expr, w: &Written, what: &str) {
        self.problems.push(TypeProblem::new(
            expr.syntax().text_range(),
            format!("expected {what}, found {}", found(w, expr)),
        ));
    }

    fn value(&mut self, expr: &ast::Expr, ty: &TypeExpr, depth: usize) {
        // They name this value only.
        let names = std::mem::take(&mut self.names);
        if depth > MAX_DEPTH {
            return;
        }
        let w = written(expr);
        if matches!(w, Written::Unknown) {
            return;
        }
        if let ast::Expr::Paren(paren) = expr
            && let Some(inner) = paren.inner()
        {
            self.names = names;
            return self.value(&inner, ty, depth + 1);
        }
        // An integer too long for an i128 is out of every range.
        let int = |min: i128, max: i128| matches!(w, Written::Int(Some(i)) if i >= min && i <= max);
        let ok = match ty {
            TypeExpr::Any | TypeExpr::Bytes => true,
            TypeExpr::Bool => matches!(w, Written::Bool),
            TypeExpr::Char => matches!(w, Written::Char),
            TypeExpr::String => matches!(w, Written::String | Written::Id),
            TypeExpr::Unit => matches!(w, Written::Unit),
            TypeExpr::I8 => int(i8::MIN.into(), i8::MAX.into()),
            TypeExpr::I16 => int(i16::MIN.into(), i16::MAX.into()),
            TypeExpr::I32 => int(i32::MIN.into(), i32::MAX.into()),
            TypeExpr::I64 => int(i64::MIN.into(), i64::MAX.into()),
            TypeExpr::I128 => int(i128::MIN, i128::MAX),
            TypeExpr::U8 => int(0, u8::MAX.into()),
            TypeExpr::U16 => int(0, u16::MAX.into()),
            TypeExpr::U32 => int(0, u32::MAX.into()),
            TypeExpr::U64 => int(0, u64::MAX.into()),
            TypeExpr::U128 => int(0, i128::MAX),
            TypeExpr::F32 | TypeExpr::F64 => matches!(w, Written::Float | Written::Int(_)),
            TypeExpr::Id(_) => matches!(w, Written::Id),
            // An id fits; anything else is the value written in place.
            TypeExpr::IdOr(_, inner) => {
                if !matches!(w, Written::Id) {
                    self.names = names;
                    self.value(expr, inner, depth + 1);
                }
                true
            }
            TypeExpr::Option(inner) => {
                match &w {
                    Written::UnitName(n) if n == "None" => {}
                    Written::NamedTuple(t) if t.name().is_some_and(|n| n.text() == "Some") => {
                        let items: Vec<ast::Expr> = t.elements().collect();
                        if let [item] = items.as_slice() {
                            self.value(item, inner, depth + 1);
                        }
                    }
                    // A value with no `Some`, which `implicit_some` takes.
                    _ => self.value(expr, inner, depth + 1),
                }
                true
            }
            TypeExpr::List(item) | TypeExpr::Set(item) => match &w {
                Written::List(list) => {
                    for element in list.elements() {
                        self.value(&element, item, depth + 1);
                    }
                    true
                }
                _ => false,
            },
            TypeExpr::Array(item, _) => match &w {
                Written::List(list) => {
                    for element in list.elements() {
                        self.value(&element, item, depth + 1);
                    }
                    true
                }
                Written::Tuple(tuple) => {
                    for element in tuple.elements() {
                        self.value(&element, item, depth + 1);
                    }
                    true
                }
                _ => false,
            },
            TypeExpr::Tuple(items) => match &w {
                Written::Tuple(tuple) => {
                    let elements: Vec<ast::Expr> = tuple.elements().collect();
                    if elements.len() == items.len() {
                        for (element, ty) in elements.iter().zip(items) {
                            self.value(element, ty, depth + 1);
                        }
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            },
            TypeExpr::Map(_, value) => match &w {
                Written::Block(block) => {
                    // A map has the names of what wraps it; a name that
                    // names nothing is a warning where the value is used
                    // (`SPEC.md` 10.2).
                    if let Some(written) = block.type_name()
                        && let NameFit::Mismatch(message) = names.of_map(written.text())
                    {
                        let range = written.text_range();
                        self.problems.push(TypeProblem::new(range, message));
                        return;
                    }
                    self.map_entries(block.entries(), value, depth);
                    true
                }
                _ => false,
            },
            TypeExpr::Named(path) => return self.named(expr, &w, path, depth, names),
        };
        if !ok {
            self.expected(expr, &w, &crate::check::describe(ty));
        }
    }

    fn map_entries(
        &mut self,
        entries: impl Iterator<Item = ast::AnyEntry>,
        value: &TypeExpr,
        depth: usize,
    ) {
        for entry in entries {
            match entry {
                ast::AnyEntry::Entry(entry) => {
                    let plain = entry
                        .target()
                        .is_some_and(|t| t.selectors().next().is_none());
                    if plain && let Some(v) = entry.value() {
                        self.value(&v, value, depth + 1);
                    }
                }
                ast::AnyEntry::VariantBlock(block) => {
                    let entries = block.syntax().children().filter_map(ast::AnyEntry::cast);
                    self.map_entries(entries, value, depth + 1);
                }
                _ => {}
            }
        }
    }

    fn named(
        &mut self,
        expr: &ast::Expr,
        w: &Written,
        path: &str,
        depth: usize,
        names: TypeNames<'s>,
    ) {
        let Some(def) = self.schema.get(path) else {
            return;
        };
        // The value itself in a newtype or transparent struct: a type name
        // on it may be the wrapper's (`SPEC.md` 10.2).
        let block_name = match w {
            Written::Block(block) => block.type_name(),
            _ => None,
        };
        let block_name = block_name.as_ref().map(SyntaxToken::text);
        match def {
            TypeDef::Transparent { name, inner, .. } => {
                self.names = names.wrap(name, block_name);
                self.value(expr, inner, depth + 1);
            }
            TypeDef::Newtype { name, inner, .. } => match w {
                // `N(x)`, or else `x` (`SPEC.md` 10.2).
                Written::NamedTuple(t) if t.name().is_some_and(|n| n.text() == name) => {
                    let items: Vec<ast::Expr> = t.elements().collect();
                    if let [item] = items.as_slice() {
                        self.value(item, inner, depth + 1);
                    }
                }
                _ => {
                    self.names = names.wrap(name, block_name);
                    self.value(expr, inner, depth + 1);
                }
            },
            TypeDef::Struct { name, fields, .. } => match w {
                Written::Block(block) => {
                    if let Some(written) = block.type_name()
                        && let NameFit::Mismatch(message) = names.of_struct(name, written.text())
                    {
                        let range = written.text_range();
                        self.problems.push(TypeProblem::new(range, message));
                        return;
                    }
                    self.fields(block.entries(), name, fields, depth);
                }
                _ => self.expected(expr, w, &format!("`{name} {{ ... }}`")),
            },
            TypeDef::TupleStruct { name, fields, .. } => match w {
                Written::NamedTuple(t) if t.name().is_some_and(|n| n.text() == name) => {
                    let items: Vec<ast::Expr> = t.elements().collect();
                    if items.len() == fields.len() {
                        for (item, ty) in items.iter().zip(fields) {
                            self.value(item, ty, depth + 1);
                        }
                    } else {
                        let what = format!("`{name}(...)` with {} elements", fields.len());
                        self.expected(expr, w, &what);
                    }
                }
                _ => self.expected(expr, w, &format!("`{name}(...)`")),
            },
            TypeDef::UnitStruct { name, .. } => match w {
                Written::UnitName(n) if n == name => {}
                _ => self.expected(expr, w, &format!("`{name}`")),
            },
            TypeDef::Enum {
                name,
                repr,
                variants,
                ..
            } => {
                // Only the external form says the variant by its name.
                if *repr != Repr::External {
                    return;
                }
                let variant_name = match w {
                    Written::UnitName(n) => n.clone(),
                    Written::NamedTuple(t) => match t.name() {
                        Some(n) => n.text().to_owned(),
                        None => return,
                    },
                    Written::Block(b) => match b.type_name() {
                        Some(n) => n.text().to_owned(),
                        None => return,
                    },
                    _ => return self.expected(expr, w, &format!("a `{name}` variant")),
                };
                let Some((serde_name, variant)) = variants
                    .iter()
                    .find(|(n, v)| **n == variant_name || v.aliases.contains(&variant_name))
                else {
                    let names: Vec<&str> = variants.keys().map(String::as_str).collect();
                    self.problems.push(TypeProblem::new(
                        expr.syntax().text_range(),
                        format!(
                            "`{name}` has no variant `{variant_name}`; its variants are `{}`",
                            names.join("`, `")
                        ),
                    ));
                    return;
                };
                match (&variant.shape, w) {
                    (VariantShape::Unit, Written::UnitName(_)) => {}
                    (VariantShape::Newtype(inner), Written::NamedTuple(t)) => {
                        let items: Vec<ast::Expr> = t.elements().collect();
                        if let [item] = items.as_slice() {
                            self.value(item, inner, depth + 1);
                        }
                    }
                    // `V { ... }` as a newtype variant (`SPEC.md` 10.4).
                    (VariantShape::Newtype(_), Written::Block(_)) => {}
                    (VariantShape::Tuple(types), Written::NamedTuple(t))
                        if t.elements().count() == types.len() =>
                    {
                        for (item, ty) in t.elements().zip(types) {
                            self.value(&item, ty, depth + 1);
                        }
                    }
                    (VariantShape::Struct(fields), Written::Block(block)) => {
                        self.fields(block.entries(), serde_name, fields, depth);
                    }
                    _ => self.expected(
                        expr,
                        w,
                        &format!("the variant `{serde_name}` of `{name}` in its shape"),
                    ),
                }
            }
        }
    }

    /// The entries of a struct's block: known fields, of the right kind.
    fn fields(
        &mut self,
        entries: impl Iterator<Item = ast::AnyEntry>,
        name: &str,
        fields: &IndexMap<String, Field>,
        depth: usize,
    ) {
        let flattened = fields.values().any(|f| f.flatten);
        for entry in entries {
            match entry {
                ast::AnyEntry::Entry(entry) => {
                    let Some(target) = entry.target() else {
                        continue;
                    };
                    let Some(key) = target.key().and_then(|k| k.token()) else {
                        continue;
                    };
                    let key_name = match key.kind() {
                        IDENT => key.text().to_owned(),
                        // A plain string; one with escapes is left alone.
                        STRING if !key.text().contains('\\') => {
                            key.text().trim_matches('"').to_owned()
                        }
                        _ => continue,
                    };
                    match field(fields, &key_name) {
                        Some(found) => {
                            if target.selectors().next().is_none()
                                && let Some(value) = entry.value()
                            {
                                self.value(&value, &found.ty, depth + 1);
                            }
                        }
                        None if flattened || self.options.allow_unknown_fields => {}
                        None => {
                            let names: Vec<&str> = fields
                                .iter()
                                .flat_map(|(n, f)| {
                                    f.aliases.iter().map(String::as_str).chain([n.as_str()])
                                })
                                .collect();
                            self.problems.push(TypeProblem::new(
                                key.text_range(),
                                format!(
                                    "`{name}` has no field `{key_name}`; its fields are `{}`",
                                    names.join("`, `")
                                ),
                            ));
                        }
                    }
                }
                ast::AnyEntry::VariantBlock(block) => {
                    let entries = block.syntax().children().filter_map(ast::AnyEntry::cast);
                    self.fields(entries, name, fields, depth + 1);
                }
                _ => {}
            }
        }
    }
}

// ---- Where typed templates are used ----

/// A template with a `#[type(...)]`: its type, and where the attribute is.
#[derive(Debug, Clone)]
pub struct TypedTemplate {
    /// The type the attribute names.
    pub ty: TypeExpr,
    /// The attribute, for messages.
    pub span: Span,
}

/// The templates with a type among the top-level value binds of
/// `documents`, by name. A type that names nothing gives no entry.
pub fn typed_templates<'d>(
    documents: impl IntoIterator<Item = (FileId, &'d ast::Document)>,
    schema: &SchemaFile,
) -> HashMap<String, TypedTemplate> {
    let mut found = HashMap::new();
    for (file, document) in documents {
        let uses = TypeUses::of(document, schema);
        for entry in document.entries() {
            let ast::AnyEntry::ValueBind(bind) = entry else {
                continue;
            };
            let (Some(name), Some(attr)) = (bind.name(), bind.attr()) else {
                continue;
            };
            let Some(Ok((ty, _))) = attr.type_ref().map(|t| resolve(schema, &uses, &t)) else {
                continue;
            };
            let span = Span::new(file, attr.syntax().text_range());
            found.insert(name.text().to_owned(), TypedTemplate { ty, span });
        }
    }
    found
}

/// What the places of a document are found from.
#[derive(Debug, Clone, Copy)]
pub enum TypeRoot<'v> {
    /// A value-mode document, of the type with this path.
    Value(&'v str),
    /// Data, with its root type (`None`: the schema's), and the evaluated
    /// root, which says the variant of each root value where the root type
    /// is an enum (`SPEC.md` 4.1).
    Data {
        /// The root type's path, if not the schema's.
        root: Option<&'v str>,
        /// The evaluated root, in the base context.
        value: Option<&'v crol_eval::Value>,
    },
}

/// Warns where `document` uses a template of `templates` as a whole value at
/// a place of another type: spread into a block, merged, or set as a value
/// (`SPEC.md` 6.1.2). Places the schema does not know the type of are left
/// alone.
pub fn check_template_uses(
    file: FileId,
    document: &ast::Document,
    schema: &SchemaFile,
    templates: &HashMap<String, TypedTemplate>,
    root: TypeRoot<'_>,
) -> Vec<Diagnostic> {
    if templates.is_empty() {
        return Vec::new();
    }
    let uses = TypeUses::of(document, schema);
    let places = Places {
        schema,
        uses: &uses,
        root,
    };
    let mut out = Vec::new();
    for reference in document
        .syntax()
        .descendants()
        .filter_map(ast::Reference::cast)
    {
        let ast::ReferenceKind::Bind(name) = reference.kind() else {
            continue;
        };
        let Some(template) = templates.get(name.text()) else {
            continue;
        };
        let Some(Pos::Type(place)) = places.of(reference.syntax()) else {
            continue;
        };
        if same_type(schema, &template.ty, &place) {
            continue;
        }
        let message = format!(
            "the template `{}` has the type `{}`, but it is used where `{}` goes",
            name.text(),
            rust_type(schema, &template.ty),
            rust_type(schema, &place),
        );
        let span = Span::new(file, reference.syntax().text_range());
        out.push(Diagnostic::warning(span, message).with_related(template.span, "its type"));
    }
    out
}

/// What a place in a document holds.
#[derive(Debug, Clone)]
enum Pos<'s> {
    /// The root keys of data whose root type is that of each root value.
    Keys,
    /// A value of a type.
    Type(TypeExpr),
    /// The fields of a struct variant.
    Fields(&'s IndexMap<String, Field>),
}

/// A step into a value, as data writes it.
enum DataStep {
    /// `.name`, a key, or `["name"]`.
    Field(String),
    /// `.0`
    Index(usize),
    /// An element of a list, or an integer key.
    Element,
}

struct Places<'s> {
    schema: &'s SchemaFile,
    uses: &'s TypeUses,
    root: TypeRoot<'s>,
}

impl<'s> Places<'s> {
    /// The place of the value `node` is, from the document down.
    fn of(&self, node: &crol_syntax::SyntaxNode) -> Option<Pos<'s>> {
        let chain: Vec<crol_syntax::SyntaxNode> = node.ancestors().collect();
        let mut pos = None;
        // From the document down: each node, and its child on the way.
        for pair in chain.windows(2).rev() {
            let (child, parent) = (&pair[0], &pair[1]);
            pos = Some(self.child_place(pos, parent, child)?);
        }
        pos
    }

    fn child_place(
        &self,
        pos: Option<Pos<'s>>,
        parent: &crol_syntax::SyntaxNode,
        child: &crol_syntax::SyntaxNode,
    ) -> Option<Pos<'s>> {
        use crol_syntax::SyntaxKind as K;
        match parent.kind() {
            K::DOCUMENT => Some(match self.root {
                TypeRoot::Value(path) => Pos::Type(TypeExpr::Named(path.to_owned())),
                TypeRoot::Data { root, .. } => match self.schema.data_root(root) {
                    crate::schema::DataRoot::Document => Pos::Type(TypeExpr::Named(
                        root.unwrap_or(&self.schema.root).to_owned(),
                    )),
                    crate::schema::DataRoot::EachValue => Pos::Keys,
                },
            }),
            K::ENTRY => {
                if child.kind() == K::TARGET {
                    return None;
                }
                let target = ast::Entry::cast(parent.clone())?.target()?;
                let mut pos = pos?;
                let key = target.key()?;
                let first = key_step(&key)?;
                if let Pos::Keys = pos {
                    let DataStep::Field(name) = first else {
                        return None;
                    };
                    pos = self.root_value(&name)?;
                } else {
                    pos = self.step(pos, &first)?;
                }
                for selector in target.selectors() {
                    if let Some(step) = selector_step(&selector) {
                        pos = self.step(pos, &step)?;
                    }
                }
                Some(pos)
            }
            K::BLOCK => {
                let pos = pos?;
                match ast::Block::cast(parent.clone())?.type_name() {
                    Some(name) => self.named(pos, name.text()),
                    None => Some(pos),
                }
            }
            K::VARIANT_BLOCK | K::PAREN_EXPR => pos,
            // The operand of a spread is at the place of the block.
            K::SPREAD => pos,
            K::BINARY_EXPR => match ast::BinaryExpr::cast(parent.clone())?.op()? {
                ast::BinaryOp::Merge | ast::BinaryOp::MergeShallow | ast::BinaryOp::Fallback => pos,
                _ => None,
            },
            K::LIST_EXPR => self.step(pos?, &DataStep::Element),
            K::TUPLE_EXPR => {
                let index = parent
                    .children()
                    .filter(|c| ast::Expr::can_cast(c.kind()))
                    .position(|c| c == *child)?;
                self.step(pos?, &DataStep::Index(index))
            }
            K::NAMED_TUPLE => pos,
            K::ARG_LIST => {
                let tuple = ast::NamedTuple::cast(parent.parent()?)?;
                let name = tuple.name()?;
                let index = parent
                    .children()
                    .filter(|c| ast::Expr::can_cast(c.kind()))
                    .position(|c| c == *child)?;
                let Pos::Type(ty) = pos? else { return None };
                payload(self.schema, &ty, name.text(), index, 0).map(Pos::Type)
            }
            // Inside a template: its own type, if it has one.
            K::VALUE_BIND => {
                let bind = ast::ValueBind::cast(parent.clone())?;
                if child.kind() == K::ENTRY_ATTR {
                    return None;
                }
                let type_ref = bind.attr()?.type_ref()?;
                let (ty, _) = resolve(self.schema, self.uses, &type_ref).ok()?;
                Some(Pos::Type(ty))
            }
            _ => None,
        }
    }

    /// The type of the root value `key`: the root type, or for an enum, the
    /// variant its evaluated value names.
    fn root_value(&self, key: &str) -> Option<Pos<'s>> {
        let TypeRoot::Data { root, value } = self.root else {
            return None;
        };
        let path = root.unwrap_or(&self.schema.root);
        let ty = TypeExpr::Named(path.to_owned());
        match self.schema.get(path)? {
            TypeDef::Enum { .. } => {
                let value = value?;
                let found =
                    crol_eval::value_at_path(value, &[crol_eval::PathStep::Field(key.to_owned())])?;
                let name = match &found.kind {
                    crol_eval::ValueKind::Keyed(keyed) => keyed.name.as_deref()?,
                    crol_eval::ValueKind::NamedTuple(name, _)
                    | crol_eval::ValueKind::UnitName(name) => name,
                    _ => return None,
                };
                self.named(Pos::Type(ty), name)
            }
            _ => Some(Pos::Type(ty)),
        }
    }

    /// `Name { ... }` at a place: the variant of that name, or the type.
    fn named(&self, pos: Pos<'s>, name: &str) -> Option<Pos<'s>> {
        let Pos::Type(ty) = pos else {
            return self
                .schema
                .type_path(name)
                .map(|p| Pos::Type(TypeExpr::Named(p.to_owned())));
        };
        named_in(self.schema, &ty, name, 0).or_else(|| {
            self.schema
                .type_path(name)
                .map(|p| Pos::Type(TypeExpr::Named(p.to_owned())))
        })
    }

    fn step(&self, pos: Pos<'s>, step: &DataStep) -> Option<Pos<'s>> {
        match pos {
            Pos::Keys => None,
            Pos::Fields(fields) => match step {
                DataStep::Field(name) => data_fields(self.schema, fields, name, 0).map(Pos::Type),
                _ => None,
            },
            Pos::Type(ty) => data_step(self.schema, &ty, step, 0).map(Pos::Type),
        }
    }
}

fn key_step(key: &ast::Key) -> Option<DataStep> {
    if let Some(accessor) = key.accessor() {
        return accessor_step(&accessor);
    }
    let token = key.token()?;
    Some(match token.kind() {
        IDENT => DataStep::Field(token.text().to_owned()),
        STRING if !token.text().contains('\\') => {
            DataStep::Field(token.text().trim_matches('"').to_owned())
        }
        INT => DataStep::Element,
        _ => return None,
    })
}

fn accessor_step(accessor: &ast::Accessor) -> Option<DataStep> {
    let key = accessor.key()?;
    Some(match key.kind() {
        INT => DataStep::Element,
        STRING if !key.text().contains('\\') => {
            DataStep::Field(key.text().trim_matches('"').to_owned())
        }
        _ => return None,
    })
}

/// A selector's step; `None` for a variant, which stays at the place.
fn selector_step(selector: &ast::Selector) -> Option<DataStep> {
    match selector {
        ast::Selector::Field(field) => Some(DataStep::Field(field.name()?.text().to_owned())),
        ast::Selector::Index(index) => Some(DataStep::Index(index.index()?.text().parse().ok()?)),
        ast::Selector::Accessor(accessor) => accessor_step(accessor),
        _ => None,
    }
}

/// The type one step into a value of `ty`, as data writes the step.
fn data_step(
    schema: &SchemaFile,
    ty: &TypeExpr,
    step: &DataStep,
    depth: usize,
) -> Option<TypeExpr> {
    if depth > MAX_DEPTH {
        return None;
    }
    match (ty, step) {
        (TypeExpr::Option(inner) | TypeExpr::IdOr(_, inner), _) => {
            data_step(schema, inner, step, depth + 1)
        }
        (TypeExpr::Named(path), _) => match schema.get(path)? {
            TypeDef::Struct { fields, .. } => match step {
                DataStep::Field(name) => data_fields(schema, fields, name, depth),
                _ => None,
            },
            TypeDef::TupleStruct { fields, .. } => match step {
                DataStep::Index(i) => fields.get(*i).cloned(),
                _ => None,
            },
            TypeDef::Newtype { inner, .. } | TypeDef::Transparent { inner, .. } => {
                data_step(schema, inner, step, depth + 1)
            }
            _ => None,
        },
        (TypeExpr::Map(_, value), DataStep::Field(_) | DataStep::Element) => {
            Some((**value).clone())
        }
        (
            TypeExpr::List(item) | TypeExpr::Set(item) | TypeExpr::Array(item, _),
            DataStep::Element,
        ) => Some((**item).clone()),
        (TypeExpr::Tuple(items), DataStep::Index(i)) => items.get(*i).cloned(),
        _ => None,
    }
}

fn data_fields(
    schema: &SchemaFile,
    fields: &IndexMap<String, Field>,
    name: &str,
    depth: usize,
) -> Option<TypeExpr> {
    if let Some(found) = field(fields, name).filter(|f| !f.flatten) {
        return Some(found.ty.clone());
    }
    fields
        .values()
        .filter(|f| f.flatten)
        .find_map(|f| data_step(schema, &f.ty, &DataStep::Field(name.to_owned()), depth + 1))
}

/// `Name { ... }` in a place of the type `ty`: its variant, or the type
/// itself.
fn named_in<'s>(
    schema: &'s SchemaFile,
    ty: &TypeExpr,
    name: &str,
    depth: usize,
) -> Option<Pos<'s>> {
    if depth > MAX_DEPTH {
        return None;
    }
    match ty {
        TypeExpr::Option(inner) | TypeExpr::IdOr(_, inner) => {
            named_in(schema, inner, name, depth + 1)
        }
        TypeExpr::Named(path) => match schema.get(path)? {
            TypeDef::Enum { variants, .. } => {
                let variant = variants.get(name).or_else(|| {
                    variants
                        .values()
                        .find(|v| v.aliases.iter().any(|a| a == name))
                })?;
                match &variant.shape {
                    VariantShape::Struct(fields) => Some(Pos::Fields(fields)),
                    VariantShape::Newtype(inner) => Some(Pos::Type(inner.clone())),
                    _ => None,
                }
            }
            def if def.name() == name => Some(Pos::Type(ty.clone())),
            TypeDef::Newtype { inner, .. } | TypeDef::Transparent { inner, .. } => {
                named_in(schema, inner, name, depth + 1)
            }
            _ => None,
        },
        _ => None,
    }
}

/// The type of argument `index` of `Name(...)` in a place of the type `ty`.
fn payload(
    schema: &SchemaFile,
    ty: &TypeExpr,
    name: &str,
    index: usize,
    depth: usize,
) -> Option<TypeExpr> {
    if depth > MAX_DEPTH {
        return None;
    }
    match ty {
        TypeExpr::Option(inner) if name == "Some" => (index == 0).then(|| (**inner).clone()),
        TypeExpr::Option(inner) | TypeExpr::IdOr(_, inner) => {
            payload(schema, inner, name, index, depth + 1)
        }
        TypeExpr::Named(path) => match schema.get(path)? {
            TypeDef::Enum { variants, .. } => match &variants.get(name)?.shape {
                VariantShape::Newtype(inner) if index == 0 => Some(inner.clone()),
                VariantShape::Tuple(items) => items.get(index).cloned(),
                _ => None,
            },
            TypeDef::Newtype {
                name: own, inner, ..
            } if own == name => (index == 0).then(|| inner.clone()),
            TypeDef::TupleStruct {
                name: own, fields, ..
            } if own == name => fields.get(index).cloned(),
            TypeDef::Newtype { inner, .. } | TypeDef::Transparent { inner, .. } => {
                payload(schema, inner, name, index, depth + 1)
            }
            _ => None,
        },
        _ => None,
    }
}

/// Whether a template of type `template` fits a place of type `place`: the
/// same type, through options and what data writes the same way as what it
/// wraps.
fn same_type(schema: &SchemaFile, template: &TypeExpr, place: &TypeExpr) -> bool {
    let (template, place) = (plain(schema, template), plain(schema, place));
    template == place || *template == TypeExpr::Any || *place == TypeExpr::Any
}

/// A type without the options and wrappers around it.
fn plain<'a>(schema: &'a SchemaFile, mut ty: &'a TypeExpr) -> &'a TypeExpr {
    for _ in 0..MAX_DEPTH {
        ty = match ty {
            // A template is a value, never an id.
            TypeExpr::Option(inner) | TypeExpr::IdOr(_, inner) => inner,
            TypeExpr::Named(path) => match schema.get(path) {
                Some(TypeDef::Newtype { inner, .. } | TypeDef::Transparent { inner, .. }) => inner,
                _ => break,
            },
            _ => break,
        };
    }
    ty
}
