//! `crol::IdOr` (`SPEC.md` 10.5): an id, or a value written in place.

use std::fmt;
use std::marker::PhantomData;

use serde::de::value::{
    BorrowedBytesDeserializer, BorrowedStrDeserializer, BytesDeserializer,
    EnumAccessDeserializer, MapAccessDeserializer, SeqAccessDeserializer, StrDeserializer,
    StringDeserializer, UnitDeserializer,
};
use serde::de::{self, Deserialize, Deserializer, EnumAccess, IntoDeserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{Serialize, Serializer};

use crate::Id;

/// The newtype name through which `IdOr` and the CROL deserializer and
/// serializer recognize each other. Other formats see an ordinary newtype
/// around the id or the value, and pass it through: the plain form
/// (`SPEC.md` 10.8).
pub(crate) const ID_OR_TOKEN: &str = "$crol::IdOr";

/// What a map keyed by `IdOr` is told, by the key deserializer and the key
/// serializer (`SPEC.md` 10.5).
pub(crate) const ID_OR_KEY: &str =
    "`IdOr` cannot be a map key; a map takes id keys (`crol::Id`) or it does not";

/// A place that takes an id, `@name`, or a `T` written in place
/// (`SPEC.md` 10.5).
///
/// ```crol
/// children: [
///     @filter,                                // IdOr::Id
///     Label { text: "q: quit", muted: true }, // IdOr::Value(Node::Label { .. })
/// ]
/// ```
///
/// The kind of the value decides: an id is [`IdOr::Id`], also when `T`
/// could take one, and anything else is [`IdOr::Value`] and must
/// deserialize as `T`. So `IdOr<String>` takes `@web` and `"web"` and tells
/// them apart. From CROL the `T` is deserialized as a field of type `T`
/// would be: its type name picks enum variants, `..` and defaults work,
/// errors carry its place, and ids in it are reported. An id is reported
/// as an [`IdUse`](crate::IdUse) like any other.
///
/// Other serde formats, and serde's own buffer under `#[serde(flatten)]` and
/// `#[serde(untagged)]`, see the plain form: the id as its text, `"@web"`,
/// and the value as `T` writes it. Read back, a string that starts with `@`
/// is the id, and an error if it is no id; anything else goes to `T`. So
/// `IdOr::Value("@web")` reads back from them as the id. A format that does
/// not describe itself (bincode, postcard) cannot read an `IdOr`.
///
/// `IdOr` is no map key: a map takes id keys or it does not (`SPEC.md`
/// 3.1).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IdOr<T> {
    /// `@name`.
    Id(Id),
    /// The value itself.
    Value(T),
}

impl<T> IdOr<T> {
    /// The id, if it is one.
    pub fn as_id(&self) -> Option<&Id> {
        match self {
            IdOr::Id(id) => Some(id),
            IdOr::Value(_) => None,
        }
    }

    /// The value written in place, if it is one.
    pub fn as_value(&self) -> Option<&T> {
        match self {
            IdOr::Id(_) => None,
            IdOr::Value(value) => Some(value),
        }
    }

    /// The value written in place, if it is one, to change.
    pub fn as_value_mut(&mut self) -> Option<&mut T> {
        match self {
            IdOr::Id(_) => None,
            IdOr::Value(value) => Some(value),
        }
    }

    /// The id, if it is one.
    pub fn into_id(self) -> Option<Id> {
        match self {
            IdOr::Id(id) => Some(id),
            IdOr::Value(_) => None,
        }
    }

    /// The value written in place, if it is one.
    pub fn into_value(self) -> Option<T> {
        match self {
            IdOr::Id(_) => None,
            IdOr::Value(value) => Some(value),
        }
    }

    /// Whether it is an id.
    pub fn is_id(&self) -> bool {
        matches!(self, IdOr::Id(_))
    }

    /// Whether it is a value written in place.
    pub fn is_value(&self) -> bool {
        matches!(self, IdOr::Value(_))
    }

    /// The value through `f`, the id as it is.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> IdOr<U> {
        match self {
            IdOr::Id(id) => IdOr::Id(id),
            IdOr::Value(value) => IdOr::Value(f(value)),
        }
    }
}

impl<T> From<Id> for IdOr<T> {
    fn from(id: Id) -> Self {
        IdOr::Id(id)
    }
}

/// The id as its CROL syntax, the value as `T` displays.
impl<T: fmt::Display> fmt::Display for IdOr<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdOr::Id(id) => id.fmt(f),
            IdOr::Value(value) => value.fmt(f),
        }
    }
}

/// A newtype struct with the reserved name around the id or the value: the
/// CROL serializer writes what is inside alone, and other formats pass it
/// through (`SPEC.md` 10.8).
impl<T: Serialize> Serialize for IdOr<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            IdOr::Id(id) => serializer.serialize_newtype_struct(ID_OR_TOKEN, id),
            IdOr::Value(value) => serializer.serialize_newtype_struct(ID_OR_TOKEN, value),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for IdOr<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_newtype_struct(ID_OR_TOKEN, IdOrVisitor(PhantomData))
    }
}

/// The one visitor of `IdOr`. Which method is called tells it where the
/// value comes from (`SPEC.md` 10.5):
///
/// - `visit_some` is a value straight from the CROL deserializer, one that
///   is no id, and reads a `T` with nothing in between;
/// - every other call reads the plain form, in which a string that starts
///   with `@` is the id. A deserializer that passes itself on through
///   `visit_newtype_struct` (serde_json, serde's buffer) is asked for
///   `deserialize_any` with this visitor again; one that forwards a newtype
///   struct to `deserialize_any` calls the other methods directly.
struct IdOrVisitor<T>(PhantomData<T>);

impl<T> IdOrVisitor<T> {
    /// The plain form's text that starts with `@`: an id, or an error, the
    /// one `Id` gives. It is never given to `T` instead.
    fn id<E: de::Error>(text: &str) -> Result<IdOr<T>, E> {
        text.parse::<Id>().map(IdOr::Id).map_err(E::custom)
    }
}

/// A value of the plain form, as `T` reads it: what `D` gives, and to an
/// `Option`, `Some` of it. serde's value deserializers forward
/// `deserialize_option` to `deserialize_any`, so without this an
/// `IdOr<Option<u32>>` would not read `3` where an `Option<u32>` does. The
/// unit, `null` in JSON, does not come through here and stays `None`.
struct Plain<D>(D);

impl<'de, D: Deserializer<'de>> Deserializer<'de> for Plain<D> {
    type Error = D::Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, D::Error> {
        self.0.deserialize_any(visitor)
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, D::Error> {
        visitor.visit_some(self.0)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, D::Error> {
        self.0.deserialize_enum(name, variants, visitor)
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf unit unit_struct newtype_struct seq tuple
        tuple_struct map struct identifier ignored_any
    }
}

/// The plain form's other values: all a `T`.
macro_rules! plain {
    ($($method:ident: $ty:ty),* $(,)?) => {
        $(
            fn $method<E: de::Error>(self, v: $ty) -> Result<IdOr<T>, E> {
                T::deserialize(Plain(v.into_deserializer())).map(IdOr::Value)
            }
        )*
    };
}

impl<'de, T: Deserialize<'de>> Visitor<'de> for IdOrVisitor<T> {
    type Value = IdOr<T>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an id (`@name`) or a value")
    }

    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<IdOr<T>, D::Error> {
        T::deserialize(d).map(IdOr::Value)
    }

    fn visit_newtype_struct<D: Deserializer<'de>>(self, d: D) -> Result<IdOr<T>, D::Error> {
        d.deserialize_any(self)
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<IdOr<T>, E> {
        if v.starts_with('@') {
            return Self::id(v);
        }
        T::deserialize(Plain(StrDeserializer::new(v))).map(IdOr::Value)
    }

    fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<IdOr<T>, E> {
        if v.starts_with('@') {
            return Self::id(v);
        }
        T::deserialize(Plain(BorrowedStrDeserializer::new(v))).map(IdOr::Value)
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<IdOr<T>, E> {
        if v.starts_with('@') {
            return Self::id(&v);
        }
        T::deserialize(Plain(StringDeserializer::new(v))).map(IdOr::Value)
    }

    plain! {
        visit_bool: bool,
        visit_i8: i8,
        visit_i16: i16,
        visit_i32: i32,
        visit_i64: i64,
        visit_i128: i128,
        visit_u8: u8,
        visit_u16: u16,
        visit_u32: u32,
        visit_u64: u64,
        visit_u128: u128,
        visit_f32: f32,
        visit_f64: f64,
        visit_char: char,
    }

    fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<IdOr<T>, E> {
        T::deserialize(Plain(BytesDeserializer::new(v))).map(IdOr::Value)
    }

    fn visit_borrowed_bytes<E: de::Error>(self, v: &'de [u8]) -> Result<IdOr<T>, E> {
        T::deserialize(Plain(BorrowedBytesDeserializer::new(v))).map(IdOr::Value)
    }

    fn visit_byte_buf<E: de::Error>(self, v: Vec<u8>) -> Result<IdOr<T>, E> {
        T::deserialize(Plain(v.into_deserializer())).map(IdOr::Value)
    }

    fn visit_unit<E: de::Error>(self) -> Result<IdOr<T>, E> {
        T::deserialize(UnitDeserializer::new()).map(IdOr::Value)
    }

    fn visit_none<E: de::Error>(self) -> Result<IdOr<T>, E> {
        // A `T` that is an `Option` reads `None` from the unit.
        T::deserialize(UnitDeserializer::new()).map(IdOr::Value)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<IdOr<T>, A::Error> {
        T::deserialize(Plain(SeqAccessDeserializer::new(seq))).map(IdOr::Value)
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<IdOr<T>, A::Error> {
        T::deserialize(Plain(MapAccessDeserializer::new(map))).map(IdOr::Value)
    }

    fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> Result<IdOr<T>, A::Error> {
        T::deserialize(Plain(EnumAccessDeserializer::new(data))).map(IdOr::Value)
    }
}
