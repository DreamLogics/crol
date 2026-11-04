//! `crol::check`: the deserializer's rules applied to evaluated data with
//! only a schema, and id checks (`SPEC.md` 10.9).

use crol_eval::{Context, ContextData, Diagnostic, Key, Keyed, Span, Value, ValueKind, Variants};

use crate::compose::{CrolData, VariantName};
use crate::schema::{Field, FieldDefault, Repr, SchemaFile, TypeDef, TypeExpr, VariantShape};

/// Options of [`check`], as `CrolConfig` sets them for deserializing.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CheckOptions {
    /// Any value is accepted where an `Option` is expected, as `Some`.
    pub implicit_some: bool,
    /// Keys a struct does not have are ignored.
    pub allow_unknown_fields: bool,
    /// Every field must be written unless the value has `..`. A type's
    /// `explicit_defaults` comes first (`SPEC.md` 10.3).
    pub require_complete: bool,
}

impl Default for CheckOptions {
    fn default() -> Self {
        Self {
            implicit_some: true,
            allow_unknown_fields: false,
            require_complete: true,
        }
    }
}

/// Evaluated data to check: the base root, how each other context differs
/// from it, and the variants. [`CrolData::view`] gives one, and so does an
/// evaluation from `crol-eval`.
#[derive(Debug, Clone, Copy)]
pub struct DataView<'a> {
    /// The base root.
    pub base: &'a Value,
    /// The other evaluated contexts.
    pub contexts: &'a [ContextData],
    /// The variants.
    pub variants: &'a Variants,
}

impl<'a> DataView<'a> {
    fn value(&self, key: &str, context: Option<&'a ContextData>) -> Option<&'a Value> {
        let key = Key::Str(key.to_owned());
        match context {
            Some(data) => data.get(self.base, &key),
            None => match &self.base.kind {
                ValueKind::Keyed(keyed) => keyed.entries.get(&key),
                _ => None,
            },
        }
    }

    fn keys(&self, context: Option<&ContextData>) -> Vec<String> {
        let root = context.map_or_else(|| self.base.clone(), |data| data.root(self.base));
        match &root.kind {
            ValueKind::Keyed(keyed) => keyed
                .entries
                .keys()
                .filter_map(|k| match k {
                    Key::Str(s) => Some(s.clone()),
                    Key::Int(_) => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    fn names(&self, context: &Context) -> String {
        format!("{{{}}}", self.variants.chosen_names(context).join(", "))
    }
}

/// Checks evaluated data against a schema (`SPEC.md` 10.9): each root
/// value against the type at `root` (`None`: the schema's root type), with
/// the rules of the deserializer, and every id: it must name a root key in
/// the same context, of one of the types it may refer to. Every evaluated
/// context is checked.
pub fn check<T: VariantName>(
    data: &CrolData<T>,
    schema: &SchemaFile,
    root: Option<&str>,
    options: &CheckOptions,
) -> Vec<Diagnostic> {
    check_view(data.view(), schema, root, options)
}

/// Checks the value of a value-mode document against the type at `root`
/// (`None`: the schema's root type), with the rules of the deserializer
/// (`SPEC.md` 4.1). Ids name the value's own keys.
pub fn check_value(
    value: &Value,
    schema: &SchemaFile,
    root: Option<&str>,
    options: &CheckOptions,
) -> Vec<Diagnostic> {
    let variants = Variants::default();
    let mut checker = Checker {
        schema,
        options,
        data: DataView {
            base: value,
            contexts: &[],
            variants: &variants,
        },
        context: None,
        found: Vec::new(),
    };
    let root_type = TypeExpr::Named(root.unwrap_or(&schema.root).to_owned());
    checker.value(value, &root_type, "", None);
    checker.found
}

/// [`check`], on a [`DataView`].
pub fn check_view(
    data: DataView<'_>,
    schema: &SchemaFile,
    root: Option<&str>,
    options: &CheckOptions,
) -> Vec<Diagnostic> {
    let root_type = TypeExpr::Named(root.unwrap_or(&schema.root).to_owned());
    let contexts: Vec<Option<&ContextData>> = std::iter::once(None)
        .chain(data.contexts.iter().map(Some))
        .collect();
    // Each error with the contexts it occurs in.
    let mut found: Vec<(Diagnostic, Vec<usize>)> = Vec::new();
    for (index, context) in contexts.iter().enumerate() {
        let mut checker = Checker {
            schema,
            options,
            data,
            context: *context,
            found: Vec::new(),
        };
        for key in data.keys(*context) {
            if let Some(value) = data.value(&key, *context) {
                checker.value(value, &root_type, &key, None);
            }
        }
        for diagnostic in checker.found {
            match found.iter_mut().find(|(d, _)| *d == diagnostic) {
                Some((_, in_contexts)) => in_contexts.push(index),
                None => found.push((diagnostic, vec![index])),
            }
        }
    }
    // An error in only some contexts says in which (`SPEC.md` 12.5).
    found
        .into_iter()
        .map(|(mut diagnostic, in_contexts)| {
            if in_contexts.len() < contexts.len() {
                let mut names: Vec<String> = in_contexts
                    .iter()
                    .take(3)
                    .map(|&i| match contexts[i] {
                        Some(context) => data.names(&context.context),
                        None => "{}".to_owned(),
                    })
                    .collect();
                if in_contexts.len() > 3 {
                    names.push(format!("and {} more", in_contexts.len() - 3));
                }
                diagnostic.message = format!("in {}: {}", names.join(", "), diagnostic.message);
            }
            diagnostic
        })
        .collect()
}

struct Checker<'a> {
    schema: &'a SchemaFile,
    options: &'a CheckOptions,
    data: DataView<'a>,
    context: Option<&'a ContextData>,
    found: Vec<Diagnostic>,
}

fn found(value: &Value) -> String {
    match &value.kind {
        ValueKind::Int(i) => i.to_string(),
        ValueKind::Float(f) => crol_eval::format_float(*f),
        ValueKind::Bool(b) => b.to_string(),
        ValueKind::String(s) => crol_eval::quote(s),
        ValueKind::Char(c) => crol_eval::quote_char(*c),
        ValueKind::UnitName(n) => format!("`{n}`"),
        ValueKind::NamedTuple(n, _) => format!("`{n}(...)`"),
        ValueKind::Keyed(Keyed { name: Some(n), .. }) => format!("`{n} {{ ... }}`"),
        kind => kind.kind_name().to_owned(),
    }
}

impl Checker<'_> {
    fn error(&mut self, span: Span, message: String, path: &str) {
        let mut diagnostic = Diagnostic::error(span, message);
        if !path.is_empty() {
            diagnostic.context = Some(path.to_owned());
        }
        self.found.push(diagnostic);
    }

    fn expected(&mut self, value: &Value, what: &str, path: &str) {
        self.error(
            value.span,
            format!("expected {what}, found {}", found(value)),
            path,
        );
    }

    fn value(&mut self, value: &Value, ty: &TypeExpr, path: &str, variant: Option<&str>) {
        let int =
            |min: i128, max: i128| matches!(value.kind, ValueKind::Int(i) if i >= min && i <= max);
        let ok = match ty {
            TypeExpr::Any => true,
            TypeExpr::Bool => matches!(value.kind, ValueKind::Bool(_)),
            TypeExpr::Char => matches!(value.kind, ValueKind::Char(_)),
            TypeExpr::String => matches!(value.kind, ValueKind::String(_) | ValueKind::Id(_)),
            TypeExpr::Unit => matches!(value.kind, ValueKind::Unit),
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
            TypeExpr::F32 | TypeExpr::F64 => {
                matches!(value.kind, ValueKind::Float(_) | ValueKind::Int(_))
            }
            TypeExpr::Bytes => {
                matches!(&value.kind, ValueKind::List(items) if items.iter().all(|i| matches!(i.kind, ValueKind::Int(0..=255))))
            }
            TypeExpr::Id(refers) => {
                if let ValueKind::Id(id) = &value.kind {
                    self.id(value, id, refers, path);
                    true
                } else {
                    false
                }
            }
            TypeExpr::Option(inner) => match &value.kind {
                ValueKind::UnitName(n) if n == "None" => true,
                ValueKind::NamedTuple(n, items) if n == "Some" && items.len() == 1 => {
                    self.value(&items[0], inner, path, None);
                    true
                }
                _ if self.options.implicit_some => {
                    self.value(value, inner, path, None);
                    true
                }
                _ => false,
            },
            TypeExpr::List(inner) | TypeExpr::Set(inner) => match &value.kind {
                ValueKind::List(items) => {
                    for (i, item) in items.iter().enumerate() {
                        self.value(item, inner, &format!("{path}[{i}]"), None);
                    }
                    true
                }
                _ => false,
            },
            TypeExpr::Array(inner, n) => match &value.kind {
                ValueKind::Tuple(items) if items.len() == *n => {
                    for (i, item) in items.iter().enumerate() {
                        self.value(item, inner, &format!("{path}.{i}"), None);
                    }
                    true
                }
                _ => false,
            },
            TypeExpr::Tuple(types) => match &value.kind {
                ValueKind::Tuple(items) if items.len() == types.len() => {
                    for (i, (item, ty)) in items.iter().zip(types).enumerate() {
                        self.value(item, ty, &format!("{path}.{i}"), None);
                    }
                    true
                }
                ValueKind::Unit if types.is_empty() => true,
                _ => false,
            },
            TypeExpr::Map(key_type, value_type) => match &value.kind {
                ValueKind::Keyed(keyed) => {
                    for (key, item) in &keyed.entries {
                        // Integer keys go into integer types; string keys into
                        // strings, unit variants and ids (`SPEC.md` 10.1).
                        let integer_type = is_integer(key_type);
                        let key_ok = match key {
                            Key::Int(_) => integer_type || **key_type == TypeExpr::Any,
                            Key::Str(_) => !integer_type,
                        };
                        if !key_ok {
                            self.error(
                                item.span,
                                format!("the key {key} is not a key of this map"),
                                path,
                            );
                        }
                        self.value(item, value_type, &format!("{path}.{key}"), None);
                    }
                    true
                }
                _ => false,
            },
            TypeExpr::Named(type_path) => return self.named(value, type_path, path, variant),
        };
        if !ok {
            let what = describe(ty);
            self.expected(value, &what, path);
        }
    }

    fn named(&mut self, value: &Value, type_path: &str, path: &str, variant: Option<&str>) {
        let Some(definition) = self.schema.get(type_path) else {
            // A type the schema does not describe is not checked.
            return;
        };
        match definition {
            TypeDef::Struct {
                name,
                fields,
                explicit_defaults,
                ..
            } => {
                let ValueKind::Keyed(keyed) = &value.kind else {
                    return self.expected(value, &format!("`{name} {{ ... }}`"), path);
                };
                if let Some(written) = &keyed.name
                    && written != name
                    && Some(written.as_str()) != variant
                {
                    return self.error(
                        value.span,
                        format!("expected a `{name}`, found a `{written}`"),
                        path,
                    );
                }
                let flattened = fields.values().any(|f| f.flatten);
                for (key, item) in &keyed.entries {
                    let Key::Str(field_name) = key else {
                        self.error(
                            item.span,
                            format!("`{name}` has named fields; the key {key} is not one"),
                            path,
                        );
                        continue;
                    };
                    let field = fields
                        .get(field_name)
                        .or_else(|| fields.values().find(|f| f.aliases.contains(field_name)));
                    match field {
                        Some(field) => self.value(item, &field.ty, &join(path, field_name), None),
                        None if self.options.allow_unknown_fields || flattened => {}
                        None => {
                            // As serde lists them: each field's aliases, then its name.
                            let names: Vec<&str> = fields
                                .iter()
                                .flat_map(|(n, f)| {
                                    f.aliases.iter().map(String::as_str).chain([n.as_str()])
                                })
                                .collect();
                            self.error(
                                item.span,
                                format!(
                                    "`{name}` has no field `{field_name}`; its fields are `{}`",
                                    names.join("`, `")
                                ),
                                &join(path, field_name),
                            );
                        }
                    }
                }
                // A field is written when its name or an alias is.
                let present = |f: &str, field: &Field| {
                    std::iter::once(f)
                        .chain(field.aliases.iter().map(String::as_str))
                        .any(|n| {
                            keyed.entries.contains_key(&Key::Str(n.to_owned()))
                                || keyed.defaulted.contains(&Key::Str(n.to_owned()))
                        })
                };
                // Fields may be left out with `..`, or without it when
                // neither the type nor the options ask for it.
                let may_leave_out =
                    keyed.defaults || !explicit_defaults.unwrap_or(self.options.require_complete);
                let missing: Vec<&str> = fields
                    .iter()
                    .filter(|(f, field)| !field.flatten && !present(f, field))
                    .filter(|(_, field)| {
                        // Serde fills fields that have defaults.
                        !may_leave_out || field.default == FieldDefault::Required
                    })
                    .map(|(f, _)| f.as_str())
                    .collect();
                if !missing.is_empty() {
                    if may_leave_out {
                        self.error(value.span, format!("missing field `{}`", missing[0]), path);
                    } else {
                        let s = if missing.len() == 1 { "" } else { "s" };
                        self.error(
                            value.span,
                            format!(
                                "`{name}` is missing the field{s} `{}`; write {}, or end the block with `..` to leave out fields that have defaults",
                                missing.join("`, `"),
                                if missing.len() == 1 { "it" } else { "them" },
                            ),
                            path,
                        );
                    }
                }
            }
            TypeDef::Newtype { name, inner, .. } => match &value.kind {
                ValueKind::NamedTuple(written, items) if written == name && items.len() == 1 => {
                    self.value(&items[0], inner, path, None);
                }
                _ => self.value(value, inner, path, None),
            },
            TypeDef::Transparent { inner, .. } => self.value(value, inner, path, None),
            TypeDef::TupleStruct { name, fields, .. } => {
                let items = match &value.kind {
                    ValueKind::NamedTuple(written, items) if written == name => items,
                    ValueKind::Tuple(items) => items,
                    _ => return self.expected(value, &format!("`{name}(...)`"), path),
                };
                if items.len() != fields.len() {
                    return self.expected(
                        value,
                        &format!("`{name}(...)` with {} elements", fields.len()),
                        path,
                    );
                }
                for (i, (item, ty)) in items.iter().zip(fields).enumerate() {
                    self.value(item, ty, &format!("{path}.{i}"), None);
                }
            }
            TypeDef::UnitStruct { name, .. } => {
                if !matches!(&value.kind, ValueKind::Unit)
                    && !matches!(&value.kind, ValueKind::UnitName(n) if n == name)
                {
                    self.expected(value, &format!("`{name}`"), path);
                }
            }
            TypeDef::Enum {
                name,
                repr,
                variants,
                ..
            } => {
                if *repr != Repr::External {
                    // Tagged and untagged enums go through serde's `any`;
                    // their shapes are not checked.
                    return;
                }
                let written = match &value.kind {
                    ValueKind::UnitName(v) | ValueKind::NamedTuple(v, _) => v,
                    ValueKind::Keyed(Keyed { name: Some(v), .. }) => v,
                    _ => return self.expected(value, &format!("a `{name}` variant"), path),
                };
                let Some((variant_name, variant)) = variants
                    .iter()
                    .find(|(n, v)| *n == written || v.aliases.contains(written))
                else {
                    let names: Vec<&str> = variants.keys().map(String::as_str).collect();
                    return self.error(
                        value.span,
                        format!(
                            "`{name}` has no variant `{written}`; its variants are `{}`",
                            names.join("`, `")
                        ),
                        path,
                    );
                };
                match (&variant.shape, &value.kind) {
                    (VariantShape::Unit, ValueKind::UnitName(_)) => {}
                    (VariantShape::Newtype(inner), ValueKind::NamedTuple(_, items))
                        if items.len() == 1 =>
                    {
                        self.value(&items[0], inner, path, None);
                    }
                    // `V { ... }` as a newtype variant (`SPEC.md` 10.4).
                    (VariantShape::Newtype(inner), ValueKind::Keyed(_)) => {
                        self.value(value, inner, path, Some(variant_name));
                    }
                    (VariantShape::Tuple(types), ValueKind::NamedTuple(_, items))
                        if items.len() == types.len() =>
                    {
                        for (i, (item, ty)) in items.iter().zip(types).enumerate() {
                            self.value(item, ty, &format!("{path}.{i}"), None);
                        }
                    }
                    (VariantShape::Struct(fields), ValueKind::Keyed(_)) => {
                        let definition = TypeDef::Struct {
                            name: variant_name.clone(),
                            doc: String::new(),
                            source: (String::new(), 0),
                            fields: fields.clone(),
                            deny_unknown_fields: false,
                            default: None,
                            explicit_defaults: variant.explicit_defaults,
                        };
                        let mut schema = self.schema.clone();
                        let synthetic = format!("{type_path}::{variant_name}");
                        schema.types.insert(synthetic.clone(), definition);
                        let mut inner = Checker {
                            schema: &schema,
                            options: self.options,
                            data: self.data,
                            context: self.context,
                            found: Vec::new(),
                        };
                        inner.named(value, &synthetic, path, None);
                        self.found.append(&mut inner.found);
                    }
                    _ => self.expected(
                        value,
                        &format!("the variant `{variant_name}` of `{name}` in its shape"),
                        path,
                    ),
                }
            }
        }
    }

    /// An id must name a root key in the same context, of an allowed type.
    fn id(&mut self, value: &Value, id: &str, refers: &[String], path: &str) {
        let Some(target) = self.data.value(id, self.context) else {
            return self.error(
                value.span,
                format!("the id `@{id}` names no root key"),
                path,
            );
        };
        if refers.is_empty() {
            return;
        }
        let target_name = match &target.kind {
            ValueKind::Keyed(keyed) => keyed.name.as_deref(),
            ValueKind::NamedTuple(n, _) | ValueKind::UnitName(n) => Some(n.as_str()),
            _ => None,
        };
        let allowed: Vec<&str> = refers
            .iter()
            .filter_map(|p| self.schema.get(p).map(TypeDef::name))
            .collect();
        if !target_name.is_some_and(|n| allowed.contains(&n)) {
            self.error(
                value.span,
                format!(
                    "the id `@{id}` names {}, but it may only name {}",
                    target_name.map_or_else(
                        || "a value without a type name".to_owned(),
                        |n| format!("a `{n}`")
                    ),
                    allowed
                        .iter()
                        .map(|n| format!("`{n}`"))
                        .collect::<Vec<_>>()
                        .join(" or ")
                ),
                path,
            );
        }
    }
}

fn is_integer(ty: &TypeExpr) -> bool {
    matches!(
        ty,
        TypeExpr::I8
            | TypeExpr::I16
            | TypeExpr::I32
            | TypeExpr::I64
            | TypeExpr::I128
            | TypeExpr::U8
            | TypeExpr::U16
            | TypeExpr::U32
            | TypeExpr::U64
            | TypeExpr::U128
    )
}

fn join(path: &str, field: &str) -> String {
    if path.is_empty() {
        field.to_owned()
    } else {
        format!("{path}.{field}")
    }
}

fn describe(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Bool => "a boolean".into(),
        TypeExpr::Char => "a char".into(),
        TypeExpr::String => "a string".into(),
        TypeExpr::Unit => "`()`".into(),
        TypeExpr::I8 => "i8".into(),
        TypeExpr::I16 => "i16".into(),
        TypeExpr::I32 => "i32".into(),
        TypeExpr::I64 => "i64".into(),
        TypeExpr::I128 => "i128".into(),
        TypeExpr::U8 => "u8".into(),
        TypeExpr::U16 => "u16".into(),
        TypeExpr::U32 => "u32".into(),
        TypeExpr::U64 => "u64".into(),
        TypeExpr::U128 => "u128".into(),
        TypeExpr::F32 | TypeExpr::F64 => "a float".into(),
        TypeExpr::Bytes => "a list of bytes".into(),
        TypeExpr::Id(_) => "an id (`@name`)".into(),
        TypeExpr::Option(_) => "`Some(...)` or `None`".into(),
        TypeExpr::List(_) | TypeExpr::Set(_) => "a list".into(),
        TypeExpr::Array(_, n) => format!("a tuple of {n}"),
        TypeExpr::Tuple(items) => format!("a tuple of {}", items.len()),
        TypeExpr::Map(..) => "a keyed value `{ ... }`".into(),
        TypeExpr::Named(path) => format!("a `{path}`"),
        TypeExpr::Any => "anything".into(),
    }
}
