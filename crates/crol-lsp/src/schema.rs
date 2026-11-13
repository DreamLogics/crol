//! What the schema says about a place in the data (`SPEC.md` 10.9, 12.5).

use crol::schema::{Field, Repr, SchemaFile, TypeDef, TypeExpr, VariantShape};
use crol_eval::{FileId, PathStep};
use crol_syntax::ast::{self, AstNode};
use crol_syntax::{SyntaxKind, SyntaxNode};
use indexmap::IndexMap;

use crate::place::{self, Place, Scope};
use crate::workspace::{DocumentSchema, Snapshot};

/// How deep to follow types into each other where a type may contain
/// itself.
const MAX_DEPTH: usize = 8;

/// How many fields a struct shows on hover.
const MAX_FIELDS: usize = 20;

/// What a place holds, by the schema.
#[derive(Clone)]
pub(crate) enum Shape<'a> {
    /// The root keys of data whose root type is that of each root value.
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

/// A field's doc, with what constrains it (`SPEC.md` 12.5).
pub(crate) fn field_doc(field: &crol::schema::Field) -> String {
    let mut parts = Vec::new();
    if !field.doc.trim().is_empty() {
        parts.push(field.doc.trim().to_owned());
    }
    if let Some(name) = &field.constraint {
        parts.push(format!("Constraint: `{name}`"));
    }
    if let Some(name) = &field.key_constraint {
        parts.push(format!("Keys: `{name}`"));
    }
    if field.sealed {
        parts.push("Sealed: only the layer that defines this value can set it.".to_owned());
    }
    parts.join("\n\n")
}

/// What hover says of a place: the doc and the type of what is there.
pub(crate) struct About {
    /// The doc: of the field the place is in, else, where it is in none, of
    /// its type. It may be empty.
    pub(crate) doc: String,
    /// The type, as [`rust_type`] writes it.
    pub(crate) ty: Option<String>,
    /// The struct the type is, with its fields, as [`struct_definition`]
    /// writes it.
    pub(crate) definition: Option<String>,
}

/// What the schema says of a place: nothing if it does not know the place,
/// or knows only that it can hold anything.
pub(crate) fn about(schema: &DocumentSchema<'_>, place: &Place<'_>) -> Option<About> {
    let ty = expected_type(schema, place)?;
    let doc = match &place.doc {
        Some(doc) => doc.clone(),
        None => named_def(schema.file, &ty).map_or(String::new(), |d| d.doc().to_owned()),
    };
    if doc.is_empty() && ty == TypeExpr::Any {
        return None;
    }
    let definition = struct_definition(schema.file, &ty);
    let ty = Some(rust_type(schema.file, &ty));
    Some(About {
        doc,
        ty,
        definition,
    })
}

/// The type of a place. A root value of a struct variant of the root type
/// has only the fields as its shape; its type is the root type.
fn expected_type(schema: &DocumentSchema<'_>, place: &Place<'_>) -> Option<TypeExpr> {
    match place.shape.as_ref()? {
        Shape::Type(ty) => Some(ty.clone()),
        Shape::Fields(_) if place.path.as_ref().is_some_and(|p| p.len() == 1) => {
            Some(root_expr(schema))
        }
        Shape::Fields(_) | Shape::Keys => None,
    }
}

/// What hover says of `Name` at a place, written as a value there or as the
/// name of one: of the variant it is, by the variant's doc, else the doc of
/// the place; of the type it is, by the doc of the place, else the type's.
/// Nothing for what is no name there, such as `Some`.
pub(crate) fn about_name(
    schema: &DocumentSchema<'_>,
    place: &Place<'_>,
    name: &str,
) -> Option<About> {
    let doc = place.doc.clone().unwrap_or_default();
    let found = match expected_type(schema, place) {
        Some(ty) if ty != TypeExpr::Any => name_in(schema.file, &ty, name, 0),
        _ => self::named(schema.file, name).and_then(|ty| name_in(schema.file, &ty, name, 0)),
    }?;
    let (own, ty, of_type) = found;
    let definition = of_type
        .as_ref()
        .and_then(|of_type| struct_definition(schema.file, of_type));
    // A variant's own doc goes first; a type's goes where the place has none.
    let doc = match of_type {
        None if !own.is_empty() => own,
        Some(_) if doc.is_empty() => own,
        _ => doc,
    };
    Some(About {
        doc,
        ty: Some(ty),
        definition,
    })
}

/// `Name` in a type: its doc, how to write it, and the type it names, or
/// nothing where it is a variant.
fn name_in(
    schema: &SchemaFile,
    ty: &TypeExpr,
    name: &str,
    depth: usize,
) -> Option<(String, String, Option<TypeExpr>)> {
    if depth > MAX_DEPTH {
        return None;
    }
    match ty {
        TypeExpr::Option(inner) => name_in(schema, inner, name, depth + 1),
        TypeExpr::Named(path) => match schema.get(path)? {
            TypeDef::Enum {
                name: own,
                variants,
                ..
            } => {
                let found = variant(variants, name)?;
                Some((found.doc.clone(), format!("{own}::{name}"), None))
            }
            def if def.name() == name => Some((
                def.doc().to_owned(),
                name.to_owned(),
                Some(TypeExpr::Named(path.clone())),
            )),
            TypeDef::Newtype { inner, .. } | TypeDef::Transparent { inner, .. } => {
                name_in(schema, inner, name, depth + 1)
            }
            _ => None,
        },
        _ => None,
    }
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
        TypeExpr::Option(inner) | TypeExpr::IdOr(_, inner) => {
            named_in(schema, inner, name, depth + 1)
        }
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
        TypeExpr::Option(inner) | TypeExpr::IdOr(_, inner) => {
            payload_in(schema, inner, name, index, depth + 1)
        }
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
        TypeExpr::Option(inner) | TypeExpr::IdOr(_, inner) => {
            is_tuple(schema, &Shape::Type((**inner).clone()))
        }
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
/// in place of them. A tagged or untagged enum is written as a block with no
/// name to say the variant by (`SPEC.md` 10.4), so it can have its tag and
/// those of every variant.
pub(crate) fn fields_of(schema: &SchemaFile, shape: &Shape<'_>) -> Vec<(String, Field)> {
    let mut found = Vec::new();
    let mut own = Vec::new();
    if let Some(fields) = shape_struct(schema, shape) {
        flatten_into(schema, fields, &mut found, 0);
    } else if let Shape::Type(ty) = shape
        && let Some(TypeDef::Enum { repr, variants, .. }) = named_def(schema, ty)
    {
        let tag = |name: &String, ty, doc: &str| {
            let mut field = Field::new(ty);
            field.doc = doc.to_owned();
            (name.clone(), field)
        };
        match repr {
            Repr::External => return Vec::new(),
            Repr::Internal { tag: name } => {
                own.push(tag(name, TypeExpr::String, "The variant."));
            }
            Repr::Adjacent { tag: name, content } => {
                own.push(tag(name, TypeExpr::String, "The variant."));
                own.push(tag(content, TypeExpr::Any, "The variant's value."));
                return own;
            }
            Repr::Untagged => {}
        }
        for variant in variants.values() {
            let fields = match &variant.shape {
                VariantShape::Struct(fields) => Some(fields),
                VariantShape::Newtype(inner) => struct_of(schema, inner).map(|(_, f)| f),
                _ => None,
            };
            if let Some(fields) = fields {
                flatten_into(schema, fields, &mut found, 0);
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    own.into_iter()
        .chain(found.into_iter().map(|(n, f)| (n.clone(), f.clone())))
        .filter(|(name, _)| seen.insert(name.clone()))
        .collect()
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

/// The definition of a named type, through options and the value side of
/// `IdOr`.
pub(crate) fn named_def<'a>(schema: &'a SchemaFile, ty: &TypeExpr) -> Option<&'a TypeDef> {
    match ty {
        TypeExpr::Option(inner) | TypeExpr::IdOr(_, inner) => named_def(schema, inner),
        TypeExpr::Named(path) => schema.get(path),
        _ => None,
    }
}

/// A type as Rust writes it, with serde names: `u32`, `Option<Limits>`.
pub(crate) fn rust_type(schema: &SchemaFile, ty: &TypeExpr) -> String {
    crol::typed::rust_type(schema, ty)
}

/// The struct a type is, through options, as Rust writes it with its
/// fields:
///
/// ```text
/// struct Limits {
///     cpu: f64,
///     memory_mb: u32,
/// }
/// ```
///
/// Nothing for any other type. Past [`MAX_FIELDS`] fields, the rest are
/// `/* … */`.
pub(crate) fn struct_definition(schema: &SchemaFile, ty: &TypeExpr) -> Option<String> {
    let mut ty = ty;
    while let TypeExpr::Option(inner) | TypeExpr::IdOr(_, inner) = ty {
        ty = inner;
    }
    let TypeExpr::Named(path) = ty else {
        return None;
    };
    let Some(TypeDef::Struct { fields, .. }) = schema.get(path) else {
        return None;
    };
    let name = rust_type(schema, ty);
    if fields.is_empty() {
        return Some(format!("struct {name} {{}}"));
    }
    let mut out = format!("struct {name} {{\n");
    for (field, def) in fields.iter().take(MAX_FIELDS) {
        if def.flatten {
            out.push_str("    #[serde(flatten)]\n");
        }
        out.push_str(&format!("    {field}: {},\n", rust_type(schema, &def.ty)));
    }
    if fields.len() > MAX_FIELDS {
        out.push_str("    /* … */\n");
    }
    out.push('}');
    Some(out)
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
        return Some((found.ty.clone(), Some(field_doc(found))));
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
        // A path goes into a value written in place, never through an id.
        (TypeExpr::Option(inner) | TypeExpr::IdOr(_, inner), _) => {
            self::step(schema, inner, step, depth + 1)
        }
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
        TypeExpr::Option(inner) | TypeExpr::IdOr(_, inner) => struct_of(schema, inner),
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
        // The value, which shows more of the shape than an empty id.
        TypeExpr::IdOr(_, inner) => placeholder_at(schema, inner, depth + 1),
        TypeExpr::Option(_) => "None".to_owned(),
        TypeExpr::Tuple(items) => format!("({})", many(items)),
        TypeExpr::Map(..) => "{}".to_owned(),
        TypeExpr::Named(path) => match schema.get(path) {
            None => "()".to_owned(),
            Some(TypeDef::Struct { name, .. }) if name.is_empty() => "{ .. }".to_owned(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn with(types: impl IntoIterator<Item = (&'static str, TypeDef)>) -> SchemaFile {
        SchemaFile {
            format: 1,
            root: String::new(),
            types: types.into_iter().map(|(p, d)| (p.to_owned(), d)).collect(),
            constraints: Default::default(),
        }
    }

    fn structure(name: &str, fields: impl IntoIterator<Item = (String, Field)>) -> TypeDef {
        TypeDef::Struct {
            name: name.to_owned(),
            doc: String::new(),
            source: (String::new(), 0),
            fields: fields.into_iter().collect(),
            deny_unknown_fields: false,
            default: None,
            explicit_defaults: None,
            scope: false,
        }
    }

    #[test]
    fn a_struct_shows_its_fields() {
        let mut base = Field::new(TypeExpr::Named("app::Base".to_owned()));
        base.flatten = true;
        let schema = with([
            (
                "app::Web",
                structure(
                    "Web",
                    [
                        ("port".to_owned(), Field::new(TypeExpr::U16)),
                        ("base".to_owned(), base),
                    ],
                ),
            ),
            ("app::Base", structure("Base", [])),
        ]);
        let web = TypeExpr::Named("app::Web".to_owned());
        let wanted = "struct Web {\n    port: u16,\n    #[serde(flatten)]\n    base: Base,\n}";
        assert_eq!(struct_definition(&schema, &web).unwrap(), wanted);
        let optional = TypeExpr::Option(Box::new(web));
        assert_eq!(struct_definition(&schema, &optional).unwrap(), wanted);
        let base = TypeExpr::Named("app::Base".to_owned());
        assert_eq!(struct_definition(&schema, &base).unwrap(), "struct Base {}");
        assert_eq!(struct_definition(&schema, &TypeExpr::U16), None);
    }

    #[test]
    fn a_struct_with_many_fields_shows_the_first() {
        let fields = (0..MAX_FIELDS + 1).map(|i| (format!("f{i}"), Field::new(TypeExpr::Bool)));
        let schema = with([("app::Big", structure("Big", fields))]);
        let shown = struct_definition(&schema, &TypeExpr::Named("app::Big".to_owned())).unwrap();
        assert_eq!(shown.lines().count(), MAX_FIELDS + 3, "{shown}");
        assert!(shown.ends_with("    /* … */\n}"), "{shown}");
    }
}
