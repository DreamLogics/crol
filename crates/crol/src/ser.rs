//! The serde `Serializer`, which writes plain value documents (`SPEC.md`
//! 10.8). It builds a [`Value`] and prints it, so its output reads back to
//! the same value.

use indexmap::IndexMap;
use serde::ser::{self, Serialize};

use crol_eval::{Key, Keyed, Span, Value, ValueKind};

use crate::error::Error;
use crate::expression::{EXPRESSION_TOKEN, text_value};
use crate::id::ID_TOKEN;
use crate::raw::RAW_TOKEN;
use crol_syntax::is_expression;

/// Options for [`to_string_pretty`](crate::to_string_pretty).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PrettyConfig {
    /// The maximum line width.
    pub max_width: usize,
    /// Write type names: `Service { ... }` rather than `{ ... }`.
    pub struct_names: bool,
    /// Write `Some(x)` as `x`, for an `implicit_some` reader. Off by default,
    /// since `Some(None)` would read back as `None`.
    pub implicit_some: bool,
}

impl Default for PrettyConfig {
    fn default() -> Self {
        Self {
            max_width: 80,
            struct_names: true,
            implicit_some: false,
        }
    }
}

impl PrettyConfig {
    /// Sets the maximum line width.
    pub fn with_max_width(mut self, max_width: usize) -> Self {
        self.max_width = max_width;
        self
    }

    /// Sets whether to write type names.
    pub fn with_struct_names(mut self, struct_names: bool) -> Self {
        self.struct_names = struct_names;
        self
    }

    /// Sets whether to write `Some(x)` as `x`.
    pub fn with_implicit_some(mut self, implicit_some: bool) -> Self {
        self.implicit_some = implicit_some;
        self
    }
}

pub(crate) fn to_value<T: Serialize + ?Sized>(
    value: &T,
    config: &PrettyConfig,
) -> Result<Value, Error> {
    value.serialize(ValueSerializer { config })
}

fn value(kind: ValueKind) -> Value {
    Value::new(kind, Span::detached())
}

struct ValueSerializer<'c> {
    config: &'c PrettyConfig,
}

impl<'c> ser::Serializer for ValueSerializer<'c> {
    type Ok = Value;
    type Error = Error;
    type SerializeSeq = SeqSerializer<'c>;
    type SerializeTuple = SeqSerializer<'c>;
    type SerializeTupleStruct = SeqSerializer<'c>;
    type SerializeTupleVariant = SeqSerializer<'c>;
    type SerializeMap = MapSerializer<'c>;
    type SerializeStruct = MapSerializer<'c>;
    type SerializeStructVariant = MapSerializer<'c>;

    fn serialize_bool(self, v: bool) -> Result<Value, Error> {
        Ok(value(ValueKind::Bool(v)))
    }

    fn serialize_i8(self, v: i8) -> Result<Value, Error> {
        self.serialize_i128(v.into())
    }

    fn serialize_i16(self, v: i16) -> Result<Value, Error> {
        self.serialize_i128(v.into())
    }

    fn serialize_i32(self, v: i32) -> Result<Value, Error> {
        self.serialize_i128(v.into())
    }

    fn serialize_i64(self, v: i64) -> Result<Value, Error> {
        self.serialize_i128(v.into())
    }

    fn serialize_i128(self, v: i128) -> Result<Value, Error> {
        Ok(value(ValueKind::Int(v)))
    }

    fn serialize_u8(self, v: u8) -> Result<Value, Error> {
        self.serialize_i128(v.into())
    }

    fn serialize_u16(self, v: u16) -> Result<Value, Error> {
        self.serialize_i128(v.into())
    }

    fn serialize_u32(self, v: u32) -> Result<Value, Error> {
        self.serialize_i128(v.into())
    }

    fn serialize_u64(self, v: u64) -> Result<Value, Error> {
        self.serialize_i128(v.into())
    }

    fn serialize_u128(self, v: u128) -> Result<Value, Error> {
        i128::try_from(v)
            .map(|v| value(ValueKind::Int(v)))
            .map_err(|_| {
                Error::new(format!(
                    "the integer {v} does not fit in CROL's 128-bit signed integers"
                ))
            })
    }

    fn serialize_f32(self, v: f32) -> Result<Value, Error> {
        // The shortest text that reads back as this `f32`, not its exact
        // value as an `f64`: `0.1` rather than `0.10000000149011612`.
        let wide = v.to_string().parse().unwrap_or(f64::from(v));
        self.serialize_f64(if wide as f32 == v { wide } else { v.into() })
    }

    fn serialize_f64(self, v: f64) -> Result<Value, Error> {
        if v.is_finite() {
            Ok(value(ValueKind::Float(v)))
        } else {
            Err(Error::new(format!(
                "CROL floats are finite; {v} cannot be written"
            )))
        }
    }

    fn serialize_char(self, v: char) -> Result<Value, Error> {
        Ok(value(ValueKind::Char(v)))
    }

    fn serialize_str(self, v: &str) -> Result<Value, Error> {
        Ok(value(ValueKind::String(v.into())))
    }

    fn serialize_bytes(self, v: &[u8]) -> Result<Value, Error> {
        let bytes = v.iter().map(|&b| value(ValueKind::Int(b.into()))).collect();
        Ok(value(ValueKind::List(bytes)))
    }

    fn serialize_none(self) -> Result<Value, Error> {
        Ok(value(ValueKind::UnitName("None".into())))
    }

    fn serialize_some<T: Serialize + ?Sized>(self, v: &T) -> Result<Value, Error> {
        let config = self.config;
        let inner = v.serialize(self)?;
        if config.implicit_some {
            Ok(inner)
        } else {
            Ok(value(ValueKind::NamedTuple("Some".into(), vec![inner])))
        }
    }

    fn serialize_unit(self) -> Result<Value, Error> {
        Ok(value(ValueKind::Unit))
    }

    fn serialize_unit_struct(self, name: &'static str) -> Result<Value, Error> {
        if self.config.struct_names {
            Ok(value(ValueKind::UnitName(name.into())))
        } else {
            Ok(value(ValueKind::Unit))
        }
    }

    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<Value, Error> {
        Ok(value(ValueKind::UnitName(variant.into())))
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        name: &'static str,
        v: &T,
    ) -> Result<Value, Error> {
        let config = self.config;
        let inner = v.serialize(self)?;
        if name == EXPRESSION_TOKEN {
            return match &inner.kind {
                ValueKind::String(text) => Ok(text_value(text)),
                _ => Err(Error::new("an expression must be CROL text")),
            };
        }
        if name == RAW_TOKEN {
            // The value's CROL text, read back as the value itself.
            return match &inner.kind {
                ValueKind::String(text) => match crate::raw::RawValue::from_text(text) {
                    Ok(raw) => Ok(raw.0),
                    // One that holds an `Expression` does not evaluate.
                    Err(_) if is_expression(text) => Ok(text_value(text)),
                    Err(message) => Err(Error::new(message)),
                },
                _ => Err(Error::new("a raw value must be CROL text")),
            };
        }
        if name == ID_TOKEN {
            return match inner.kind {
                ValueKind::String(text) => crol_eval::IdPath::parse(&text)
                    .map(|id| value(ValueKind::Id(id)))
                    .map_err(Error::new),
                _ => Err(Error::new("an id must be a string")),
            };
        }
        if config.struct_names {
            Ok(value(ValueKind::NamedTuple(name.into(), vec![inner])))
        } else {
            Ok(inner)
        }
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        v: &T,
    ) -> Result<Value, Error> {
        let inner = v.serialize(self)?;
        Ok(value(ValueKind::NamedTuple(variant.into(), vec![inner])))
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<SeqSerializer<'c>, Error> {
        Ok(SeqSerializer::new(self.config, SeqKind::List, len))
    }

    fn serialize_tuple(self, len: usize) -> Result<SeqSerializer<'c>, Error> {
        Ok(SeqSerializer::new(self.config, SeqKind::Tuple, Some(len)))
    }

    fn serialize_tuple_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<SeqSerializer<'c>, Error> {
        let kind = if self.config.struct_names {
            SeqKind::Named(name)
        } else {
            SeqKind::Tuple
        };
        Ok(SeqSerializer::new(self.config, kind, Some(len)))
    }

    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<SeqSerializer<'c>, Error> {
        Ok(SeqSerializer::new(
            self.config,
            SeqKind::Named(variant),
            Some(len),
        ))
    }

    fn serialize_map(self, len: Option<usize>) -> Result<MapSerializer<'c>, Error> {
        Ok(MapSerializer::new(self.config, None, len))
    }

    fn serialize_struct(self, name: &'static str, len: usize) -> Result<MapSerializer<'c>, Error> {
        // An empty name (`#[serde(rename = "")]`) writes a block without one.
        let name = (self.config.struct_names && !name.is_empty()).then(|| name.to_owned());
        Ok(MapSerializer::new(self.config, name, Some(len)))
    }

    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<MapSerializer<'c>, Error> {
        Ok(MapSerializer::new(
            self.config,
            Some(variant.to_owned()),
            Some(len),
        ))
    }
}

enum SeqKind {
    List,
    Tuple,
    Named(&'static str),
}

struct SeqSerializer<'c> {
    config: &'c PrettyConfig,
    kind: SeqKind,
    elements: Vec<Value>,
}

impl<'c> SeqSerializer<'c> {
    fn new(config: &'c PrettyConfig, kind: SeqKind, len: Option<usize>) -> Self {
        Self {
            config,
            kind,
            elements: Vec::with_capacity(len.unwrap_or(0)),
        }
    }

    fn push<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Error> {
        self.elements.push(v.serialize(ValueSerializer {
            config: self.config,
        })?);
        Ok(())
    }

    fn finish(self) -> Value {
        value(match self.kind {
            SeqKind::List => ValueKind::List(self.elements),
            SeqKind::Tuple if self.elements.is_empty() => ValueKind::Unit,
            SeqKind::Tuple => ValueKind::Tuple(self.elements),
            SeqKind::Named(name) => ValueKind::NamedTuple(name.into(), self.elements),
        })
    }
}

impl ser::SerializeSeq for SeqSerializer<'_> {
    type Ok = Value;
    type Error = Error;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Error> {
        self.push(v)
    }

    fn end(self) -> Result<Value, Error> {
        Ok(self.finish())
    }
}

impl ser::SerializeTuple for SeqSerializer<'_> {
    type Ok = Value;
    type Error = Error;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Error> {
        self.push(v)
    }

    fn end(self) -> Result<Value, Error> {
        Ok(self.finish())
    }
}

impl ser::SerializeTupleStruct for SeqSerializer<'_> {
    type Ok = Value;
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Error> {
        self.push(v)
    }

    fn end(self) -> Result<Value, Error> {
        Ok(self.finish())
    }
}

impl ser::SerializeTupleVariant for SeqSerializer<'_> {
    type Ok = Value;
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Error> {
        self.push(v)
    }

    fn end(self) -> Result<Value, Error> {
        Ok(self.finish())
    }
}

struct MapSerializer<'c> {
    config: &'c PrettyConfig,
    name: Option<String>,
    entries: IndexMap<Key, Value>,
    next_key: Option<Key>,
    /// Set when a field was skipped: the struct gets `..`.
    defaults: bool,
}

impl<'c> MapSerializer<'c> {
    fn new(config: &'c PrettyConfig, name: Option<String>, len: Option<usize>) -> Self {
        Self {
            config,
            name,
            entries: IndexMap::with_capacity(len.unwrap_or(0)),
            next_key: None,
            defaults: false,
        }
    }

    fn field<T: Serialize + ?Sized>(&mut self, key: &'static str, v: &T) -> Result<(), Error> {
        let v = v.serialize(ValueSerializer {
            config: self.config,
        })?;
        self.entries.insert(Key::Str(key.into()), v);
        Ok(())
    }

    fn finish(self) -> Value {
        value(ValueKind::Keyed(Box::new(Keyed {
            name: self.name.map(Into::into),
            entries: self.entries,
            defaults: self.defaults,
            defaulted: Default::default(),
            scope: false,
        })))
    }
}

impl ser::SerializeMap for MapSerializer<'_> {
    type Ok = Value;
    type Error = Error;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Error> {
        self.next_key = Some(key.serialize(KeySerializer)?);
        Ok(())
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Error> {
        let key = self
            .next_key
            .take()
            .expect("serialize_value follows serialize_key");
        let v = v.serialize(ValueSerializer {
            config: self.config,
        })?;
        self.entries.insert(key, v);
        Ok(())
    }

    fn end(self) -> Result<Value, Error> {
        Ok(self.finish())
    }
}

impl ser::SerializeStruct for MapSerializer<'_> {
    type Ok = Value;
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        v: &T,
    ) -> Result<(), Error> {
        self.field(key, v)
    }

    fn skip_field(&mut self, _: &'static str) -> Result<(), Error> {
        self.defaults = true;
        Ok(())
    }

    fn end(self) -> Result<Value, Error> {
        Ok(self.finish())
    }
}

impl ser::SerializeStructVariant for MapSerializer<'_> {
    type Ok = Value;
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        v: &T,
    ) -> Result<(), Error> {
        self.field(key, v)
    }

    fn skip_field(&mut self, _: &'static str) -> Result<(), Error> {
        self.defaults = true;
        Ok(())
    }

    fn end(self) -> Result<Value, Error> {
        Ok(self.finish())
    }
}

/// Map keys: strings, integers and unit variants (`SPEC.md` 3.1, 10.8).
struct KeySerializer;

fn bad_key(kind: &str) -> Error {
    Error::new(format!(
        "map keys must be strings, integers or unit variants; {kind} cannot be a key"
    ))
}

impl ser::Serializer for KeySerializer {
    type Ok = Key;
    type Error = Error;
    type SerializeSeq = ser::Impossible<Key, Error>;
    type SerializeTuple = ser::Impossible<Key, Error>;
    type SerializeTupleStruct = ser::Impossible<Key, Error>;
    type SerializeTupleVariant = ser::Impossible<Key, Error>;
    type SerializeMap = ser::Impossible<Key, Error>;
    type SerializeStruct = ser::Impossible<Key, Error>;
    type SerializeStructVariant = ser::Impossible<Key, Error>;

    fn serialize_bool(self, _: bool) -> Result<Key, Error> {
        Err(bad_key("a boolean"))
    }

    fn serialize_i8(self, v: i8) -> Result<Key, Error> {
        Ok(Key::Int(v.into()))
    }

    fn serialize_i16(self, v: i16) -> Result<Key, Error> {
        Ok(Key::Int(v.into()))
    }

    fn serialize_i32(self, v: i32) -> Result<Key, Error> {
        Ok(Key::Int(v.into()))
    }

    fn serialize_i64(self, v: i64) -> Result<Key, Error> {
        Ok(Key::Int(v.into()))
    }

    fn serialize_i128(self, v: i128) -> Result<Key, Error> {
        Ok(Key::Int(v))
    }

    fn serialize_u8(self, v: u8) -> Result<Key, Error> {
        Ok(Key::Int(v.into()))
    }

    fn serialize_u16(self, v: u16) -> Result<Key, Error> {
        Ok(Key::Int(v.into()))
    }

    fn serialize_u32(self, v: u32) -> Result<Key, Error> {
        Ok(Key::Int(v.into()))
    }

    fn serialize_u64(self, v: u64) -> Result<Key, Error> {
        Ok(Key::Int(v.into()))
    }

    fn serialize_u128(self, v: u128) -> Result<Key, Error> {
        i128::try_from(v)
            .map(Key::Int)
            .map_err(|_| bad_key("an integer this large"))
    }

    fn serialize_f32(self, _: f32) -> Result<Key, Error> {
        Err(bad_key("a float"))
    }

    fn serialize_f64(self, _: f64) -> Result<Key, Error> {
        Err(bad_key("a float"))
    }

    fn serialize_char(self, _: char) -> Result<Key, Error> {
        Err(bad_key("a char"))
    }

    fn serialize_str(self, v: &str) -> Result<Key, Error> {
        Ok(Key::Str(v.into()))
    }

    fn serialize_bytes(self, _: &[u8]) -> Result<Key, Error> {
        Err(bad_key("a byte string"))
    }

    fn serialize_none(self) -> Result<Key, Error> {
        Err(bad_key("an option"))
    }

    fn serialize_some<T: Serialize + ?Sized>(self, _: &T) -> Result<Key, Error> {
        Err(bad_key("an option"))
    }

    fn serialize_unit(self) -> Result<Key, Error> {
        Err(bad_key("the unit value"))
    }

    fn serialize_unit_struct(self, _: &'static str) -> Result<Key, Error> {
        Err(bad_key("a unit struct"))
    }

    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<Key, Error> {
        Ok(Key::Str(variant.into()))
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        name: &'static str,
        v: &T,
    ) -> Result<Key, Error> {
        // An id key is written without `@` (`SPEC.md` 10.5), and only a
        // single key can be one.
        if name == ID_TOKEN {
            let Key::Str(text) = v.serialize(self)? else {
                return Err(Error::new("an id must be a string"));
            };
            let id = crol_eval::IdPath::parse(&text).map_err(Error::new)?;
            return match id.name() {
                Some(name) => Ok(Key::Str(name.into())),
                None => Err(bad_key(&format!("the path id `{id}`"))),
            };
        }
        v.serialize(self)
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: &T,
    ) -> Result<Key, Error> {
        Err(bad_key("a newtype variant"))
    }

    fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, Error> {
        Err(bad_key("a list"))
    }

    fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, Error> {
        Err(bad_key("a tuple"))
    }

    fn serialize_tuple_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleStruct, Error> {
        Err(bad_key("a tuple struct"))
    }

    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleVariant, Error> {
        Err(bad_key("a tuple variant"))
    }

    fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, Error> {
        Err(bad_key("a map"))
    }

    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self::SerializeStruct, Error> {
        Err(bad_key("a struct"))
    }

    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStructVariant, Error> {
        Err(bad_key("a struct variant"))
    }
}
