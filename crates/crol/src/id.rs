//! `crol::Id` (`SPEC.md` 10.5).

use std::fmt;

use serde::de::{self, Deserialize, Deserializer, Visitor};
use serde::ser::{Serialize, Serializer};

use crol_eval::Span;

/// The newtype name through which `Id` and the CROL deserializer recognize
/// each other. Other formats see an ordinary newtype around a string.
pub(crate) const ID_TOKEN: &str = "$crol::Id";

/// A root key, written `@name` (`SPEC.md` 3.2).
///
/// Unlike a `String`, an `Id` accepts only an id, not a string. As a map key
/// it accepts a string key, since keys cannot be written with `@`. The
/// library checks nothing about what an id names; [`IdUse`] reports every id
/// read, so the application can.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Id(pub String);

impl Id {
    /// The id's text: `web` for `@web`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Id {
    fn from(text: &str) -> Self {
        Self(text.to_owned())
    }
}

impl From<String> for Id {
    fn from(text: String) -> Self {
        Self(text)
    }
}

impl Serialize for Id {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_newtype_struct(ID_TOKEN, &self.0)
    }
}

impl<'de> Deserialize<'de> for Id {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct IdVisitor;

        impl<'de> Visitor<'de> for IdVisitor {
            type Value = Id;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an id")
            }

            fn visit_newtype_struct<D: Deserializer<'de>>(self, d: D) -> Result<Id, D::Error> {
                String::deserialize(d).map(Id)
            }

            fn visit_str<E: de::Error>(self, text: &str) -> Result<Id, E> {
                Ok(Id(text.to_owned()))
            }

            fn visit_string<E: de::Error>(self, text: String) -> Result<Id, E> {
                Ok(Id(text))
            }
        }

        deserializer.deserialize_newtype_struct(ID_TOKEN, IdVisitor)
    }
}

/// An id that the deserializer produced (`SPEC.md` 10.5): which id, where
/// it was written, and where in the Rust value it went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdUse {
    /// The id's text.
    pub id: String,
    /// Where it was written.
    pub span: Span,
    /// The path in the deserialized value: `web.depends_on[0]`.
    pub rust_path: String,
}
