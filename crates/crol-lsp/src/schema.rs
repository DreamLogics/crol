//! What the schema says about a place in the data (`SPEC.md` 10.9, 12.5).

use crol::schema::{Field, SchemaFile, TypeDef, TypeExpr, VariantShape};
use crol_eval::{FileId, PathStep};
use crol_syntax::ast::{self, AstNode};
use crol_syntax::{SyntaxKind, SyntaxNode};
use indexmap::IndexMap;

use crate::place::{self, Scope};
use crate::workspace::{DocumentSchema, Snapshot};

/// How deep to follow types into each other where a type may contain
/// itself.
const MAX_DEPTH: usize = 8;

/// What a place holds, by the schema.
#[derive(Clone)]
pub(crate) enum Shape<'a> {
    /// The root keys of data, each a value of the root type.
    Keys,
    /// A value of a type.
    Type(TypeExpr),
    /// The fields of a struct variant, which has no type of its own.
    Fields(&'a IndexMap<String, Field>),
}

/// The root type of a document.
pub(crate) fn root_expr(schema: &DocumentSchema<'_>) -> TypeExpr {
    TypeExpr::Named(schema.root_type().to_owned())
}

fn document_text(node: &SyntaxNode) -> String {
    node.ancestors()
        .last()
        .unwrap_or_else(|| node.clone())
        .text()
        .to_string()
}

/// The fields of a block, by the place it is at (`place`).
pub(crate) fn block_fields<'a>(
    snapshot: &'a Snapshot,
    file: Option<FileId>,
    block: &ast::Block,
) -> Option<(&'a SchemaFile, &'a IndexMap<String, Field>)> {
    let schema = snapshot.schema_for(file)?.file;
    let brace = block
        .syntax()
        .children_with_tokens()
        .find(|t| t.kind() == SyntaxKind::L_BRACE)?;
    let text = document_text(block.syntax());
    let scope = Scope::new(snapshot, file);
    let cursor = place::locate(&scope, &text, brace.text_range().end());
    let shape = cursor.bracket()?.shape.as_ref()?;
    shape_struct(schema, shape).map(|fields| (schema, fields))
}

/// The type of an entry's value, and the doc of what it sets: the
/// field's, or the type's for a root entry.
pub(crate) fn entry_type(
    snapshot: &Snapshot,
    file: Option<FileId>,
    entry: &ast::Entry,
) -> Option<(TypeExpr, String)> {
    let schema = snapshot.schema_for(file)?;
    let end = entry.target()?.syntax().text_range().end();
    let text = document_text(entry.syntax());
    let scope = Scope::new(snapshot, file);
    let cursor = place::locate(&scope, &text, end);
    let place = cursor.current()?;
    let Some(Shape::Type(ty)) = &place.shape else {
        return None;
    };
    let doc = match &place.doc {
        Some(doc) => doc.clone(),
        None => named_def(schema.file, ty).map_or(String::new(), |d| d.doc().to_owned()),
    };
    Some((ty.clone(), doc))
}

/// The place one step into a shape, and the doc of the field stepped into.
pub(crate) fn step_shape<'a>(
    schema: &'a SchemaFile,
    shape: &Shape<'a>,
    path_step: &PathStep,
) -> Option<(Shape<'a>, Option<String>)> {
    let (ty, doc) = match (shape, path_step) {
        (Shape::Type(ty), _) => step(schema, ty, path_step, 0)?,
        (Shape::Fields(fields), PathStep::Field(name)) => {
            field_step(schema, fields, name, path_step, 0)?
        }
        _ => return None,
    };
    Some((Shape::Type(ty), doc))
}

/// What `Name { ... }` is at a place of the type `expected`: its variant
/// or the type itself, else the type of that name anywhere.
pub(crate) fn named_shape<'a>(
    schema: &'a SchemaFile,
    expected: Option<&Shape<'a>>,
    name: &str,
) -> Option<Shape<'a>> {
    if let Some(Shape::Type(ty)) = expected
        && let Some(shape) = named_in(schema, ty, name, 0)
    {
        return Some(shape);
    }
    self::named(schema, name).map(Shape::Type)
}

fn named_in<'a>(
    schema: &'a SchemaFile,
    ty: &TypeExpr,
    name: &str,
    depth: usize,
) -> Option<Shape<'a>> {
    if depth > MAX_DEPTH {
        return None;
    }
    match ty {
        TypeExpr::Option(inner) => named_in(schema, inner, name, depth + 1),
        TypeExpr::Named(path) => match schema.get(path)? {
            TypeDef::Enum { variants, .. } => match &variant(variants, name)?.shape {
                VariantShape::Struct(fields) => Some(Shape::Fields(fields)),
                VariantShape::Newtype(inner) => Some(Shape::Type(inner.clone())),
                _ => None,
            },
            def if def.name() == name => Some(Shape::Type(ty.clone())),
            TypeDef::Newtype { inner, .. } | TypeDef::Transparent { inner, .. } => {
                named_in(schema, inner, name, depth + 1)
            }
            _ => None,
        },
        _ => None,
    }
}

/// An enum's variant by its name or an alias.
fn variant<'a>(
    variants: &'a IndexMap<String, crol::schema::VariantDef>,
    name: &str,
) -> Option<&'a crol::schema::VariantDef> {
    variants.get(name).or_else(|| {
        variants
            .values()
            .find(|v| v.aliases.iter().any(|a| a == name))
    })
}

/// The type of argument `index` of `Name(...)` at a place of the type
/// `expected`, else of the type of that name anywhere.
pub(crate) fn payload(
    schema: &SchemaFile,
    expected: Option<&Shape<'_>>,
    name: &str,
    index: usize,
) -> Option<TypeExpr> {
    if let Some(Shape::Type(ty)) = expected
        && let Some(found) = payload_in(schema, ty, name, index, 0)
    {
        return Some(found);
    }
    payload_in(schema, &self::named(schema, name)?, name, index, 0)
}

fn payload_in(
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
        TypeExpr::Option(inner) => payload_in(schema, inner, name, index, depth + 1),
        TypeExpr::Named(path) => match schema.get(path)? {
            TypeDef::Enum { variants, .. } => match &variant(variants, name)?.shape {
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
                payload_in(schema, inner, name, index, depth + 1)
            }
            _ => None,
        },
        _ => None,
    }
}

/// Whether a place holds an enum, which only its value can say the
/// variant of.
pub(crate) fn is_enum(schema: &SchemaFile, shape: &Shape<'_>) -> bool {
    let Shape::Type(ty) = shape else {
        return false;
    };
    matches!(named_def(schema, ty), Some(TypeDef::Enum { .. }))
}

/// Whether a place holds a tuple, so that `(a, b)` is not parentheses.
pub(crate) fn is_tuple(schema: &SchemaFile, shape: &Shape<'_>) -> bool {
    let Shape::Type(ty) = shape else {
        return false;
    };
    match ty {
        TypeExpr::Tuple(_) => true,
        TypeExpr::Option(inner) => is_tuple(schema, &Shape::Type((**inner).clone())),
        _ => matches!(named_def(schema, ty), Some(TypeDef::TupleStruct { .. })),
    }
}

/// The fields of a struct or struct variant, as written in the schema.
fn shape_struct<'a>(
    schema: &'a SchemaFile,
    shape: &Shape<'a>,
) -> Option<&'a IndexMap<String, Field>> {
    match shape {
        Shape::Keys => None,
        Shape::Type(ty) => struct_of(schema, ty).map(|(_, fields)| fields),
        Shape::Fields(fields) => Some(fields),
    }
}

/// The fields a block at a place can have, with those of flattened fields
/// in place of them.
pub(crate) fn fields_of<'a>(
    schema: &'a SchemaFile,
    shape: &Shape<'a>,
) -> Vec<(&'a String, &'a Field)> {
    let mut out = Vec::new();
    if let Some(fields) = shape_struct(schema, shape) {
        flatten_into(schema, fields, &mut out, 0);
    }
    out
}

fn flatten_into<'a>(
    schema: &'a SchemaFile,
    fields: &'a IndexMap<String, Field>,
    out: &mut Vec<(&'a String, &'a Field)>,
    depth: usize,
) {
    if depth > MAX_DEPTH {
        return;
    }
    for (name, field) in fields {
        if !field.flatten {
            out.push((name, field));
        } else if let Some((_, inner)) = struct_of(schema, &field.ty) {
            flatten_into(schema, inner, out, depth + 1);
        }
    }
}

/// The definition of a named type, through options.
pub(crate) fn named_def<'a>(schema: &'a SchemaFile, ty: &TypeExpr) -> Option<&'a TypeDef> {
    match ty {
        TypeExpr::Option(inner) => named_def(schema, inner),
        TypeExpr::Named(path) => schema.get(path),
        _ => None,
    }
}

/// A type as Rust writes it, with serde names: `u32`, `Option<Limits>`.
pub(crate) fn rust_type(schema: &SchemaFile, ty: &TypeExpr) -> String {
    let inner = |t: &TypeExpr| rust_type(schema, t);
    match ty {
        TypeExpr::Bool => "bool".to_owned(),
        TypeExpr::Char => "char".to_owned(),
        TypeExpr::String => "String".to_owned(),
        TypeExpr::Unit => "()".to_owned(),
        TypeExpr::Any => "any value".to_owned(),
        TypeExpr::Bytes => "bytes".to_owned(),
        TypeExpr::Id(_) => "Id".to_owned(),
        TypeExpr::Option(t) => format!("Option<{}>", inner(t)),
        TypeExpr::List(t) => format!("Vec<{}>", inner(t)),
        TypeExpr::Set(t) => format!("Set<{}>", inner(t)),
        TypeExpr::Array(t, n) => format!("[{}; {n}]", inner(t)),
        TypeExpr::Map(k, v) => format!("Map<{}, {}>", inner(k), inner(v)),
        TypeExpr::Tuple(items) => {
            let items: Vec<String> = items.iter().map(inner).collect();
            format!("({})", items.join(", "))
        }
        TypeExpr::Named(path) => schema
            .get(path)
            .map_or_else(|| path.clone(), |d| d.name().to_owned()),
        other => format!("{other:?}").to_lowercase(),
    }
}

/// The named type with the serde name `name`, if exactly one has it.
pub(crate) fn named(schema: &SchemaFile, name: &str) -> Option<TypeExpr> {
    let mut found = schema.by_name(name);
    let (path, _) = found.next()?;
    found
        .next()
        .is_none()
        .then(|| TypeExpr::Named(path.to_owned()))
}

/// A field of a struct by its name, in a flattened field if not its own.
fn field_step(
    schema: &SchemaFile,
    fields: &IndexMap<String, Field>,
    name: &str,
    path_step: &PathStep,
    depth: usize,
) -> Option<(TypeExpr, Option<String>)> {
    if let Some(found) = field(fields, name).filter(|f| !f.flatten) {
        return Some((found.ty.clone(), Some(found.doc.clone())));
    }
    fields
        .values()
        .filter(|f| f.flatten)
        .find_map(|f| step(schema, &f.ty, path_step, depth + 1))
}

/// The type one step further in, and the doc of the field stepped into.
fn step(
    schema: &SchemaFile,
    ty: &TypeExpr,
    step: &PathStep,
    depth: usize,
) -> Option<(TypeExpr, Option<String>)> {
    if depth > MAX_DEPTH {
        return None;
    }
    match (ty, step) {
        (TypeExpr::Option(inner), _) => self::step(schema, inner, step, depth + 1),
        (TypeExpr::Named(path), _) => match schema.get(path)? {
            TypeDef::Struct { fields, .. } => match step {
                PathStep::Field(name) => field_step(schema, fields, name, step, depth),
                _ => None,
            },
            TypeDef::TupleStruct { fields, .. } => match step {
                PathStep::Element(i) => fields.get(*i).map(|t| (t.clone(), None)),
                _ => None,
            },
            TypeDef::Newtype { inner, .. } | TypeDef::Transparent { inner, .. } => {
                self::step(schema, inner, step, depth + 1)
            }
            _ => None,
        },
        (TypeExpr::Map(_, value), PathStep::Field(_) | PathStep::Accessor(_)) => {
            Some(((**value).clone(), None))
        }
        (
            TypeExpr::List(item) | TypeExpr::Set(item) | TypeExpr::Array(item, _),
            PathStep::Accessor(_) | PathStep::Element(_),
        ) => Some(((**item).clone(), None)),
        (TypeExpr::Tuple(items), PathStep::Element(i)) => items.get(*i).map(|t| (t.clone(), None)),
        _ => None,
    }
}

/// A struct's field by its name or an alias.
pub(crate) fn field<'a>(fields: &'a IndexMap<String, Field>, name: &str) -> Option<&'a Field> {
    fields.get(name).or_else(|| {
        fields
            .values()
            .find(|f| f.aliases.iter().any(|a| a == name))
    })
}

/// The struct a type is, through options, newtypes and transparent types:
/// its serde name and fields.
pub(crate) fn struct_of<'a>(
    schema: &'a SchemaFile,
    ty: &TypeExpr,
) -> Option<(&'a str, &'a IndexMap<String, Field>)> {
    match ty {
        TypeExpr::Option(inner) => struct_of(schema, inner),
        TypeExpr::Named(path) => match schema.get(path)? {
            TypeDef::Struct { name, fields, .. } => Some((name, fields)),
            TypeDef::Newtype { inner, .. } | TypeDef::Transparent { inner, .. } => {
                struct_of(schema, inner)
            }
            _ => None,
        },
        _ => None,
    }
}

/// A value of the right shape for a type, to be filled in: `0`, `""`,
/// `[]`, `None`, `Limits { .. }`, an enum's first variant.
pub(crate) fn placeholder(schema: &SchemaFile, ty: &TypeExpr) -> String {
    placeholder_at(schema, ty, 0)
}

fn placeholder_at(schema: &SchemaFile, ty: &TypeExpr, depth: usize) -> String {
    // Types that contain themselves stop here.
    if depth > 8 {
        return "()".to_owned();
    }
    let many = |types: &[TypeExpr]| {
        types
            .iter()
            .map(|t| placeholder_at(schema, t, depth + 1))
            .collect::<Vec<_>>()
            .join(", ")
    };
    match ty {
        TypeExpr::Bool => "false".to_owned(),
        TypeExpr::Char => "' '".to_owned(),
        TypeExpr::String => "\"\"".to_owned(),
        TypeExpr::Unit | TypeExpr::Any => "()".to_owned(),
        TypeExpr::I8
        | TypeExpr::I16
        | TypeExpr::I32
        | TypeExpr::I64
        | TypeExpr::I128
        | TypeExpr::U8
        | TypeExpr::U16
        | TypeExpr::U32
        | TypeExpr::U64
        | TypeExpr::U128 => "0".to_owned(),
        TypeExpr::F32 | TypeExpr::F64 => "0.0".to_owned(),
        TypeExpr::Bytes | TypeExpr::List(_) | TypeExpr::Set(_) | TypeExpr::Array(..) => {
            "[]".to_owned()
        }
        TypeExpr::Id(_) => "@\"\"".to_owned(),
        TypeExpr::Option(_) => "None".to_owned(),
        TypeExpr::Tuple(items) => format!("({})", many(items)),
        TypeExpr::Map(..) => "{}".to_owned(),
        TypeExpr::Named(path) => match schema.get(path) {
            None => "()".to_owned(),
            Some(TypeDef::Struct { name, .. }) => format!("{name} {{ .. }}"),
            Some(TypeDef::TupleStruct { name, fields, .. }) => format!("{name}({})", many(fields)),
            Some(TypeDef::Newtype { name, inner, .. }) => {
                format!("{name}({})", placeholder_at(schema, inner, depth + 1))
            }
            Some(TypeDef::Transparent { inner, .. }) => placeholder_at(schema, inner, depth + 1),
            Some(TypeDef::UnitStruct { name, .. }) => name.clone(),
            Some(TypeDef::Enum { variants, .. }) => match variants.first() {
                None => "()".to_owned(),
                Some((name, variant)) => match &variant.shape {
                    VariantShape::Unit => name.clone(),
                    VariantShape::Newtype(inner) => {
                        format!("{name}({})", placeholder_at(schema, inner, depth + 1))
                    }
                    VariantShape::Tuple(items) => format!("{name}({})", many(items)),
                    VariantShape::Struct(_) => format!("{name} {{ .. }}"),
                },
            },
        },
    }
}
