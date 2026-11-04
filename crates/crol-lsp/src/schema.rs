//! What the schema says about a place in the data (`SPEC.md` 10.9, 12.5).

use crol::schema::{Field, SchemaFile, TypeDef, TypeExpr, VariantShape};
use crol_eval::{FileId, PathStep, ValueKind};
use crol_syntax::SyntaxKind;
use crol_syntax::ast::{self, AstNode};
use indexmap::IndexMap;

use crate::features;
use crate::workspace::{DocumentSchema, Snapshot};

/// The type of a block: its type name's, else that of its place in the
/// block or root entry around it, through struct fields, map values,
/// elements, and the payloads of options, newtypes and transparent types.
pub(crate) fn block_type(
    snapshot: &Snapshot,
    file: Option<FileId>,
    block: &ast::Block,
) -> Option<TypeExpr> {
    let schema = snapshot.schema_for(file)?;
    // The outer braces of a document on its own are its root type.
    if schema.value_mode
        && block.type_name().is_none()
        && block
            .syntax()
            .parent()
            .is_some_and(|p| p.kind() == SyntaxKind::DOCUMENT)
    {
        return Some(TypeExpr::Named(schema.root_type().to_owned()));
    }
    block_type_in(
        schema.file,
        &|key| root_key_type(snapshot, &schema, key),
        block,
        0,
    )
}

/// The type of an entry's value, and the doc of what it sets: the
/// field's, or the type's for a root entry.
pub(crate) fn entry_type(
    snapshot: &Snapshot,
    file: Option<FileId>,
    entry: &ast::Entry,
) -> Option<(TypeExpr, String)> {
    let schema = snapshot.schema_for(file)?;
    let (ty, doc) = entry_type_in(
        schema.file,
        &|key| root_key_type(snapshot, &schema, key),
        entry,
        0,
    )?;
    let doc = match doc {
        Some(doc) => doc,
        None => named_def(schema.file, &ty).map_or(String::new(), |d| d.doc().to_owned()),
    };
    Some((ty, doc))
}

/// The type of a root key's value, and its doc as a field. In a document
/// on its own, a root key is a field of the root type. In data, it is the
/// root type, or, when that is an enum (`Definition`), the type the key's
/// value names (`Service`).
fn root_key_type(
    snapshot: &Snapshot,
    schema: &DocumentSchema<'_>,
    key: &str,
) -> Option<(TypeExpr, Option<String>)> {
    let root = schema.root_type();
    if schema.value_mode {
        let root = TypeExpr::Named(root.to_owned());
        return step(schema.file, &root, &PathStep::Field(key.to_owned()));
    }
    if let Some(TypeDef::Enum { .. }) = schema.file.get(root) {
        let evaluation = match (&snapshot.evaluation.value, &snapshot.last_good) {
            (None, Some(good)) => good,
            _ => &snapshot.evaluation,
        };
        let name = features::value_in(evaluation, key, &[], &[]).and_then(|v| match v.kind {
            ValueKind::Keyed(k) => k.name,
            ValueKind::NamedTuple(n, _) | ValueKind::UnitName(n) => Some(n),
            _ => None,
        });
        if let Some(ty) = name.and_then(|n| named(schema.file, &n)) {
            return Some((ty, None));
        }
    }
    Some((TypeExpr::Named(root.to_owned()), None))
}

/// The type of each root key's value, and its doc as a field.
type RootTypes<'a> = &'a dyn Fn(&str) -> Option<(TypeExpr, Option<String>)>;

fn block_type_in(
    schema: &SchemaFile,
    root: RootTypes<'_>,
    block: &ast::Block,
    depth: usize,
) -> Option<TypeExpr> {
    if let Some(name) = block.type_name() {
        return named(schema, name.text());
    }
    let entry = block.syntax().parent().and_then(ast::Entry::cast)?;
    entry_type_in(schema, root, &entry, depth).map(|(ty, _)| ty)
}

/// The type of an entry's value, and the doc of the last field its
/// target goes through, if any.
fn entry_type_in(
    schema: &SchemaFile,
    root: RootTypes<'_>,
    entry: &ast::Entry,
    depth: usize,
) -> Option<(TypeExpr, Option<String>)> {
    if depth > crol_syntax::MAX_NESTING {
        return None;
    }
    let target = entry.target()?;
    // The block or document the entry is in, through variant blocks.
    let mut container = entry.syntax().parent()?;
    while container.kind() == SyntaxKind::VARIANT_BLOCK {
        container = container.parent()?;
    }
    let outer = ast::Block::cast(container.clone()).filter(|b| {
        b.type_name().is_some()
            || container
                .parent()
                .is_some_and(|p| p.kind() != SyntaxKind::DOCUMENT)
    });
    let key = features::key_text(&target.key()?)?;
    let (mut ty, mut doc) = match outer {
        // In a block: the key is one of its fields.
        Some(outer) => {
            let ty = block_type_in(schema, root, &outer, depth + 1)?;
            step(schema, &ty, &PathStep::Field(key))?
        }
        // A root entry.
        None => root(&key)?,
    };
    for s in features::selector_steps(target.selectors()) {
        let (next, field_doc) = step(schema, &ty, &s)?;
        ty = next;
        doc = field_doc;
    }
    Some((ty, doc))
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

/// The type one step further in, and the doc of the field stepped into.
fn step(schema: &SchemaFile, ty: &TypeExpr, step: &PathStep) -> Option<(TypeExpr, Option<String>)> {
    match (ty, step) {
        (TypeExpr::Option(inner), _) => self::step(schema, inner, step),
        (TypeExpr::Named(path), _) => match schema.get(path)? {
            TypeDef::Struct { fields, .. } => match step {
                PathStep::Field(name) => {
                    field(fields, name).map(|f| (f.ty.clone(), Some(f.doc.clone())))
                }
                _ => None,
            },
            TypeDef::TupleStruct { fields, .. } => match step {
                PathStep::Element(i) => fields.get(*i).map(|t| (t.clone(), None)),
                _ => None,
            },
            TypeDef::Newtype { inner, .. } | TypeDef::Transparent { inner, .. } => {
                self::step(schema, inner, step)
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
