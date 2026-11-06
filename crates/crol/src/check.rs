//! `crol::check`: the deserializer's rules applied to evaluated data with
//! only a schema, and id checks (`SPEC.md` 10.9).

use std::rc::Rc;

use crol_eval::{Context, ContextData, Diagnostic, Key, Span, Value, ValueKind, Variants};
use indexmap::IndexMap;
use rustc_hash::FxHashMap;

use crate::compose::{CrolData, VariantName};
use crate::schema::{
    DataRoot, Field, FieldDefault, Repr, SchemaFile, TypeDef, TypeExpr, VariantShape,
};

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
        let key = Key::Str(key.into());
        match context {
            Some(data) => data.get(self.base, &key),
            None => match &self.base.kind {
                ValueKind::Keyed(keyed) => keyed.entries.get(&key),
                _ => None,
            },
        }
    }

    fn names(&self, context: &Context) -> String {
        format!("{{{}}}", self.variants.chosen_names(context).join(", "))
    }
}

/// Checks evaluated data against a schema (`SPEC.md` 10.9): the composed root
/// against the type at `root` (`None`: the schema's root type) if it is the
/// whole document, else each root value (`SPEC.md` 4.1), with the rules of
/// the deserializer, and every id: it must name a root key in the same
/// context, of one of the types it may refer to. Every evaluated context is
/// checked.
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
    let data = DataView {
        base: value,
        contexts: &[],
        variants: &variants,
    };
    let root_type = TypeExpr::Named(root.unwrap_or(&schema.root).to_owned());
    let mut checker = Checker::new(schema, options, data, None);
    checker.value(value, &root_type, Path::Root(""), None);
    checker.found
}

/// The type a value-mode document's top-level type name chooses in a schema,
/// as the path to give [`check_value`]: `None` if it has no name, or the
/// name is not one type's (`SPEC.md` 4.1).
pub fn value_type(value: &Value, schema: &SchemaFile) -> Option<String> {
    let name = match &value.kind {
        ValueKind::Keyed(keyed) => keyed.name.as_deref(),
        ValueKind::NamedTuple(name, _) | ValueKind::UnitName(name) => Some(&**name),
        _ => None,
    }?;
    let mut found = schema.by_name(name).map(|(path, _)| path.to_owned());
    found.next().filter(|_| found.next().is_none())
}

/// [`check`], on a [`DataView`].
pub fn check_view(
    data: DataView<'_>,
    schema: &SchemaFile,
    root: Option<&str>,
    options: &CheckOptions,
) -> Vec<Diagnostic> {
    let root_type = TypeExpr::Named(root.unwrap_or(&schema.root).to_owned());
    let whole = schema.data_root(root) == DataRoot::Document;
    let contexts: Vec<Option<&ContextData>> = std::iter::once(None)
        .chain(data.contexts.iter().map(Some))
        .collect();
    let found = if whole {
        check_whole(data, schema, &root_type, options, &contexts)
    } else {
        check_keys(data, schema, &root_type, options, &contexts)
    };
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

/// Each error with the indices of the contexts it occurs in.
type Found = Vec<(Diagnostic, Vec<usize>)>;

/// Adds what context `index` found.
fn merge(found: &mut Found, diagnostics: Vec<Diagnostic>, index: usize) {
    for diagnostic in diagnostics {
        match found.iter_mut().find(|(d, _)| *d == diagnostic) {
            Some((_, in_contexts)) => {
                if in_contexts.last() != Some(&index) {
                    in_contexts.push(index);
                }
            }
            None => found.push((diagnostic, vec![index])),
        }
    }
}

/// The root as a whole, in each context.
fn check_whole(
    data: DataView<'_>,
    schema: &SchemaFile,
    root_type: &TypeExpr,
    options: &CheckOptions,
    contexts: &[Option<&ContextData>],
) -> Found {
    let mut found = Found::new();
    for (index, context) in contexts.iter().enumerate() {
        let mut checker = Checker::new(schema, options, data, *context);
        let root = context.map(|data_in| data_in.root(data.base));
        checker.value(
            root.as_ref().unwrap_or(data.base),
            root_type,
            Path::Root(""),
            None,
        );
        merge(&mut found, checker.found, index);
    }
    found
}

/// Each root value, in each context. A context holds what differs from the
/// base, and a value that does not differ checks the same there, but for the
/// ids in it, which name root keys of that context.
fn check_keys(
    data: DataView<'_>,
    schema: &SchemaFile,
    root_type: &TypeExpr,
    options: &CheckOptions,
    contexts: &[Option<&ContextData>],
) -> Found {
    let ValueKind::Keyed(root) = &data.base.kind else {
        return Found::new();
    };
    let mut found = Found::new();
    // What each base value found, and the ids in it.
    let mut base = Vec::with_capacity(root.entries.len());
    let mut checker = Checker::new(schema, options, data, None);
    for (key, value) in &root.entries {
        let Key::Str(name) = key else { continue };
        checker.ids = Some(Vec::new());
        checker.value(value, root_type, Path::Root(name), None);
        let diagnostics = std::mem::take(&mut checker.found);
        let ids = checker.ids.take().unwrap_or_default();
        base.push((key, diagnostics.clone(), ids));
        merge(&mut found, diagnostics, 0);
    }
    // Each context on its own, and with enough to do on a thread of its own.
    let check_context = |context: &&ContextData| {
        let mut checker = Checker::new(schema, options, data, Some(context));
        let mut same = Vec::new();
        for (key, diagnostics, ids) in &base {
            if context.changes.contains_key(*key) {
                continue;
            }
            same.extend(diagnostics.iter().cloned());
            for id in ids {
                checker.id(id.value, id.id, id.refers, Path::Root(&id.path));
            }
        }
        for (key, change) in &context.changes {
            if let (Key::Str(name), Some(value)) = (key, change) {
                checker.value(value, root_type, Path::Root(name), None);
            }
        }
        same.append(&mut checker.found);
        same
    };
    let others: Vec<&ContextData> = contexts.iter().skip(1).flatten().copied().collect();
    let threaded = root.entries.len() >= THREADED_KEYS && others.len() > 1;
    let results: Vec<Vec<Diagnostic>> = if threaded {
        std::thread::scope(|scope| {
            let check_context = &check_context;
            let threads: Vec<_> = (others.iter())
                .map(|context| scope.spawn(move || check_context(context)))
                .collect();
            (threads.into_iter())
                .map(|thread| {
                    thread
                        .join()
                        .unwrap_or_else(|e| std::panic::resume_unwind(e))
                })
                .collect()
        })
    } else {
        others.iter().map(check_context).collect()
    };
    for (index, diagnostics) in results.into_iter().enumerate() {
        merge(&mut found, diagnostics, index + 1);
    }
    found
}

/// How many root keys there must be before contexts are checked on threads:
/// below this, starting them costs more than they save.
const THREADED_KEYS: usize = 2_000;

/// Where a value is, for the diagnostics of the few values that have any: it
/// costs nothing to go deeper, and is only written out if needed.
#[derive(Clone, Copy)]
enum Path<'p> {
    /// A root key, or nothing for the root.
    Root(&'p str),
    Field(&'p Path<'p>, &'p str),
    Key(&'p Path<'p>, &'p Key),
    /// An element of a list.
    Index(&'p Path<'p>, usize),
    /// An element of a tuple.
    Element(&'p Path<'p>, usize),
}

impl Path<'_> {
    fn is_empty(&self) -> bool {
        matches!(self, Path::Root(""))
    }
}

impl std::fmt::Display for Path<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Path::Root(key) => f.write_str(key),
            Path::Field(path, field) if path.is_empty() => f.write_str(field),
            Path::Field(path, field) => write!(f, "{path}.{field}"),
            Path::Key(path, key) if path.is_empty() => write!(f, "{key}"),
            Path::Key(path, key) => write!(f, "{path}.{key}"),
            Path::Index(path, index) => write!(f, "{path}[{index}]"),
            Path::Element(path, index) => write!(f, "{path}.{index}"),
        }
    }
}

/// An id met in a value, to check in the other contexts.
struct IdUse<'a> {
    value: &'a Value,
    id: &'a str,
    refers: &'a [String],
    path: String,
}

struct Checker<'a> {
    schema: &'a SchemaFile,
    options: &'a CheckOptions,
    data: DataView<'a>,
    context: Option<&'a ContextData>,
    found: Vec<Diagnostic>,
    /// The ids met, if they are being recorded.
    ids: Option<Vec<IdUse<'a>>>,
    /// The types by path.
    types: FxHashMap<&'a str, &'a TypeDef>,
    /// What is looked up in each struct met, by its fields.
    structs: FxHashMap<*const IndexMap<String, Field>, Rc<StructIndex<'a>>>,
}

/// A struct's fields as checking looks them up: for every value of it, so
/// not one by one, and not with the hash of a map open to attack.
struct StructIndex<'a> {
    /// By name and alias; a name comes before an alias.
    by_name: FxHashMap<&'a str, &'a Field>,
    /// The fields a value has to have.
    required: Vec<(&'a str, &'a Field)>,
    flattened: bool,
}

impl<'a> StructIndex<'a> {
    fn new(fields: &'a IndexMap<String, Field>) -> Self {
        let aliases = fields.values().flat_map(|field| {
            field
                .aliases
                .iter()
                .map(move |alias| (alias.as_str(), field))
        });
        let names = fields.iter().map(|(name, field)| (name.as_str(), field));
        Self {
            by_name: aliases.chain(names).collect(),
            required: fields
                .iter()
                .filter(|(_, field)| field.default == FieldDefault::Required && !field.flatten)
                .map(|(name, field)| (name.as_str(), field))
                .collect(),
            flattened: fields.values().any(|field| field.flatten),
        }
    }
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
        ValueKind::Keyed(keyed) if keyed.name.is_some() => {
            format!("`{} {{ ... }}`", keyed.name.as_deref().unwrap_or_default())
        }
        kind => kind.kind_name().to_owned(),
    }
}

impl<'a> Checker<'a> {
    fn new(
        schema: &'a SchemaFile,
        options: &'a CheckOptions,
        data: DataView<'a>,
        context: Option<&'a ContextData>,
    ) -> Self {
        Self {
            schema,
            options,
            data,
            context,
            found: Vec::new(),
            ids: None,
            types: (schema.types.iter())
                .map(|(path, definition)| (path.as_str(), definition))
                .collect(),
            structs: FxHashMap::default(),
        }
    }

    fn struct_index(&mut self, fields: &'a IndexMap<String, Field>) -> Rc<StructIndex<'a>> {
        let key: *const IndexMap<String, Field> = fields;
        let index = self.structs.entry(key);
        Rc::clone(index.or_insert_with(|| Rc::new(StructIndex::new(fields))))
    }

    fn error(&mut self, span: Span, message: String, path: Path<'_>) {
        let mut diagnostic = Diagnostic::error(span, message);
        if !path.is_empty() {
            diagnostic.context = Some(path.to_string());
        }
        self.found.push(diagnostic);
    }

    fn expected(&mut self, value: &Value, what: &str, path: Path<'_>) {
        self.error(
            value.span,
            format!("expected {what}, found {}", found(value)),
            path,
        );
    }

    fn value(&mut self, value: &'a Value, ty: &'a TypeExpr, path: Path<'_>, variant: Option<&str>) {
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
                    if let Some(ids) = &mut self.ids {
                        ids.push(IdUse {
                            value,
                            id,
                            refers,
                            path: path.to_string(),
                        });
                    }
                    self.id(value, id, refers, path);
                    true
                } else {
                    false
                }
            }
            TypeExpr::Option(inner) => match &value.kind {
                ValueKind::UnitName(n) if &**n == "None" => true,
                ValueKind::NamedTuple(n, items) if &**n == "Some" && items.len() == 1 => {
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
                        self.value(item, inner, Path::Index(&path, i), None);
                    }
                    true
                }
                _ => false,
            },
            TypeExpr::Array(inner, n) => match &value.kind {
                ValueKind::Tuple(items) if items.len() == *n => {
                    for (i, item) in items.iter().enumerate() {
                        self.value(item, inner, Path::Element(&path, i), None);
                    }
                    true
                }
                _ => false,
            },
            TypeExpr::Tuple(types) => match &value.kind {
                ValueKind::Tuple(items) if items.len() == types.len() => {
                    for (i, (item, ty)) in items.iter().zip(types).enumerate() {
                        self.value(item, ty, Path::Element(&path, i), None);
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
                        self.value(item, value_type, Path::Key(&path, key), None);
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

    fn named(&mut self, value: &'a Value, type_path: &str, path: Path<'_>, variant: Option<&str>) {
        let Some(&definition) = self.types.get(type_path) else {
            // A type the schema does not describe is not checked.
            return;
        };
        match definition {
            TypeDef::Struct {
                name,
                fields,
                explicit_defaults,
                ..
            } => self.fields(value, name, fields, *explicit_defaults, path, variant),
            TypeDef::Newtype { name, inner, .. } => match &value.kind {
                ValueKind::NamedTuple(written, items)
                    if **written == **name && items.len() == 1 =>
                {
                    self.value(&items[0], inner, path, None);
                }
                _ => self.value(value, inner, path, None),
            },
            TypeDef::Transparent { inner, .. } => self.value(value, inner, path, None),
            TypeDef::TupleStruct { name, fields, .. } => {
                let items = match &value.kind {
                    ValueKind::NamedTuple(written, items) if **written == **name => items,
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
                    self.value(item, ty, Path::Element(&path, i), None);
                }
            }
            TypeDef::UnitStruct { name, .. } => {
                if !matches!(&value.kind, ValueKind::Unit)
                    && !matches!(&value.kind, ValueKind::UnitName(n) if **n == **name)
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
                let written: &str = match &value.kind {
                    ValueKind::UnitName(v) | ValueKind::NamedTuple(v, _) => v,
                    ValueKind::Keyed(keyed) if keyed.name.is_some() => {
                        keyed.name.as_deref().expect("checked to be there")
                    }
                    _ => return self.expected(value, &format!("a `{name}` variant"), path),
                };
                let Some((variant_name, variant)) = variants
                    .iter()
                    .find(|(n, v)| *n == written || v.aliases.iter().any(|a| a == written))
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
                            self.value(item, ty, Path::Element(&path, i), None);
                        }
                    }
                    (VariantShape::Struct(fields), ValueKind::Keyed(_)) => {
                        let explicit = variant.explicit_defaults;
                        self.fields(value, variant_name, fields, explicit, path, None);
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

    /// Checks a struct, or a variant with named fields.
    fn fields(
        &mut self,
        value: &'a Value,
        name: &str,
        fields: &'a IndexMap<String, Field>,
        explicit_defaults: Option<bool>,
        path: Path<'_>,
        variant: Option<&str>,
    ) {
        let ValueKind::Keyed(keyed) = &value.kind else {
            return self.expected(value, &format!("`{name} {{ ... }}`"), path);
        };
        if let Some(written) = &keyed.name
            && **written != *name
            && Some(&**written) != variant
        {
            return self.error(
                value.span,
                format!("expected a `{name}`, found a `{written}`"),
                path,
            );
        }
        let index = self.struct_index(fields);
        for (key, item) in &keyed.entries {
            let Key::Str(field_name) = key else {
                self.error(
                    item.span,
                    format!("`{name}` has named fields; the key {key} is not one"),
                    path,
                );
                continue;
            };
            let field = index.by_name.get(&**field_name).copied();
            match field {
                Some(field) => self.value(item, &field.ty, Path::Field(&path, field_name), None),
                None if self.options.allow_unknown_fields || index.flattened => {}
                None => {
                    // As serde lists them: each field's aliases, then its name.
                    let names: Vec<&str> = fields
                        .iter()
                        .flat_map(|(n, f)| f.aliases.iter().map(String::as_str).chain([n.as_str()]))
                        .collect();
                    self.error(
                        item.span,
                        format!(
                            "`{name}` has no field `{field_name}`; its fields are `{}`",
                            names.join("`, `")
                        ),
                        Path::Field(&path, field_name),
                    );
                }
            }
        }
        // A field is written when its name or an alias is.
        let written = |f: &str, field: &Field| {
            keyed.has_name(f) || field.aliases.iter().any(|a| keyed.has_name(a))
        };
        let removed = |f: &str, field: &Field| {
            let is = |name: &str| {
                let same = |key: &Key| matches!(key, Key::Str(s) if &**s == name);
                keyed.defaulted.iter().any(same)
            };
            is(f) || field.aliases.iter().any(|a| is(a))
        };
        // Fields may be left out with `..`, or without it when
        // neither the type nor the options ask for it.
        let may_leave_out =
            keyed.defaults || !explicit_defaults.unwrap_or(self.options.require_complete);
        let left_out: Vec<&str> = if may_leave_out {
            Vec::new()
        } else {
            fields
                .iter()
                .filter(|(f, field)| !field.flatten && !written(f, field) && !removed(f, field))
                .map(|(f, _)| f.as_str())
                .collect()
        };
        // Serde fills fields that have defaults, also removed ones.
        let required = index.required.iter().find(|(f, field)| !written(f, field));
        if !may_leave_out && !left_out.is_empty() {
            let s = if left_out.len() == 1 { "" } else { "s" };
            self.error(
                value.span,
                format!(
                    "`{name}` is missing the field{s} `{}`; write {}, or end the block with `..` to leave out fields that have defaults",
                    left_out.join("`, `"),
                    if left_out.len() == 1 { "it" } else { "them" },
                ),
                path,
            );
        } else if let Some((field, _)) = required {
            self.error(value.span, format!("missing field `{field}`"), path);
        }
    }

    /// An id must name a root key in the same context, of an allowed type.
    fn id(&mut self, value: &Value, id: &str, refers: &[String], path: Path<'_>) {
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
            ValueKind::NamedTuple(n, _) | ValueKind::UnitName(n) => Some(&**n),
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
