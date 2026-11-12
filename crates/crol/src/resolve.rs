//! What ids name: scopes and resolution (`SPEC.md` 3.2, 10.5).
//!
//! An id is resolved against evaluated data, at the place its value ends up.
//! The scopes around that place are the root and every keyed value on the
//! way down that is a scope: one with the scope flag (`#[scope]`), or one
//! whose type the schema marks (`#[crol(scope)]`). `@x` names `x` in the
//! innermost of them that has it; `@::x` starts from the root; `@a.b` goes
//! from `a` down its keys.

use std::fmt;

use crol_eval::{IdPath, Key, Keyed, Span, Value, ValueKind};

use crate::schema::{DataRoot, Field, SchemaFile, TypeDef, TypeExpr, VariantShape};

/// How deep a type is followed through wrappers before giving up.
const MAX_WRAPPERS: usize = 64;

/// One step of a [`DataPath`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DataStep {
    /// A field or a map's key, string or integer.
    Key(Key),
    /// An element of a list.
    Index(usize),
    /// An element of a tuple, or of a named tuple's payload.
    Element(usize),
}

/// A place in the data, from the root: where an id resolved to
/// ([`IdUse::target`](crate::IdUse::target)). It is written as a path:
/// `dashboard.filter`, `screens[0].list`, `pair.1`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct DataPath(Vec<DataStep>);

impl DataPath {
    /// The root.
    pub fn root() -> DataPath {
        DataPath(Vec::new())
    }

    /// A path of these steps.
    pub fn new(steps: impl IntoIterator<Item = DataStep>) -> DataPath {
        DataPath(steps.into_iter().collect())
    }

    /// The steps from the root.
    pub fn steps(&self) -> &[DataStep] {
        &self.0
    }

    /// The root key it starts with, if it starts with a string key.
    pub fn root_key(&self) -> Option<&str> {
        match self.0.first() {
            Some(DataStep::Key(Key::Str(key))) => Some(key),
            _ => None,
        }
    }

    fn push(&mut self, step: DataStep) {
        self.0.push(step);
    }

    /// The value at this path in `root`.
    pub fn value_in<'v>(&self, root: &'v Value) -> Option<&'v Value> {
        let mut value = root;
        for step in &self.0 {
            value = step_into(value, step)?;
        }
        Some(value)
    }
}

/// `dashboard.filter`, `screens[0]`, `pair.1`, `"odd key".x`, `map[3]`.
impl fmt::Display for DataPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, step) in self.0.iter().enumerate() {
            match step {
                DataStep::Key(Key::Str(key)) if crol_eval::is_identifier(key) => {
                    if i > 0 {
                        f.write_str(".")?;
                    }
                    f.write_str(key)?;
                }
                DataStep::Key(Key::Str(key)) if i == 0 => {
                    f.write_str(&crol_eval::quote(key))?;
                }
                DataStep::Key(Key::Str(key)) => write!(f, "[{}]", crol_eval::quote(key))?,
                DataStep::Key(Key::Int(key)) => write!(f, "[{key}]")?,
                DataStep::Key(Key::Id(key)) => write!(f, "[{key}]")?,
                DataStep::Index(index) => write!(f, "[{index}]")?,
                DataStep::Element(index) => write!(f, ".{index}")?,
            }
        }
        Ok(())
    }
}

fn step_into<'v>(value: &'v Value, step: &DataStep) -> Option<&'v Value> {
    match (step, &value.kind) {
        (DataStep::Key(key), ValueKind::Keyed(keyed)) => keyed.entries.get(key),
        (DataStep::Index(i), ValueKind::List(items)) => items.get(*i),
        (DataStep::Element(i), ValueKind::Tuple(items) | ValueKind::NamedTuple(_, items)) => {
            items.get(*i)
        }
        _ => None,
    }
}

/// What the schema says about types, for finding scopes by type.
#[derive(Clone, Copy)]
pub(crate) struct Types<'a> {
    schema: &'a SchemaFile,
}

impl<'a> Types<'a> {
    pub(crate) fn new(schema: &'a SchemaFile) -> Self {
        Types { schema }
    }
}

/// Which of the values on the way from the root to a place are scopes by
/// their type, for the evaluator, which resolves id keys as it goes
/// (`crol_eval::DefaultsProvider::scopes_on`): `values[0]` is the root, of
/// the type at `root_type` (`None`: the schema's root type), and `steps[i]`
/// leads from `values[i]` to `values[i + 1]`. The values need only their
/// kind and type name.
pub(crate) fn scopes_on(
    schema: &SchemaFile,
    root_type: Option<&str>,
    values: &[Value],
    steps: &[crol_eval::ScopeStep],
) -> Vec<bool> {
    let mut scopes = vec![false; values.len()];
    let Some(root) = values.first() else {
        return scopes;
    };
    let resolver = Resolver::with_root_type(root, Some(schema), root_type);
    let types = Types { schema };
    let mut ty = resolver.root_type();
    for (i, step) in steps.iter().enumerate() {
        let (Some(value), Some(next)) = (values.get(i), values.get(i + 1)) else {
            break;
        };
        let step = match step {
            crol_eval::ScopeStep::Key(key) => DataStep::Key(key.clone()),
            crol_eval::ScopeStep::Index(index) => DataStep::Index(*index),
            crol_eval::ScopeStep::Element(index) => DataStep::Element(*index),
        };
        ty = ty.and_then(|ty| types.child(&ty, value, &step));
        scopes[i + 1] = matches!(next.kind, ValueKind::Keyed(_))
            && ty.as_ref().is_some_and(|ty| types.is_scope(ty, next));
    }
    scopes
}

/// The data and types ids resolve against.
#[derive(Clone, Copy)]
pub(crate) struct Resolver<'a> {
    root: &'a Value,
    types: Option<Types<'a>>,
    /// The type of the root value, if the schema says.
    root_type: Option<&'a str>,
    /// Whether that type describes the whole root, or each root value.
    whole: bool,
}

/// An id met in the data, and what it names.
pub(crate) struct Resolved<'a> {
    /// The id value, or for an id key, the value of its entry.
    pub(crate) value: &'a Value,
    pub(crate) id: IdPath,
    /// Where it is: the id's place, or for an id key, the map's.
    pub(crate) location: DataPath,
    pub(crate) target: Option<DataPath>,
    /// Whether it is a key.
    pub(crate) key: bool,
}

impl<'a> Resolver<'a> {
    /// Ids in `root`, with scopes from the scope flag and, with a schema,
    /// from its scope types.
    pub(crate) fn new(root: &'a Value, schema: Option<&'a SchemaFile>) -> Resolver<'a> {
        Self::with_root_type(root, schema, None)
    }

    /// [`new`](Self::new), with the type at `root_type` in the schema
    /// (`None`: its root type) as the root type of data-mode data.
    pub(crate) fn with_root_type(
        root: &'a Value,
        schema: Option<&'a SchemaFile>,
        root_type: Option<&'a str>,
    ) -> Resolver<'a> {
        Resolver {
            root,
            types: schema.map(|schema| Types { schema }),
            root_type: root_type.or(schema.map(|schema| schema.root.as_str())),
            whole: schema.is_some_and(|schema| schema.data_root(root_type) == DataRoot::Document),
        }
    }

    /// A value document of the type at `ty` in `schema`.
    pub(crate) fn for_value(
        root: &'a Value,
        schema: Option<&'a SchemaFile>,
        ty: &'a str,
    ) -> Resolver<'a> {
        Resolver {
            root,
            types: schema.map(|schema| Types { schema }),
            root_type: Some(ty),
            whole: true,
        }
    }

    /// The type of the root value.
    fn root_type(&self) -> Option<TypeExpr> {
        self.types?;
        let root = TypeExpr::Named(self.root_type?.to_owned());
        Some(if self.whole {
            root
        } else {
            // The root type is the type of each root value.
            TypeExpr::Map(Box::new(TypeExpr::String), Box::new(root))
        })
    }

    /// The scopes around `location`, outermost first: the root, and each
    /// keyed value that holds it and is a scope, with where it is. The value
    /// at `location` itself is not among them.
    fn scopes_around(&self, location: &DataPath) -> Vec<(usize, &'a Keyed)> {
        let mut scopes = Vec::new();
        let mut value = self.root;
        let mut ty = self.root_type();
        if let ValueKind::Keyed(keyed) = &value.kind {
            scopes.push((0, &**keyed));
        }
        let Some((_, inner)) = location.0.split_last() else {
            return scopes;
        };
        for (i, step) in inner.iter().enumerate() {
            let Some(next) = step_into(value, step) else {
                break;
            };
            ty = ty.and_then(|ty| self.types?.child(&ty, value, step));
            value = next;
            if let ValueKind::Keyed(keyed) = &value.kind
                && (keyed.scope || self.is_scope_type(ty.as_ref(), value))
            {
                scopes.push((i + 1, &**keyed));
            }
        }
        scopes
    }

    fn is_scope_type(&self, ty: Option<&TypeExpr>, value: &Value) -> bool {
        match (self.types, ty) {
            (Some(types), Some(ty)) => types.is_scope(ty, value),
            _ => false,
        }
    }

    /// What `id`, at `location`, names: where it resolved to, or `None`.
    pub(crate) fn resolve(&self, id: &IdPath, location: &DataPath) -> Option<DataPath> {
        let scopes = self.scopes_around(location);
        let [first, rest @ ..] = id.keys() else {
            return None;
        };
        let first = Key::Str(first.clone());
        let (depth, keyed) = if id.is_rooted() {
            *scopes.first()?
        } else {
            *scopes
                .iter()
                .rev()
                .find(|(_, keyed)| keyed.entries.contains_key(&first))?
        };
        let mut target = DataPath(location.0[..depth].to_vec());
        let mut value = keyed.entries.get(&first)?;
        target.push(DataStep::Key(first));
        for key in rest {
            let ValueKind::Keyed(keyed) = &value.kind else {
                return None;
            };
            value = keyed.field(key)?;
            target.push(DataStep::Key(Key::Str(key.clone())));
        }
        Some(target)
    }

    /// Why `id`, at `location`, names nothing, for a diagnostic: which
    /// scopes do not have its first key, or which key a path id stops at.
    pub(crate) fn unresolved(&self, id: &IdPath, location: &DataPath) -> String {
        let scopes = self.scopes_around(location);
        let [first, rest @ ..] = id.keys() else {
            return format!("the id `{id}` names nothing");
        };
        let first_key = Key::Str(first.clone());
        let found = if id.is_rooted() {
            scopes.first().copied()
        } else {
            scopes
                .iter()
                .rev()
                .find(|(_, keyed)| keyed.entries.contains_key(&first_key))
                .copied()
        };
        let Some((_, keyed)) = found else {
            // Only the root around it, as without scopes.
            if id.is_rooted() || scopes.len() <= 1 {
                return format!("the id `{id}` names no root key");
            }
            let inner: Vec<String> = scopes[1..]
                .iter()
                .rev()
                .take(3)
                .map(|(depth, _)| format!("`{}`", DataPath(location.0[..*depth].to_vec())))
                .collect();
            return format!(
                "the id `{id}` names nothing: no `{first}` in {} or the root",
                inner.join(", ")
            );
        };
        let mut value = keyed.entries.get(&first_key);
        let mut reached = vec![first.to_string()];
        for key in rest {
            match value.map(|value| &value.kind) {
                Some(ValueKind::Keyed(keyed)) if keyed.has_name(key) => {
                    value = keyed.field(key);
                    reached.push(key.to_string());
                }
                _ => {
                    return format!(
                        "the id `{id}` names nothing: `{}` has no key `{key}`",
                        reached.join(".")
                    );
                }
            }
        }
        format!("the id `{id}` names nothing")
    }

    /// Every id in the data, and what it names: id values and id keys
    /// (`SPEC.md` 10.5).
    pub(crate) fn all(&self) -> Vec<Resolved<'a>> {
        let mut found = Vec::new();
        let ty = self.root_type();
        self.collect(self.root, ty, &mut DataPath::root(), &mut found);
        found
    }

    fn collect(
        &self,
        value: &'a Value,
        ty: Option<TypeExpr>,
        location: &mut DataPath,
        found: &mut Vec<Resolved<'a>>,
    ) {
        let child_type = |step: &DataStep| {
            ty.as_ref()
                .and_then(|ty| self.types?.child(ty, value, step))
        };
        match &value.kind {
            ValueKind::Id(id) => {
                let target = self.resolve(id, location);
                found.push(Resolved {
                    value,
                    id: id.clone(),
                    location: location.clone(),
                    target,
                    key: false,
                });
            }
            ValueKind::Keyed(keyed) => {
                for (key, entry) in &keyed.entries {
                    if let Key::Id(id) = key {
                        // A key resolves at the map's place.
                        let target = self.resolve(id, location);
                        found.push(Resolved {
                            value: entry,
                            id: id.clone(),
                            location: location.clone(),
                            target,
                            key: true,
                        });
                    }
                    let step = DataStep::Key(key.clone());
                    let ty = child_type(&step);
                    location.push(step);
                    self.collect(entry, ty, location, found);
                    location.0.pop();
                }
            }
            ValueKind::List(items) => {
                for (i, item) in items.iter().enumerate() {
                    let step = DataStep::Index(i);
                    let ty = child_type(&step);
                    location.push(step);
                    self.collect(item, ty, location, found);
                    location.0.pop();
                }
            }
            ValueKind::Tuple(items) | ValueKind::NamedTuple(_, items) => {
                for (i, item) in items.iter().enumerate() {
                    let step = DataStep::Element(i);
                    let ty = child_type(&step);
                    location.push(step);
                    self.collect(item, ty, location, found);
                    location.0.pop();
                }
            }
            _ => {}
        }
    }

    /// The type name of the value at `path`.
    pub(crate) fn type_name_at(&self, path: &DataPath) -> Option<&'a str> {
        match &path.value_in(self.root)?.kind {
            ValueKind::Keyed(keyed) => keyed.name.as_deref(),
            ValueKind::NamedTuple(name, _) | ValueKind::UnitName(name) => Some(name),
            _ => None,
        }
    }
}

impl<'a> Types<'a> {
    /// The definition a type stands for, through `Option`, newtypes and
    /// transparent structs, and the type it is once unwrapped.
    fn definition(&self, ty: &TypeExpr, value: &Value) -> Option<(&'a TypeDef, TypeExpr)> {
        let mut ty = ty.clone();
        for _ in 0..MAX_WRAPPERS {
            match ty {
                TypeExpr::Option(inner) => ty = *inner,
                TypeExpr::Named(ref path) => {
                    let def = self.schema.get(path)?;
                    match def {
                        // Data writes the wrapped value, or `Name(value)`.
                        TypeDef::Newtype { inner, .. } | TypeDef::Transparent { inner, .. }
                            if !matches!(&value.kind, ValueKind::NamedTuple(..)) =>
                        {
                            ty = inner.clone();
                        }
                        _ => return Some((def, ty)),
                    }
                }
                _ => return None,
            }
        }
        None
    }

    /// A type named by a value's own type name, if the schema has one type
    /// of that name: `Screen { ... }` is a `Screen` wherever it is.
    fn named(&self, value: &Value) -> Option<TypeExpr> {
        let ValueKind::Keyed(keyed) = &value.kind else {
            return None;
        };
        let name = keyed.name.as_deref()?;
        let mut found = self.schema.by_name(name);
        let (path, def) = found.next()?;
        // A struct, or what data may write as one: a newtype or transparent
        // struct around a keyed value (`Screen { ... }` for a map).
        if found.next().is_some()
            || !matches!(
                def,
                TypeDef::Struct { .. } | TypeDef::Newtype { .. } | TypeDef::Transparent { .. }
            )
        {
            return None;
        }
        Some(TypeExpr::Named(path.to_owned()))
    }

    /// Whether a keyed value of type `ty` is a scope: a struct, a newtype or
    /// transparent struct around it, or a struct variant, with
    /// `#[crol(scope)]`.
    fn is_scope(&self, ty: &TypeExpr, value: &Value) -> bool {
        let ty = self.named(value).unwrap_or_else(|| ty.clone());
        if self.wrapper_is_scope(&ty, value) {
            return true;
        }
        match self.definition(&ty, value) {
            Some((TypeDef::Struct { scope, .. }, _)) => *scope,
            Some((TypeDef::Enum { variants, .. }, _)) => match &value.kind {
                ValueKind::Keyed(keyed) => keyed
                    .name
                    .as_deref()
                    .and_then(|name| variants.get(name))
                    .is_some_and(|variant| variant.scope),
                _ => false,
            },
            _ => false,
        }
    }

    /// Whether a newtype or transparent struct with `#[crol(scope)]` wraps
    /// the value, through `Option` and other wrappers. Data writes such a
    /// value as what it wraps, here a keyed value.
    fn wrapper_is_scope(&self, ty: &TypeExpr, value: &Value) -> bool {
        let mut ty = ty.clone();
        for _ in 0..MAX_WRAPPERS {
            match ty {
                TypeExpr::Option(inner) => ty = *inner,
                TypeExpr::Named(ref path) => match self.schema.get(path) {
                    Some(
                        TypeDef::Newtype { inner, scope, .. }
                        | TypeDef::Transparent { inner, scope, .. },
                    ) if matches!(value.kind, ValueKind::Keyed(_)) => {
                        if *scope {
                            return true;
                        }
                        ty = inner.clone();
                    }
                    _ => return false,
                },
                _ => return false,
            }
        }
        false
    }

    /// The types an id of type `ty` may name, if `ty` is an id, through
    /// newtypes and transparent structs around one.
    pub(crate) fn id_type(&self, ty: &'a TypeExpr) -> Option<&'a [String]> {
        let mut ty = ty;
        for _ in 0..MAX_WRAPPERS {
            match ty {
                TypeExpr::Id(refers) => return Some(refers),
                TypeExpr::Named(path) => match self.schema.get(path) {
                    Some(TypeDef::Newtype { inner, .. } | TypeDef::Transparent { inner, .. }) => {
                        ty = inner;
                    }
                    _ => return None,
                },
                _ => return None,
            }
        }
        None
    }

    /// The type of what `step` reaches in `value`, of type `ty`.
    fn child(&self, ty: &TypeExpr, value: &Value, step: &DataStep) -> Option<TypeExpr> {
        let ty = self.named(value).unwrap_or_else(|| ty.clone());
        // `Some(x)` for an `Option`: `x` has the inner type.
        if let (TypeExpr::Option(inner), ValueKind::NamedTuple(name, _), DataStep::Element(0)) =
            (&ty, &value.kind, step)
            && &**name == "Some"
        {
            return Some((**inner).clone());
        }
        let mut ty = ty;
        for _ in 0..MAX_WRAPPERS {
            match (&ty, step) {
                (TypeExpr::Option(inner), _) => ty = (**inner).clone(),
                (TypeExpr::Map(_, item), DataStep::Key(_)) => return Some((**item).clone()),
                (
                    TypeExpr::List(item) | TypeExpr::Set(item) | TypeExpr::Array(item, _),
                    DataStep::Index(_),
                ) => return Some((**item).clone()),
                (TypeExpr::Tuple(items), DataStep::Element(i)) => return items.get(*i).cloned(),
                (TypeExpr::Named(path), _) => {
                    return match self.schema.get(path)? {
                        TypeDef::Struct { fields, .. } => self.field(fields, step, 0),
                        TypeDef::TupleStruct { fields, .. } => match step {
                            DataStep::Element(i) => fields.get(*i).cloned(),
                            _ => None,
                        },
                        TypeDef::Newtype { inner, .. } | TypeDef::Transparent { inner, .. } => {
                            match (&value.kind, step) {
                                (ValueKind::NamedTuple(..), DataStep::Element(0)) => {
                                    Some(inner.clone())
                                }
                                _ => {
                                    ty = inner.clone();
                                    continue;
                                }
                            }
                        }
                        TypeDef::Enum { variants, .. } => {
                            let name = match &value.kind {
                                ValueKind::Keyed(keyed) => keyed.name.as_deref()?,
                                ValueKind::NamedTuple(name, _) => name,
                                _ => return None,
                            };
                            match (&variants.get(name)?.shape, step) {
                                (VariantShape::Struct(fields), _) => self.field(fields, step, 0),
                                (VariantShape::Newtype(inner), DataStep::Element(0)) => {
                                    Some(inner.clone())
                                }
                                (VariantShape::Tuple(items), DataStep::Element(i)) => {
                                    items.get(*i).cloned()
                                }
                                _ => None,
                            }
                        }
                        _ => None,
                    };
                }
                _ => return None,
            }
        }
        None
    }

    /// The type of a struct's field, or of a key that a flattened field
    /// takes.
    fn field(
        &self,
        fields: &indexmap::IndexMap<String, Field>,
        step: &DataStep,
        depth: usize,
    ) -> Option<TypeExpr> {
        let DataStep::Key(Key::Str(name)) = step else {
            return None;
        };
        if depth > MAX_WRAPPERS {
            return None;
        }
        if let Some(field) = fields
            .get(&**name)
            .filter(|field| !field.flatten)
            .or_else(|| {
                fields
                    .values()
                    .find(|field| !field.flatten && field.aliases.iter().any(|a| **a == **name))
            })
        {
            return Some(field.ty.clone());
        }
        // A flattened field takes the keys the others do not: a map's, or a
        // struct's own fields.
        fields
            .values()
            .filter(|field| field.flatten)
            .find_map(|field| {
                let mut ty = field.ty.clone();
                for _ in 0..MAX_WRAPPERS {
                    match ty {
                        TypeExpr::Option(inner) => ty = *inner,
                        TypeExpr::Map(_, item) => return Some(*item),
                        TypeExpr::Named(ref path) => match self.schema.get(path)? {
                            TypeDef::Struct { fields, .. } => {
                                return self.field(fields, step, depth + 1);
                            }
                            TypeDef::Newtype { inner, .. } | TypeDef::Transparent { inner, .. } => {
                                ty = inner.clone();
                            }
                            _ => return None,
                        },
                        _ => return None,
                    }
                }
                None
            })
    }
}

/// The span to report a resolution at.
pub(crate) fn span_of(resolved: &Resolved<'_>) -> Span {
    resolved.value.span
}

impl<'a> Resolver<'a> {
    /// The keys `@name` can name at `location`: those of each scope around
    /// it, innermost first, each once.
    pub(crate) fn visible(&self, location: &DataPath) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for (_, keyed) in self.scopes_around(location).into_iter().rev() {
            for key in keyed.entries.keys() {
                if let Key::Str(name) = key
                    && seen.insert(name.clone())
                {
                    out.push(name.to_string());
                }
            }
        }
        out
    }
}

/// An id met in evaluated data, and what it names (`SPEC.md` 10.5).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ResolvedId {
    /// The id as written.
    pub id: crate::Id,
    /// Where it was written: one id written in a template is met at each
    /// place the template is copied to, with the same span.
    pub span: Span,
    /// Where it is in the data; for an id key, the map's place.
    pub location: DataPath,
    /// Whether it is a key of a map rather than a value: `@name: value`.
    /// Its span is then that of the entry's value.
    pub is_key: bool,
    /// Where it resolved to, or `None` if it names nothing.
    pub target: Option<DataPath>,
    /// Why it names nothing, as a diagnostic says it.
    pub problem: Option<String>,
}

/// Resolves the ids of evaluated data, as the loader and `crol::check` do,
/// for tools (`SPEC.md` 10.5): `IdResolver::new(&root).schema(&schema)`.
#[derive(Clone, Copy)]
pub struct IdResolver<'a> {
    root: &'a Value,
    schema: Option<&'a SchemaFile>,
    root_type: Option<&'a str>,
    value_type: Option<&'a str>,
}

impl<'a> IdResolver<'a> {
    /// Ids in `root`, a context's whole root, with scopes from `#[scope]`
    /// only.
    pub fn new(root: &'a Value) -> Self {
        IdResolver {
            root,
            schema: None,
            root_type: None,
            value_type: None,
        }
    }

    /// Scopes from the schema's `#[crol(scope)]` types as well.
    pub fn schema(mut self, schema: &'a SchemaFile) -> Self {
        self.schema = Some(schema);
        self
    }

    /// Data-mode data of the type at `path` in the schema, rather than its
    /// root type (`SPEC.md` 4.1, 12.4).
    pub fn root_type(mut self, path: &'a str) -> Self {
        self.root_type = Some(path);
        self
    }

    /// A value document of the type at `path` in the schema (`SPEC.md` 4).
    pub fn value_type(mut self, path: &'a str) -> Self {
        self.value_type = Some(path);
        self
    }

    fn inner(&self) -> Resolver<'a> {
        match self.value_type {
            Some(ty) => Resolver::for_value(self.root, self.schema, ty),
            None => Resolver::with_root_type(self.root, self.schema, self.root_type),
        }
    }

    /// What `id`, at `location`, names.
    pub fn resolve(&self, id: &crate::Id, location: &DataPath) -> Option<DataPath> {
        self.inner().resolve(&id.to_path(), location)
    }

    /// The keys `@name` can name at `location`, innermost scope first.
    pub fn visible(&self, location: &DataPath) -> Vec<String> {
        self.inner().visible(location)
    }

    /// Every id in the data and what it names: id values and id keys.
    pub fn ids(&self) -> Vec<ResolvedId> {
        let resolver = self.inner();
        resolver
            .all()
            .into_iter()
            .map(|resolved| ResolvedId {
                id: crate::Id::from_path(&resolved.id),
                span: span_of(&resolved),
                is_key: resolved.key,
                problem: resolved
                    .target
                    .is_none()
                    .then(|| resolver.unresolved(&resolved.id, &resolved.location)),
                location: resolved.location,
                target: resolved.target,
            })
            .collect()
    }
}
