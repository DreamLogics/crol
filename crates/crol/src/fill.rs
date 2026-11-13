//! Filling in the defaults serde would fill, for showing the data as the
//! application sees it (`SPEC.md` 11).

use indexmap::IndexMap;

use crate::RawValue;
use crate::schema::{
    DataRoot, Field, FieldDefault, Repr, SchemaFile, TypeDef, TypeExpr, VariantShape,
};
use crol_eval::{Key, Keyed, PathStep, Span, Value, ValueKind};

/// How deep types are followed; types that contain themselves stop here.
const MAX_DEPTH: usize = 64;

/// The data with each struct field that takes its default written out:
/// fields removed with `-field`, and, in a struct with `..` or of a type
/// with `explicit_defaults: false`, fields left out. Only defaults the schema records are filled in; each filled-in
/// value has the span `filled`, so a printer can say where it came from.
/// `root` is the root type (`None`: the schema's): of the whole root if it
/// is the whole document, else of each root value (`SPEC.md` 4.1).
pub fn fill_defaults(data: &Value, schema: &SchemaFile, root: Option<&str>, filled: Span) -> Value {
    let root_type = TypeExpr::Named(root.unwrap_or(&schema.root).to_owned());
    let filler = Filler { schema, filled };
    let mut data = data.clone();
    if schema.data_root(root) == DataRoot::Document {
        filler.value(&mut data, &root_type, None, 0);
    } else if let ValueKind::Keyed(keyed) = &mut data.kind {
        for value in keyed.entries.values_mut() {
            filler.value(value, &root_type, None, 0);
        }
    }
    data
}

/// [`fill_defaults`] for a value-mode document of the type at `root`.
pub fn fill_defaults_value(
    value: &Value,
    schema: &SchemaFile,
    root: Option<&str>,
    filled: Span,
) -> Value {
    let root_type = TypeExpr::Named(root.unwrap_or(&schema.root).to_owned());
    let mut value = value.clone();
    Filler { schema, filled }.value(&mut value, &root_type, None, 0);
    value
}

struct Filler<'a> {
    schema: &'a SchemaFile,
    filled: Span,
}

impl Filler<'_> {
    fn value(&self, value: &mut Value, ty: &TypeExpr, variant: Option<&str>, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        match (ty, &mut value.kind) {
            (TypeExpr::Option(inner), _) => self.value(value, inner, variant, depth + 1),
            // An id has nothing to fill; a value written in place does.
            (TypeExpr::IdOr(..), ValueKind::Id(_)) => {}
            (TypeExpr::IdOr(_, inner), _) => self.value(value, inner, variant, depth + 1),
            (
                TypeExpr::List(item) | TypeExpr::Set(item) | TypeExpr::Array(item, _),
                ValueKind::List(items),
            ) => {
                for element in items {
                    self.value(element, item, None, depth + 1);
                }
            }
            (TypeExpr::Tuple(types), ValueKind::Tuple(items)) => {
                for (element, ty) in items.iter_mut().zip(types) {
                    self.value(element, ty, None, depth + 1);
                }
            }
            (TypeExpr::Map(_, item), ValueKind::Keyed(keyed)) => {
                for element in keyed.entries.values_mut() {
                    self.value(element, item, None, depth + 1);
                }
            }
            (TypeExpr::Named(path), _) => self.named(value, path, variant, depth + 1),
            _ => {}
        }
    }

    fn named(&self, value: &mut Value, type_path: &str, variant: Option<&str>, depth: usize) {
        let Some(definition) = self.schema.get(type_path) else {
            return;
        };
        match definition {
            TypeDef::Struct {
                name,
                fields,
                default,
                explicit_defaults,
                ..
            } => {
                if let ValueKind::Keyed(keyed) = &mut value.kind
                    && keyed
                        .name
                        .as_ref()
                        .is_none_or(|w| **w == **name || Some(&**w) == variant)
                {
                    self.fields(keyed, fields, default.as_ref(), *explicit_defaults, depth);
                }
            }
            TypeDef::Newtype { name, inner, .. } => match &mut value.kind {
                ValueKind::NamedTuple(written, items)
                    if **written == **name && items.len() == 1 =>
                {
                    self.value(&mut items[0], inner, None, depth);
                }
                _ => self.value(value, inner, None, depth),
            },
            TypeDef::Transparent { inner, .. } => self.value(value, inner, None, depth),
            TypeDef::TupleStruct { fields, .. } => {
                if let ValueKind::NamedTuple(_, items) | ValueKind::Tuple(items) = &mut value.kind
                    && items.len() == fields.len()
                {
                    for (item, ty) in items.iter_mut().zip(fields) {
                        self.value(item, ty, None, depth);
                    }
                }
            }
            TypeDef::Enum { repr, variants, .. } if *repr == Repr::External => {
                let written = match &value.kind {
                    ValueKind::NamedTuple(v, _) => v.to_string(),
                    ValueKind::Keyed(keyed) if keyed.name.is_some() => {
                        keyed.name.as_deref().unwrap_or_default().to_owned()
                    }
                    _ => return,
                };
                let Some((variant_name, variant)) = variants
                    .iter()
                    .find(|(n, v)| **n == written || v.aliases.contains(&written))
                else {
                    return;
                };
                match (&variant.shape, &mut value.kind) {
                    (VariantShape::Newtype(inner), ValueKind::NamedTuple(_, items))
                        if items.len() == 1 =>
                    {
                        self.value(&mut items[0], inner, None, depth);
                    }
                    // `V { ... }` as a newtype variant (`SPEC.md` 10.4).
                    (VariantShape::Newtype(inner), ValueKind::Keyed(_)) => {
                        self.value(value, inner, Some(variant_name), depth);
                    }
                    (VariantShape::Tuple(types), ValueKind::NamedTuple(_, items))
                        if items.len() == types.len() =>
                    {
                        for (item, ty) in items.iter_mut().zip(types) {
                            self.value(item, ty, None, depth);
                        }
                    }
                    (VariantShape::Struct(fields), ValueKind::Keyed(keyed)) => {
                        self.fields(keyed, fields, None, variant.explicit_defaults, depth);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// Fills a struct's fields that take their defaults, in the order of
    /// the struct, after those written; then looks inside every field.
    fn fields(
        &self,
        keyed: &mut Keyed,
        fields: &IndexMap<String, Field>,
        struct_default: Option<&RawValue>,
        explicit_defaults: Option<bool>,
        depth: usize,
    ) {
        let written = |keyed: &Keyed, name: &str, field: &Field| {
            std::iter::once(name)
                .chain(field.aliases.iter().map(String::as_str))
                .any(|n| keyed.entries.contains_key(&Key::Str(n.into())))
        };
        for (name, field) in fields {
            if field.flatten || written(keyed, name, field) {
                continue;
            }
            let key = Key::Str(name.as_str().into());
            if !keyed.defaults
                && explicit_defaults != Some(false)
                && !keyed.defaulted.contains(&key)
            {
                continue;
            }
            if let Some(mut default) = field_default(name, field, struct_default) {
                default.span = self.filled;
                keyed.defaulted.remove(&key);
                keyed.entries.insert(key, default);
            }
        }
        // `..` says nothing more once every field is written.
        if keyed.defaults
            && fields
                .iter()
                .all(|(n, f)| f.flatten || written(keyed, n, f))
        {
            keyed.defaults = false;
        }
        for (key, item) in &mut keyed.entries {
            if item.span == self.filled {
                continue;
            }
            let Key::Str(name) = key else { continue };
            let field = fields.get(&**name).or_else(|| {
                fields
                    .values()
                    .find(|f| f.aliases.iter().any(|a| **a == **name))
            });
            if let Some(field) = field {
                self.value(item, &field.ty, None, depth + 1);
            }
        }
    }
}

/// A field's default: its own, or its value in the struct's default.
fn field_default(name: &str, field: &Field, struct_default: Option<&RawValue>) -> Option<Value> {
    match &field.default {
        FieldDefault::Value(value) => Some(value.0.clone()),
        _ => crol_eval::value_at_path(&struct_default?.0, &[PathStep::Field(name.to_owned())])
            .cloned(),
    }
}
