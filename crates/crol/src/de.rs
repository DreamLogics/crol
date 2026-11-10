//! The serde `Deserializer` over evaluated values (`SPEC.md` 10.1 to 10.6).

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::rc::Rc;
use std::sync::Arc;

use serde::de::{
    self, DeserializeSeed, EnumAccess, IntoDeserializer, MapAccess, SeqAccess, VariantAccess,
    Visitor,
};

use crol_eval::{IdPath, Key, Keyed, Sources, Value, ValueKind};

use crate::error::Error;
use crate::id::{ID_TOKEN, Id, IdUse};
use crate::raw::RAW_TOKEN;
use crate::resolve::{DataPath, DataStep, Resolver};
use crate::schema::{Field, SchemaFile, TypeDef, VariantShape};

/// Options of the deserializer (`SPEC.md` 10.7's `CrolConfig` fields).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Options {
    /// Any value is accepted where an `Option` is expected, as `Some`.
    pub(crate) implicit_some: bool,
    /// Keys a struct does not have are ignored instead of an error.
    pub(crate) allow_unknown_fields: bool,
    /// Every field must be written unless the value has `..`.
    pub(crate) require_complete: bool,
    /// What the schema says about structs, if there is one.
    pub(crate) structs: Option<Arc<Structs>>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            implicit_some: true,
            allow_unknown_fields: false,
            require_complete: true,
            structs: None,
        }
    }
}

/// What a schema says about a struct or struct variant that serde does not
/// tell the deserializer.
#[derive(Debug, PartialEq, Eq)]
struct StructInfo {
    /// The names of each field, its own name first: serde gives every name
    /// a struct accepts, without saying which belong to one field.
    fields: Vec<Vec<String>>,
    /// `#[crol(explicit_defaults)]`.
    explicit_defaults: Option<bool>,
}

/// The structs and struct variants that have serde aliases or
/// `#[crol(explicit_defaults)]`, by serde name (`SPEC.md` 10.2).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Structs(HashMap<String, Vec<StructInfo>>);

impl Structs {
    pub(crate) fn from_schema(schema: &SchemaFile) -> Self {
        let mut structs = Self::default();
        let mut add = |name: &str,
                       fields: &indexmap::IndexMap<String, Field>,
                       explicit_defaults: Option<bool>| {
            if explicit_defaults.is_none() && fields.values().all(|f| f.aliases.is_empty()) {
                return;
            }
            let fields: Vec<Vec<String>> = fields
                .iter()
                .map(|(n, f)| std::iter::once(n).chain(&f.aliases).cloned().collect())
                .collect();
            let known = structs.0.entry(name.to_owned()).or_default();
            // Serde cannot tell types of one name and the same fields
            // apart: of their settings, the one that asks most holds.
            match known.iter_mut().find(|s| s.fields == fields) {
                Some(info) => {
                    info.explicit_defaults = info.explicit_defaults.max(explicit_defaults)
                }
                None => known.push(StructInfo {
                    fields,
                    explicit_defaults,
                }),
            }
        };
        for def in schema.types.values() {
            match def {
                TypeDef::Struct {
                    name,
                    fields,
                    explicit_defaults,
                    ..
                } => add(name, fields, *explicit_defaults),
                TypeDef::Enum { variants, .. } => {
                    for (name, variant) in variants {
                        if let VariantShape::Struct(fields) = &variant.shape {
                            add(name, fields, variant.explicit_defaults);
                        }
                    }
                }
                _ => {}
            }
        }
        structs
    }

    /// The struct `name` that accepts exactly `accepted`.
    fn find(&self, name: &str, accepted: &[&str]) -> Option<&StructInfo> {
        self.0.get(name)?.iter().find(|info| {
            info.fields.iter().map(Vec::len).sum::<usize>() == accepted.len()
                && info
                    .fields
                    .iter()
                    .flatten()
                    .all(|n| accepted.contains(&n.as_str()))
        })
    }
}

/// What every deserializer of one value shares.
pub(crate) struct Context<'a> {
    pub(crate) sources: &'a Sources,
    pub(crate) options: Options,
    pub(crate) ids: RefCell<Vec<IdUse>>,
    /// What ids resolve against (`SPEC.md` 10.5); without it, no id
    /// resolves.
    pub(crate) resolver: Option<Resolver<'a>>,
}

/// Where in the data we are, which a wrapper such as `Some(x)` adds a step
/// to, though the Rust path stays: `web.depends_on[0]`.
enum Place {
    Root,
    Step(Rc<Place>, DataStep),
}

impl Place {
    fn to_path(&self) -> DataPath {
        let mut steps = Vec::new();
        let mut node = self;
        while let Place::Step(parent, step) = node {
            steps.push(step.clone());
            node = parent;
        }
        steps.reverse();
        DataPath::new(steps)
    }
}

/// Where in the Rust value we are: `web.depends_on[0]`.
enum PathNode {
    Root,
    Key(Rc<PathNode>, Key),
    Index(Rc<PathNode>, usize),
}

fn describe(path: &PathNode) -> String {
    let mut parts = Vec::new();
    let mut node = path;
    loop {
        match node {
            PathNode::Root => break,
            PathNode::Key(parent, key) => {
                parts.push(match key {
                    Key::Str(s) if key.is_identifier() => format!(".{s}"),
                    key => format!("[{key}]"),
                });
                node = parent;
            }
            PathNode::Index(parent, i) => {
                parts.push(format!("[{i}]"));
                node = parent;
            }
        }
    }
    let mut out = String::new();
    for part in parts.iter().rev() {
        out.push_str(part);
    }
    out.strip_prefix('.').map(str::to_owned).unwrap_or(out)
}

/// Deserializes one value.
pub(crate) struct ValueDeserializer<'a> {
    value: &'a Value,
    context: &'a Context<'a>,
    path: Rc<PathNode>,
    place: Rc<Place>,
    /// The name of the newtype variant whose payload this is: `V { ... }`
    /// deserializes as a newtype variant whose struct accepts the name `V`.
    variant: Option<&'a str>,
}

impl<'a> ValueDeserializer<'a> {
    pub(crate) fn new(value: &'a Value, context: &'a Context<'a>) -> Self {
        Self {
            value,
            context,
            path: Rc::new(PathNode::Root),
            place: Rc::new(Place::Root),
            variant: None,
        }
    }

    /// A deserializer for the value of the root key `key`.
    pub(crate) fn at_key(value: &'a Value, context: &'a Context<'a>, key: &str) -> Self {
        Self {
            value,
            context,
            path: Rc::new(PathNode::Key(Rc::new(PathNode::Root), Key::Str(key.into()))),
            place: Rc::new(Place::Step(
                Rc::new(Place::Root),
                DataStep::Key(Key::Str(key.into())),
            )),
            variant: None,
        }
    }

    /// A deserializer for what `path` reaches in this value: a key, an
    /// element, or with `PathNode::Root`, a wrapper's single element.
    fn child(&self, value: &'a Value, path: PathNode) -> ValueDeserializer<'a> {
        let step = match (&path, &self.value.kind) {
            (PathNode::Key(_, key), _) => DataStep::Key(key.clone()),
            (PathNode::Index(_, i), ValueKind::List(_)) => DataStep::Index(*i),
            (PathNode::Index(_, i), _) => DataStep::Element(*i),
            (PathNode::Root, _) => DataStep::Element(0),
        };
        ValueDeserializer {
            value,
            context: self.context,
            path: Rc::new(path),
            place: Rc::new(Place::Step(self.place.clone(), step)),
            variant: None,
        }
    }

    fn error(&self, message: impl Into<String>) -> Error {
        self.attach(Error::new(message))
    }

    /// Adds this value's span and path to an error that has none.
    fn attach(&self, error: Error) -> Error {
        error.at(self.context.sources, self.value.span, || {
            describe(&self.path)
        })
    }

    fn found(&self) -> String {
        match &self.value.kind {
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

    /// The text of the id this is, without `@`, for a hint.
    fn id_text(&self) -> String {
        match &self.value.kind {
            ValueKind::Id(id) => id.keys().join("."),
            _ => String::new(),
        }
    }

    fn expected(&self, what: &str) -> Error {
        self.error(format!("expected {what}, found {}", self.found()))
    }

    /// Records an id this value is, or for a map key, holds as a key.
    fn record_id(&self, id: &IdPath) {
        let place = self.place.to_path();
        let target = self
            .context
            .resolver
            .and_then(|resolver| resolver.resolve(id, &place));
        self.context.ids.borrow_mut().push(IdUse {
            id: Id::from_path(id),
            span: self.value.span,
            rust_path: describe(&self.path),
            target,
        });
    }

    fn int<T: TryFrom<i128>>(&self, what: &str) -> Result<T, Error> {
        match &self.value.kind {
            ValueKind::Int(i) => T::try_from(*i).map_err(|_| self.expected(what)),
            _ => Err(self.expected(what)),
        }
    }

    fn seq<V: Visitor<'a>>(&self, elements: &'a [Value], visitor: V) -> Result<V::Value, Error> {
        let mut access = SeqDeserializer {
            parent: self,
            elements: elements.iter().enumerate(),
        };
        let value = visitor.visit_seq(&mut access).map_err(|e| self.attach(e))?;
        if access.elements.len() > 0 {
            return Err(self.error(format!(
                "expected {} elements, found {}",
                elements.len() - access.elements.len(),
                elements.len()
            )));
        }
        Ok(value)
    }

    fn map<V: Visitor<'a>>(&self, keyed: &'a Keyed, visitor: V) -> Result<V::Value, Error> {
        let mut access = MapDeserializer {
            parent: self,
            entries: keyed.entries.iter(),
            value: None,
        };
        visitor.visit_map(&mut access).map_err(|e| self.attach(e))
    }

    /// The struct checks of `SPEC.md` 10.2.
    fn check_struct(&self, keyed: &Keyed, name: &str, fields: &[&str]) -> Result<(), Error> {
        if let Some(written) = &keyed.name
            && &**written != name
            && Some(&**written) != self.variant
        {
            return Err(self.error(format!("expected a `{name}`, found a `{written}`")));
        }
        for (key, value) in &keyed.entries {
            let field = match key {
                Key::Str(s) => s,
                Key::Int(i) => {
                    return Err(self
                        .child(value, PathNode::Key(self.path.clone(), key.clone()))
                        .error(format!("`{name}` has named fields; the key {i} is not one")));
                }
            };
            if !self.context.options.allow_unknown_fields && !fields.contains(&field.as_ref()) {
                let mut message = format!("`{name}` has no field `{field}`");
                if !fields.is_empty() {
                    let _ = write!(message, "; its fields are `{}`", fields.join("`, `"));
                }
                return Err(self
                    .child(value, PathNode::Key(self.path.clone(), key.clone()))
                    .error(message));
            }
        }
        let options = &self.context.options;
        let info = options.structs.as_ref().and_then(|s| s.find(name, fields));
        // The type's setting, if the schema gives it, else the option's.
        let explicit = info
            .and_then(|i| i.explicit_defaults)
            .unwrap_or(options.require_complete);
        if explicit && !keyed.defaults {
            let present = |f: &str| {
                let key = Key::Str(f.into());
                keyed.entries.contains_key(&key) || keyed.defaulted.contains(&key)
            };
            // A field is written when one of its names is.
            let missing: Vec<&str> = match info {
                Some(info) => info
                    .fields
                    .iter()
                    .filter(|names| !names.iter().any(|n| present(n)))
                    .map(|names| names[0].as_str())
                    .collect(),
                None => fields.iter().copied().filter(|f| !present(f)).collect(),
            };
            if !missing.is_empty() {
                let s = if missing.len() == 1 { "" } else { "s" };
                return Err(self.error(format!(
                    "`{name}` is missing the field{s} `{}`; write {}, or end the block with `..` to leave out fields that have defaults",
                    missing.join("`, `"),
                    if missing.len() == 1 { "it" } else { "them" },
                )));
            }
        }
        Ok(())
    }
}

macro_rules! deserialize_int {
    ($($method:ident => $visit:ident: $ty:ty),* $(,)?) => {
        $(
            fn $method<V: Visitor<'a>>(self, visitor: V) -> Result<V::Value, Error> {
                let value: $ty = self.int(stringify!($ty))?;
                visitor.$visit(value).map_err(|e| self.attach(e))
            }
        )*
    };
}

impl<'a> de::Deserializer<'a> for ValueDeserializer<'a> {
    type Error = Error;

    /// The value's natural form (`SPEC.md` 10.1).
    fn deserialize_any<V: Visitor<'a>>(self, visitor: V) -> Result<V::Value, Error> {
        let result = match &self.value.kind {
            ValueKind::Unit => visitor.visit_unit(),
            ValueKind::Bool(b) => visitor.visit_bool(*b),
            ValueKind::Int(i) => match (i64::try_from(*i), u64::try_from(*i)) {
                (Ok(i), _) => visitor.visit_i64(i),
                (_, Ok(u)) => visitor.visit_u64(u),
                _ => visitor.visit_i128(*i),
            },
            ValueKind::Float(f) => visitor.visit_f64(*f),
            ValueKind::Char(c) => visitor.visit_char(*c),
            ValueKind::String(s) => visitor.visit_borrowed_str(s),
            // As its text, `@` included (`SPEC.md` 10.1, 10.5).
            ValueKind::Id(id) => {
                self.record_id(id);
                visitor.visit_string(id.to_string())
            }
            ValueKind::UnitName(name) => visitor.visit_borrowed_str(name),
            ValueKind::List(elements) | ValueKind::Tuple(elements) => {
                return self.seq(elements, visitor);
            }
            ValueKind::NamedTuple(name, elements) => {
                let mut access = NamedTupleMap {
                    parent: &self,
                    name: Some(name),
                    elements,
                };
                visitor.visit_map(&mut access)
            }
            ValueKind::Keyed(keyed) => return self.map(keyed, visitor),
        };
        result.map_err(|e| self.attach(e))
    }

    fn deserialize_bool<V: Visitor<'a>>(self, visitor: V) -> Result<V::Value, Error> {
        match &self.value.kind {
            ValueKind::Bool(b) => visitor.visit_bool(*b).map_err(|e| self.attach(e)),
            _ => Err(self.expected("a boolean")),
        }
    }

    deserialize_int! {
        deserialize_i8 => visit_i8: i8,
        deserialize_i16 => visit_i16: i16,
        deserialize_i32 => visit_i32: i32,
        deserialize_i64 => visit_i64: i64,
        deserialize_i128 => visit_i128: i128,
        deserialize_u8 => visit_u8: u8,
        deserialize_u16 => visit_u16: u16,
        deserialize_u32 => visit_u32: u32,
        deserialize_u64 => visit_u64: u64,
        deserialize_u128 => visit_u128: u128,
    }

    fn deserialize_f32<V: Visitor<'a>>(self, visitor: V) -> Result<V::Value, Error> {
        match &self.value.kind {
            ValueKind::Float(f) => visitor.visit_f32(*f as f32),
            ValueKind::Int(i) => visitor.visit_f32(*i as f32),
            _ => return Err(self.expected("a float")),
        }
        .map_err(|e| self.attach(e))
    }

    fn deserialize_f64<V: Visitor<'a>>(self, visitor: V) -> Result<V::Value, Error> {
        match &self.value.kind {
            ValueKind::Float(f) => visitor.visit_f64(*f),
            ValueKind::Int(i) => visitor.visit_f64(*i as f64),
            _ => return Err(self.expected("a float")),
        }
        .map_err(|e| self.attach(e))
    }

    fn deserialize_char<V: Visitor<'a>>(self, visitor: V) -> Result<V::Value, Error> {
        match &self.value.kind {
            ValueKind::Char(c) => visitor.visit_char(*c).map_err(|e| self.attach(e)),
            _ => Err(self.expected("a char")),
        }
    }

    fn deserialize_str<V: Visitor<'a>>(self, visitor: V) -> Result<V::Value, Error> {
        match &self.value.kind {
            ValueKind::String(s) => visitor.visit_borrowed_str(s),
            // A string field takes no id (`SPEC.md` 10.1).
            ValueKind::Id(_) => {
                return Err(self.error(format!(
                    "expected a string, found an id; a field that takes ids is a `crol::Id`, \
                     and a string is written `\"{}\"`",
                    self.id_text()
                )));
            }
            _ => return Err(self.expected("a string")),
        }
        .map_err(|e| self.attach(e))
    }

    fn deserialize_string<V: Visitor<'a>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_str(visitor)
    }

    fn deserialize_bytes<V: Visitor<'a>>(self, visitor: V) -> Result<V::Value, Error> {
        let ValueKind::List(elements) = &self.value.kind else {
            return Err(self.expected("a list of bytes"));
        };
        let mut bytes = Vec::with_capacity(elements.len());
        for (i, element) in elements.iter().enumerate() {
            let byte: u8 = self
                .child(element, PathNode::Index(self.path.clone(), i))
                .int("a byte (0 to 255)")?;
            bytes.push(byte);
        }
        visitor.visit_byte_buf(bytes).map_err(|e| self.attach(e))
    }

    fn deserialize_byte_buf<V: Visitor<'a>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_bytes(visitor)
    }

    fn deserialize_option<V: Visitor<'a>>(self, visitor: V) -> Result<V::Value, Error> {
        match &self.value.kind {
            ValueKind::UnitName(name) if &**name == "None" => visitor.visit_none(),
            ValueKind::NamedTuple(name, elements) if &**name == "Some" && elements.len() == 1 => {
                let inner = self.child(&elements[0], PathNode::Index(self.path.clone(), 0));
                let inner = ValueDeserializer {
                    path: self.path.clone(),
                    ..inner
                };
                visitor.visit_some(inner)
            }
            _ if self.context.options.implicit_some => {
                let inner = ValueDeserializer {
                    value: self.value,
                    context: self.context,
                    path: self.path.clone(),
                    place: self.place.clone(),
                    variant: None,
                };
                visitor.visit_some(inner)
            }
            _ => return Err(self.expected("`Some(...)` or `None`")),
        }
        .map_err(|e| self.attach(e))
    }

    fn deserialize_unit<V: Visitor<'a>>(self, visitor: V) -> Result<V::Value, Error> {
        match &self.value.kind {
            ValueKind::Unit => visitor.visit_unit().map_err(|e| self.attach(e)),
            _ => Err(self.expected("`()`")),
        }
    }

    fn deserialize_unit_struct<V: Visitor<'a>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        match &self.value.kind {
            ValueKind::Unit => {}
            ValueKind::UnitName(written) if &**written == name => {}
            _ => return Err(self.expected(&format!("`{name}`"))),
        }
        visitor.visit_unit().map_err(|e| self.attach(e))
    }

    fn deserialize_newtype_struct<V: Visitor<'a>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        if name == RAW_TOKEN {
            // The value as CROL text, which `RawValue` reads back.
            let text = crol_eval::print::to_text(self.value);
            let inner: de::value::StringDeserializer<Error> =
                de::value::StringDeserializer::new(text);
            return visitor
                .visit_newtype_struct(inner)
                .map_err(|e| self.attach(e));
        }
        if name == ID_TOKEN {
            let ValueKind::Id(id) = &self.value.kind else {
                return Err(self.expected("an id (`@name`)"));
            };
            self.record_id(id);
            let inner: de::value::StringDeserializer<Error> =
                de::value::StringDeserializer::new(id.to_string());
            return visitor
                .visit_newtype_struct(inner)
                .map_err(|e| self.attach(e));
        }
        match &self.value.kind {
            ValueKind::NamedTuple(written, elements)
                if &**written == name && elements.len() == 1 =>
            {
                let inner = ValueDeserializer {
                    path: self.path.clone(),
                    ..self.child(&elements[0], PathNode::Root)
                };
                visitor.visit_newtype_struct(inner)
            }
            _ => {
                let inner = ValueDeserializer {
                    value: self.value,
                    context: self.context,
                    path: self.path.clone(),
                    place: self.place.clone(),
                    variant: None,
                };
                visitor.visit_newtype_struct(inner)
            }
        }
        .map_err(|e| self.attach(e))
    }

    fn deserialize_seq<V: Visitor<'a>>(self, visitor: V) -> Result<V::Value, Error> {
        match &self.value.kind {
            ValueKind::List(elements) => self.seq(elements, visitor),
            _ => Err(self.expected("a list")),
        }
    }

    fn deserialize_tuple<V: Visitor<'a>>(self, len: usize, visitor: V) -> Result<V::Value, Error> {
        match &self.value.kind {
            ValueKind::Tuple(elements) if elements.len() == len => self.seq(elements, visitor),
            ValueKind::Unit if len == 0 => self.seq(&[], visitor),
            _ => Err(self.expected(&format!("a tuple of {len}"))),
        }
    }

    fn deserialize_tuple_struct<V: Visitor<'a>>(
        self,
        name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Error> {
        match &self.value.kind {
            ValueKind::NamedTuple(written, elements)
                if &**written == name && elements.len() == len =>
            {
                self.seq(elements, visitor)
            }
            ValueKind::Tuple(elements) if elements.len() == len => self.seq(elements, visitor),
            _ => Err(self.expected(&format!("`{name}(...)` with {len} elements"))),
        }
    }

    fn deserialize_map<V: Visitor<'a>>(self, visitor: V) -> Result<V::Value, Error> {
        match &self.value.kind {
            ValueKind::Keyed(keyed) => self.map(keyed, visitor),
            _ => Err(self.expected("a keyed value `{ ... }`")),
        }
    }

    fn deserialize_struct<V: Visitor<'a>>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        let ValueKind::Keyed(keyed) = &self.value.kind else {
            return Err(self.expected(&format!("`{name} {{ ... }}`")));
        };
        self.check_struct(keyed, name, fields)?;
        self.map(keyed, visitor)
    }

    fn deserialize_enum<V: Visitor<'a>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        let variant = match &self.value.kind {
            ValueKind::UnitName(variant) | ValueKind::NamedTuple(variant, _) => &**variant,
            ValueKind::Keyed(keyed) if keyed.name.is_some() => {
                keyed.name.as_deref().unwrap_or_default()
            }
            _ => return Err(self.expected(&format!("a `{name}` variant"))),
        };
        if !variants.contains(&variant) {
            return Err(self.error(format!(
                "`{name}` has no variant `{variant}`; its variants are `{}`",
                variants.join("`, `")
            )));
        }
        visitor
            .visit_enum(EnumDeserializer {
                parent: &self,
                variant,
            })
            .map_err(|e| self.attach(e))
    }

    fn deserialize_identifier<V: Visitor<'a>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_str(visitor)
    }

    fn deserialize_ignored_any<V: Visitor<'a>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_unit()
    }
}

struct SeqDeserializer<'p, 'a> {
    parent: &'p ValueDeserializer<'a>,
    elements: std::iter::Enumerate<std::slice::Iter<'a, Value>>,
}

impl<'a> SeqAccess<'a> for SeqDeserializer<'_, 'a> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'a>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        match self.elements.next() {
            None => Ok(None),
            Some((i, element)) => seed
                .deserialize(
                    self.parent
                        .child(element, PathNode::Index(self.parent.path.clone(), i)),
                )
                .map(Some),
        }
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.elements.len())
    }
}

struct MapDeserializer<'p, 'a> {
    parent: &'p ValueDeserializer<'a>,
    entries: indexmap::map::Iter<'a, Key, Value>,
    value: Option<(&'a Key, &'a Value)>,
}

impl<'a> MapAccess<'a> for MapDeserializer<'_, 'a> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'a>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        let Some((key, value)) = self.entries.next() else {
            return Ok(None);
        };
        self.value = Some((key, value));
        let at_value = self
            .parent
            .child(value, PathNode::Key(self.parent.path.clone(), key.clone()));
        seed.deserialize(KeyDeserializer { key, at: &at_value })
            .map(Some)
            .map_err(|e| at_value.attach(e))
    }

    fn next_value_seed<V: DeserializeSeed<'a>>(&mut self, seed: V) -> Result<V::Value, Error> {
        let (key, value) = self.value.take().expect("next_value follows next_key");
        seed.deserialize(
            self.parent
                .child(value, PathNode::Key(self.parent.path.clone(), key.clone())),
        )
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.entries.len())
    }
}

/// A named tuple in `deserialize_any`: a map from its name to its payload.
struct NamedTupleMap<'p, 'a> {
    parent: &'p ValueDeserializer<'a>,
    name: Option<&'a str>,
    elements: &'a [Value],
}

impl<'a> MapAccess<'a> for NamedTupleMap<'_, 'a> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'a>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        match self.name {
            None => Ok(None),
            Some(name) => seed
                .deserialize(de::value::BorrowedStrDeserializer::<Error>::new(name))
                .map(Some),
        }
    }

    fn next_value_seed<V: DeserializeSeed<'a>>(&mut self, seed: V) -> Result<V::Value, Error> {
        self.name = None;
        match self.elements {
            [single] => seed.deserialize(
                self.parent
                    .child(single, PathNode::Index(self.parent.path.clone(), 0)),
            ),
            elements => {
                let mut access = SeqDeserializer {
                    parent: self.parent,
                    elements: elements.iter().enumerate(),
                };
                seed.deserialize(de::value::SeqAccessDeserializer::new(&mut access))
            }
        }
    }
}

/// A key of a keyed value (`SPEC.md` 10.1): string keys deserialize into
/// strings, unit enum variants and `crol::Id`; integer keys into integers.
struct KeyDeserializer<'p, 'a> {
    key: &'a Key,
    /// The entry's value, for spans and paths in errors and id uses.
    at: &'p ValueDeserializer<'a>,
}

macro_rules! key_int {
    ($($method:ident => $visit:ident: $ty:ty),* $(,)?) => {
        $(
            fn $method<V: Visitor<'a>>(self, visitor: V) -> Result<V::Value, Error> {
                match self.key {
                    Key::Int(i) => match <$ty>::try_from(*i) {
                        Ok(value) => visitor.$visit(value),
                        Err(_) => Err(Error::new(format!("expected a {} key, found {i}", stringify!($ty)))),
                    },
                    Key::Str(s) => Err(Error::new(format!(
                        "expected an integer key, found the string key {}",
                        crol_eval::quote(s)
                    ))),
                }
            }
        )*
    };
}

impl<'a> de::Deserializer<'a> for KeyDeserializer<'_, 'a> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'a>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.key {
            Key::Str(s) => visitor.visit_borrowed_str(s),
            Key::Int(i) => match i64::try_from(*i) {
                Ok(i) => visitor.visit_i64(i),
                Err(_) => visitor.visit_i128(*i),
            },
        }
    }

    key_int! {
        deserialize_i8 => visit_i8: i8,
        deserialize_i16 => visit_i16: i16,
        deserialize_i32 => visit_i32: i32,
        deserialize_i64 => visit_i64: i64,
        deserialize_i128 => visit_i128: i128,
        deserialize_u8 => visit_u8: u8,
        deserialize_u16 => visit_u16: u16,
        deserialize_u32 => visit_u32: u32,
        deserialize_u64 => visit_u64: u64,
        deserialize_u128 => visit_u128: u128,
    }

    fn deserialize_newtype_struct<V: Visitor<'a>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        match self.key {
            Key::Str(s) => {
                if name == ID_TOKEN {
                    // A key is written without `@`; `Id` takes its text with
                    // one (`SPEC.md` 10.5).
                    let id = IdPath::key(s.clone());
                    self.at.record_id(&id);
                    return visitor.visit_newtype_struct(
                        de::value::StringDeserializer::<Error>::new(id.to_string()),
                    );
                }
                visitor.visit_newtype_struct(de::value::BorrowedStrDeserializer::<Error>::new(s))
            }
            Key::Int(_) => visitor.visit_newtype_struct(self),
        }
    }

    fn deserialize_enum<V: Visitor<'a>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        match self.key {
            Key::Str(s) => (&**s)
                .into_deserializer()
                .deserialize_enum(name, variants, visitor),
            Key::Int(i) => Err(Error::new(format!(
                "expected a `{name}` variant as the key, found {i}"
            ))),
        }
    }

    fn deserialize_option<V: Visitor<'a>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_some(self)
    }

    serde::forward_to_deserialize_any! {
        <W: Visitor<'a>>
        bool f32 f64 char str string bytes byte_buf unit unit_struct seq tuple
        tuple_struct map struct identifier ignored_any
    }
}

struct EnumDeserializer<'p, 'a> {
    parent: &'p ValueDeserializer<'a>,
    variant: &'a str,
}

impl<'p, 'a> EnumAccess<'a> for EnumDeserializer<'p, 'a> {
    type Error = Error;
    type Variant = Self;

    fn variant_seed<T: DeserializeSeed<'a>>(self, seed: T) -> Result<(T::Value, Self), Error> {
        let variant = seed.deserialize(de::value::BorrowedStrDeserializer::<Error>::new(
            self.variant,
        ))?;
        Ok((variant, self))
    }
}

impl<'a> VariantAccess<'a> for EnumDeserializer<'_, 'a> {
    type Error = Error;

    fn unit_variant(self) -> Result<(), Error> {
        match &self.parent.value.kind {
            ValueKind::UnitName(_) => Ok(()),
            _ => Err(self
                .parent
                .expected(&format!("the unit variant `{}`", self.variant))),
        }
    }

    fn newtype_variant_seed<T: DeserializeSeed<'a>>(self, seed: T) -> Result<T::Value, Error> {
        let parent = self.parent;
        match &parent.value.kind {
            ValueKind::NamedTuple(_, elements) if elements.len() == 1 => {
                seed.deserialize(ValueDeserializer {
                    path: parent.path.clone(),
                    ..parent.child(&elements[0], PathNode::Root)
                })
            }
            // `V { ... }` as a newtype variant: its payload is the block,
            // whose type name check accepts `V` (`SPEC.md` 10.4).
            ValueKind::Keyed(_) => seed.deserialize(ValueDeserializer {
                value: parent.value,
                context: parent.context,
                path: parent.path.clone(),
                place: parent.place.clone(),
                variant: Some(self.variant),
            }),
            _ => Err(parent.expected(&format!("`{}(...)` with one element", self.variant))),
        }
    }

    fn tuple_variant<V: Visitor<'a>>(self, len: usize, visitor: V) -> Result<V::Value, Error> {
        match &self.parent.value.kind {
            ValueKind::NamedTuple(_, elements) if elements.len() == len => {
                self.parent.seq(elements, visitor)
            }
            _ => Err(self
                .parent
                .expected(&format!("`{}(...)` with {len} elements", self.variant))),
        }
    }

    fn struct_variant<V: Visitor<'a>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        match &self.parent.value.kind {
            ValueKind::Keyed(keyed) => {
                self.parent.check_struct(keyed, self.variant, fields)?;
                self.parent.map(keyed, visitor)
            }
            _ => Err(self
                .parent
                .expected(&format!("`{} {{ ... }}`", self.variant))),
        }
    }
}
