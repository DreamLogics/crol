//! Constraints and sealed fields: rules on the values a schema's fields hold
//! (`SPEC.md` 10.12, 10.13), and the `schema.crod` of a layer, which defines
//! and extends constraints.

use std::borrow::Cow;
use std::collections::BTreeMap;

use crol_eval::{
    DataOptions, Diagnostic, Key, Layer, SCHEMA_DOCUMENT, Sources, Span, Value, ValueKind,
    compile_schema_documents, evaluate_compiled, print,
};

use crate::de;
use crate::raw::RawValue;
use crate::schema::{
    Constraint, ConstraintDef, DataRoot, Field, SchemaDefaults, SchemaFile, TypeDef, TypeExpr,
    VariantShape,
};

/// The text of a value, where it stands for a name.
fn text_of(value: &Value) -> Option<Cow<'_, str>> {
    match &value.kind {
        ValueKind::String(s) | ValueKind::UnitName(s) => Some(Cow::Borrowed(s)),
        ValueKind::Id(id) => Some(crate::id::id_text(id)),
        _ => None,
    }
}

fn number(value: &Value) -> Option<f64> {
    match value.kind {
        ValueKind::Int(i) => Some(i as f64),
        ValueKind::Float(f) => Some(f),
        _ => None,
    }
}

fn describe(value: &Value) -> String {
    print::to_text(value)
}

/// Why `value` breaks a constraint, if it does.
fn broken(
    name: &str,
    constraint: &Constraint,
    value: &Value,
    root: &dyn Fn(&str) -> Option<Value>,
) -> Option<String> {
    let outside = |min: Option<f64>, max: Option<f64>| {
        let Some(found) = number(value) else {
            return Some(format!(
                "`{name}` takes a number, not {}",
                value.kind.kind_name()
            ));
        };
        let text = |n: f64| {
            if n.fract() == 0.0 && n.abs() < 1e15 {
                (n as i64).to_string()
            } else {
                crol_eval::format_float(n)
            }
        };
        match (min, max) {
            (Some(min), _) if found < min => Some(format!(
                "{} is below {}, the minimum of `{name}`",
                describe(value),
                text(min)
            )),
            (_, Some(max)) if found > max => Some(format!(
                "{} is above {}, the maximum of `{name}`",
                describe(value),
                text(max)
            )),
            _ => None,
        }
    };
    match constraint {
        Constraint::Min(min) => outside(Some(*min), None),
        Constraint::Max(max) => outside(None, Some(*max)),
        Constraint::Range(min, max) => outside(Some(*min), Some(*max)),
        Constraint::OneOf(values) => {
            let same = |allowed: &RawValue| match (text_of(&allowed.0), text_of(value)) {
                (Some(a), Some(b)) => a == b,
                _ => allowed.0.equals(value),
            };
            (!values.iter().any(same)).then(|| {
                let allowed: Vec<String> = values.iter().map(|v| describe(&v.0)).collect();
                format!(
                    "{} is not one of {} (`{name}`)",
                    describe(value),
                    allowed.join(", ")
                )
            })
        }
        Constraint::Refers(types) => {
            let Some(id) = text_of(value) else {
                return Some(format!(
                    "`{name}` takes an id, not {}",
                    value.kind.kind_name()
                ));
            };
            let Some(target) = root(&id) else {
                return Some(format!("`{id}` names nothing (`{name}`)"));
            };
            let type_name = match &target.kind {
                ValueKind::Keyed(keyed) => keyed.name.as_deref(),
                ValueKind::NamedTuple(n, _) | ValueKind::UnitName(n) => Some(&**n),
                _ => None,
            };
            (!type_name.is_some_and(|n| types.iter().any(|t| t == n)))
                .then(|| format!("`{id}` is not a {} (`{name}`)", types.join(" or a ")))
        }
    }
}

/// `Some(x)`'s `x`, or the value itself.
fn unwrap_some(value: &Value) -> &Value {
    match &value.kind {
        ValueKind::NamedTuple(name, items) if &**name == "Some" && items.len() == 1 => &items[0],
        _ => value,
    }
}

fn is_none(value: &Value) -> bool {
    matches!(&value.kind, ValueKind::UnitName(name) if &**name == "None")
}

/// Walks a value by its type, for what a schema's fields say.
struct Walker<'a> {
    schema: &'a SchemaFile,
    root: &'a dyn Fn(&str) -> Option<Value>,
    found: Vec<(Span, String)>,
}

impl Walker<'_> {
    fn apply(&mut self, name: &str, value: &Value) {
        let Some(definition) = self.schema.constraints.get(name) else {
            return;
        };
        if let Some(message) = broken(name, &definition.constraint, value, self.root) {
            self.found.push((value.span, message));
        }
    }

    /// A field's constraint, on its value or, for a collection, on what it
    /// holds.
    fn constrain(&mut self, name: &str, ty: &TypeExpr, value: &Value) {
        match (ty, &value.kind) {
            (TypeExpr::Option(_), _) if is_none(value) => {}
            (TypeExpr::Option(inner), _) => self.constrain(name, inner, unwrap_some(value)),
            (
                TypeExpr::List(inner) | TypeExpr::Set(inner) | TypeExpr::Array(inner, _),
                ValueKind::List(items) | ValueKind::Tuple(items),
            ) => {
                for item in items {
                    self.constrain(name, inner, item);
                }
            }
            (TypeExpr::Map(_, inner), ValueKind::Keyed(keyed)) => {
                for item in keyed.entries.values() {
                    self.constrain(name, inner, item);
                }
            }
            _ => self.apply(name, value),
        }
    }

    fn key_constraint(&mut self, name: &str, value: &Value) {
        let ValueKind::Keyed(keyed) = &unwrap_some(value).kind else {
            return;
        };
        for key in keyed.entries.keys() {
            let kind = match key {
                Key::Str(text) => ValueKind::String(text.as_ref().into()),
                Key::Int(i) => ValueKind::Int(*i),
            };
            self.apply(
                name,
                &Value {
                    kind,
                    span: value.span,
                },
            );
        }
    }

    fn field(&mut self, field: &Field, value: &Value) {
        if let Some(name) = &field.constraint {
            self.constrain(name, &field.ty, value);
        }
        if let Some(name) = &field.key_constraint {
            self.key_constraint(name, value);
        }
        self.value(&field.ty, value);
    }

    fn fields<'f>(
        &mut self,
        fields: impl IntoIterator<Item = (&'f String, &'f Field)>,
        value: &Value,
    ) {
        let ValueKind::Keyed(keyed) = &value.kind else {
            return;
        };
        for (name, field) in fields {
            if field.flatten {
                self.value(&field.ty, value);
            } else if let Some(entry) = keyed.entries.get(&Key::Str(name.as_str().into())) {
                self.field(field, entry);
            }
        }
    }

    fn value(&mut self, ty: &TypeExpr, value: &Value) {
        match (ty, &value.kind) {
            (TypeExpr::Option(_), _) if is_none(value) => {}
            (TypeExpr::Option(inner), _) => self.value(inner, unwrap_some(value)),
            (
                TypeExpr::List(inner) | TypeExpr::Set(inner) | TypeExpr::Array(inner, _),
                ValueKind::List(items) | ValueKind::Tuple(items),
            ) => {
                for item in items {
                    self.value(inner, item);
                }
            }
            (TypeExpr::Tuple(types), ValueKind::Tuple(items)) => {
                for (ty, item) in types.iter().zip(items) {
                    self.value(ty, item);
                }
            }
            (TypeExpr::Map(_, inner), ValueKind::Keyed(keyed)) => {
                for item in keyed.entries.values() {
                    self.value(inner, item);
                }
            }
            (TypeExpr::Named(path), _) => self.named(path, value),
            _ => {}
        }
    }

    fn named(&mut self, path: &str, value: &Value) {
        let schema = self.schema;
        match schema.get(path) {
            Some(TypeDef::Struct { fields, .. }) => self.fields(fields, value),
            Some(
                TypeDef::Newtype {
                    inner, constraint, ..
                }
                | TypeDef::Transparent {
                    inner, constraint, ..
                },
            ) => {
                let value = match &value.kind {
                    ValueKind::NamedTuple(_, items) if items.len() == 1 => &items[0],
                    _ => value,
                };
                if let Some(name) = constraint {
                    self.constrain(name, inner, value);
                }
                self.value(inner, value);
            }
            Some(TypeDef::TupleStruct { fields, .. }) => {
                if let ValueKind::NamedTuple(_, items) | ValueKind::Tuple(items) = &value.kind {
                    for (ty, item) in fields.iter().zip(items) {
                        self.value(ty, item);
                    }
                }
            }
            Some(TypeDef::Enum { variants, .. }) => {
                let (written, items): (&str, &[Value]) = match &value.kind {
                    ValueKind::NamedTuple(name, items) => (name, items),
                    ValueKind::Keyed(keyed) => (keyed.name.as_deref().unwrap_or(""), &[]),
                    _ => return,
                };
                match variants.get(written).map(|v| &v.shape) {
                    Some(VariantShape::Struct(fields)) => self.fields(fields, value),
                    Some(VariantShape::Newtype(ty)) => {
                        self.value(ty, items.first().unwrap_or(value));
                    }
                    Some(VariantShape::Tuple(types)) => {
                        for (ty, item) in types.iter().zip(items) {
                            self.value(ty, item);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// The type of a root key's value, or the field it is of a root struct.
enum RootSlot<'a> {
    Field(&'a Field),
    Type(TypeExpr),
}

impl SchemaFile {
    fn root_slot(&self, root: Option<&str>, key: &str) -> Option<RootSlot<'_>> {
        let root = root.unwrap_or(&self.root);
        if self.data_root(Some(root)) != DataRoot::Document {
            return Some(RootSlot::Type(TypeExpr::Named(root.to_owned())));
        }
        let mut path = root;
        for _ in 0..64 {
            match self.get(path)? {
                TypeDef::Struct { fields, .. } => return fields.get(key).map(RootSlot::Field),
                TypeDef::Newtype { inner, .. } | TypeDef::Transparent { inner, .. } => {
                    match inner {
                        TypeExpr::Named(next) => path = next,
                        TypeExpr::Map(_, value) => return Some(RootSlot::Type((**value).clone())),
                        _ => return None,
                    }
                }
                _ => return None,
            }
        }
        None
    }

    fn sealed_anywhere(&self) -> bool {
        self.fields().any(|field| field.sealed)
    }

    /// Every field of every type.
    fn fields(&self) -> impl Iterator<Item = &Field> {
        self.types.values().flat_map(|def| {
            let mut fields: Vec<&Field> = Vec::new();
            match def {
                TypeDef::Struct { fields: own, .. } => fields.extend(own.values()),
                TypeDef::Enum { variants, .. } => {
                    for variant in variants.values() {
                        if let VariantShape::Struct(own) = &variant.shape {
                            fields.extend(own.values());
                        }
                    }
                }
                _ => {}
            }
            fields
        })
    }

    /// The constraints the types name.
    fn named_constraints(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .fields()
            .flat_map(|field| [&field.constraint, &field.key_constraint])
            .flatten()
            .map(String::as_str)
            .collect();
        for def in self.types.values() {
            if let TypeDef::Newtype {
                constraint: Some(name),
                ..
            }
            | TypeDef::Transparent {
                constraint: Some(name),
                ..
            } = def
            {
                names.push(name);
            }
        }
        names.sort_unstable();
        names.dedup();
        names
    }

    /// This schema with the `schema.crod` of each layer applied, lowest first
    /// (`SPEC.md` 10.12), and what is wrong with them. Use the schema it
    /// returns for evaluating `layers`.
    pub fn patched(
        &self,
        sources: &mut Sources,
        layers: &[Layer],
    ) -> (SchemaFile, Vec<Diagnostic>) {
        let mut schema = self.clone();
        let mut diagnostics = Vec::new();
        // What has no place of its own goes on the first document there is.
        let file = layers.iter().flat_map(|layer| &layer.documents).next();
        let nowhere = file.map_or(Span::detached(), |(_, file)| {
            Span::new(*file, Default::default())
        });
        let patches: Vec<Layer> = layers
            .iter()
            .filter_map(|layer| {
                let documents: Vec<_> = layer
                    .documents
                    .iter()
                    .filter(|(path, _)| path == SCHEMA_DOCUMENT)
                    .cloned()
                    .collect();
                (!documents.is_empty()).then(|| Layer {
                    name: layer.name.clone(),
                    documents,
                })
            })
            .collect();
        if !patches.is_empty() {
            let constraints = self.apply_patches(sources, patches, nowhere, &mut diagnostics);
            if let Some(constraints) = constraints {
                schema.constraints = constraints;
            }
        }
        if !diagnostics.iter().any(Diagnostic::is_error) {
            for name in schema.named_constraints() {
                if !schema.constraints.contains_key(name) {
                    diagnostics.push(Diagnostic::error(
                        nowhere,
                        format!("nothing defines the constraint `{name}`; define it in a layer's `schema.crod`, or with `#[crol(constraints(...))]`"),
                    ));
                }
            }
        }
        (schema, diagnostics)
    }

    fn apply_patches(
        &self,
        sources: &mut Sources,
        patches: Vec<Layer>,
        nowhere: Span,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<BTreeMap<String, ConstraintDef>> {
        // What the program defines is the layer below them all.
        let own: BTreeMap<&String, &Constraint> = self
            .constraints
            .iter()
            .map(|(name, definition)| (name, &definition.constraint))
            .collect();
        let own = crate::to_value(&own).ok()?;
        let text = format!("+constraints: {}", print::to_text(&own));
        let file = sources.add("<schema>", text);
        let mut layers = vec![Layer {
            name: "schema".to_owned(),
            documents: vec![("constraints.crod".to_owned(), file)],
        }];
        layers.extend(patches);

        let (compiled, mut found) = compile_schema_documents(sources, &layers);
        diagnostics.append(&mut found);
        let compiled = compiled?;
        let options = DataOptions::default()
            .with_contexts(Some(Vec::new()))
            .with_merge_root(true);
        let mut evaluation = evaluate_compiled(&compiled, &options);
        diagnostics.append(&mut evaluation.diagnostics);
        let root = evaluation.value?;
        if evaluation.variants.ids().next().is_some() {
            diagnostics.push(Diagnostic::error(
                nowhere,
                "a `schema.crod` cannot use variants",
            ));
            return None;
        }
        let ValueKind::Keyed(root) = root.kind else {
            return None;
        };
        let mut result = BTreeMap::new();
        for (key, value) in root.entries {
            if key != Key::Str("constraints".into()) {
                diagnostics.push(Diagnostic::error(
                    value.span,
                    format!("a `schema.crod` holds only `constraints`, not `{key}`"),
                ));
                continue;
            }
            let ValueKind::Keyed(constraints) = value.kind else {
                continue;
            };
            for (name, value) in constraints.entries {
                let name = name.to_string();
                let read: Result<(Constraint, _), _> =
                    crate::deserialize(&value, sources, de::Options::default(), Some(&name));
                match read {
                    Ok((constraint, _)) => {
                        let sealed = self.constraints.get(&name).is_some_and(|d| d.sealed);
                        result.insert(name, ConstraintDef { constraint, sealed });
                    }
                    Err(error) => diagnostics.push(Diagnostic::error(
                        value.span,
                        format!("`{name}`: {}", error.message()),
                    )),
                }
            }
        }
        for (name, definition) in &self.constraints {
            if !definition.sealed {
                continue;
            }
            if result.get(name).map(|d| &d.constraint) != Some(&definition.constraint) {
                diagnostics.push(Diagnostic::error(
                    nowhere,
                    format!("the constraint `{name}` is sealed: the program defines it, and a `schema.crod` cannot change it"),
                ));
            }
        }
        Some(result)
    }
}

impl SchemaDefaults {
    pub(crate) fn any_constraints(&self) -> bool {
        !self.schema.constraints.is_empty()
    }

    pub(crate) fn any_sealed(&self) -> bool {
        self.schema.sealed_anywhere()
    }

    pub(crate) fn broken(
        &self,
        key: &str,
        value: &Value,
        root: &dyn Fn(&str) -> Option<Value>,
    ) -> Vec<(Span, String)> {
        let mut walker = Walker {
            schema: &self.schema,
            root,
            found: Vec::new(),
        };
        match self.schema.root_slot(self.root.as_deref(), key) {
            Some(RootSlot::Field(field)) => walker.field(field, value),
            Some(RootSlot::Type(ty)) => walker.value(&ty, value),
            None => {}
        }
        walker.found
    }

    pub(crate) fn sealed(&self, key: &str, before: &Value, after: &Value) -> Vec<Vec<String>> {
        let mut found = Vec::new();
        let mut path = Vec::new();
        match self.schema.root_slot(self.root.as_deref(), key) {
            Some(RootSlot::Field(field)) if field.sealed => {
                if !before.equals(after) {
                    found.push(Vec::new());
                }
            }
            Some(RootSlot::Field(field)) => {
                self.sealed_in(&field.ty, before, after, &mut path, &mut found);
            }
            Some(RootSlot::Type(ty)) => self.sealed_in(&ty, before, after, &mut path, &mut found),
            None => {}
        }
        found
    }

    fn sealed_fields<'f>(
        &self,
        fields: impl IntoIterator<Item = (&'f String, &'f Field)>,
        before: &Value,
        after: &Value,
        path: &mut Vec<String>,
        found: &mut Vec<Vec<String>>,
    ) {
        let (ValueKind::Keyed(a), ValueKind::Keyed(b)) = (&before.kind, &after.kind) else {
            return;
        };
        for (name, field) in fields {
            if field.flatten {
                self.sealed_in(&field.ty, before, after, path, found);
                continue;
            }
            let key = Key::Str(name.as_str().into());
            let (old, new) = (a.entries.get(&key), b.entries.get(&key));
            path.push(name.clone());
            if field.sealed {
                let same = match (old, new) {
                    (Some(old), Some(new)) => old.equals(new),
                    (None, None) => true,
                    _ => false,
                };
                if !same {
                    found.push(path.clone());
                }
            } else if let (Some(old), Some(new)) = (old, new) {
                self.sealed_in(&field.ty, old, new, path, found);
            }
            path.pop();
        }
    }

    fn sealed_in(
        &self,
        ty: &TypeExpr,
        before: &Value,
        after: &Value,
        path: &mut Vec<String>,
        found: &mut Vec<Vec<String>>,
    ) {
        match (ty, &before.kind, &after.kind) {
            (TypeExpr::Option(inner), ..) => {
                self.sealed_in(inner, unwrap_some(before), unwrap_some(after), path, found);
            }
            (TypeExpr::Map(_, inner), ValueKind::Keyed(a), ValueKind::Keyed(b)) => {
                for (key, old) in &a.entries {
                    if let Some(new) = b.entries.get(key) {
                        path.push(key.to_string());
                        self.sealed_in(inner, old, new, path, found);
                        path.pop();
                    }
                }
            }
            (TypeExpr::Named(type_path), ..) => match self.schema.get(type_path) {
                Some(TypeDef::Struct { fields, .. }) => {
                    self.sealed_fields(fields, before, after, path, found);
                }
                Some(TypeDef::Newtype { inner, .. } | TypeDef::Transparent { inner, .. }) => {
                    self.sealed_in(inner, before, after, path, found);
                }
                Some(TypeDef::Enum { variants, .. }) => {
                    let name = |value: &Value| match &value.kind {
                        ValueKind::Keyed(keyed) => keyed.name.clone(),
                        ValueKind::NamedTuple(name, _) => Some(name.clone()),
                        _ => None,
                    };
                    let (Some(old), Some(new)) = (name(before), name(after)) else {
                        return;
                    };
                    if old != new {
                        return;
                    }
                    match variants.get(&*old).map(|v| &v.shape) {
                        Some(VariantShape::Struct(fields)) => {
                            self.sealed_fields(fields, before, after, path, found);
                        }
                        Some(VariantShape::Newtype(ty)) => {
                            let inner = |value: &'_ Value| -> Value {
                                match &value.kind {
                                    ValueKind::NamedTuple(_, items) if items.len() == 1 => {
                                        items[0].clone()
                                    }
                                    _ => value.clone(),
                                }
                            };
                            self.sealed_in(ty, &inner(before), &inner(after), path, found);
                        }
                        _ => {}
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
}
