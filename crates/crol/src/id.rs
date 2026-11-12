//! `crol::Id` (`SPEC.md` 10.5).

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use serde::de::{self, Deserialize, Deserializer, Visitor};
use serde::ser::{Serialize, Serializer};

use crol_eval::{IdPath, Span};

use crate::Error;
use crate::resolve::DataPath;

/// The newtype name through which `Id` and the CROL deserializer recognize
/// each other. Inside it is the id's text, `@` included; other formats see an
/// ordinary newtype around that string.
pub(crate) const ID_TOKEN: &str = "$crol::Id";

/// A name of something in the data: `@web`, `@settings.filter`, `@::web`
/// (`SPEC.md` 3.2, 10.5).
///
/// Unlike a `String`, an `Id` accepts only an id, not a string. As a map key
/// it accepts only an id key, `@web: 1`, and a map of another key type takes
/// none. An id is a name, not a lookup; [`IdUse`] reports every id read, a
/// key as well, so the application can check what it names.
///
/// As text, in [`Display`](fmt::Display), [`FromStr`] and other serde
/// formats, an id is its CROL syntax with the `@`: `"@web"`. Equality is by
/// what was written, so `@x` and `@::x` are different ids.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Id {
    path: Vec<String>,
    rooted: bool,
}

impl Id {
    /// An id of one key: `@web` for `"web"`.
    pub fn key(name: impl Into<String>) -> Id {
        Id {
            path: vec![name.into()],
            rooted: false,
        }
    }

    /// The keys as written, at least one: `["settings", "filter"]` for
    /// `@settings.filter`.
    pub fn path(&self) -> &[String] {
        &self.path
    }

    /// The single key of an id like `@web`; `None` for a path id or one that
    /// starts from the root.
    pub fn name(&self) -> Option<&str> {
        match self.path.as_slice() {
            [name] if !self.rooted => Some(name),
            _ => None,
        }
    }

    /// Whether it starts from the root: `@::web`.
    pub fn is_rooted(&self) -> bool {
        self.rooted
    }

    pub(crate) fn from_path(id: &IdPath) -> Id {
        Id {
            path: id.keys().iter().map(|key| key.to_string()).collect(),
            rooted: id.is_rooted(),
        }
    }

    pub(crate) fn to_path(&self) -> IdPath {
        IdPath::new(
            self.rooted,
            self.path.iter().map(|key| Arc::from(key.as_str())),
        )
        .expect("an id has a key")
    }
}

/// `@""`: the id of the empty key, so that types with an `Id` can derive
/// `Default`.
impl Default for Id {
    fn default() -> Self {
        Id::key("")
    }
}

/// The CROL syntax: `@web`, `@"odd id"`, `@a.b`, `@::a`.
impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.to_path().fmt(f)
    }
}

/// Reads the CROL syntax, `@` included.
impl FromStr for Id {
    type Err = Error;

    fn from_str(text: &str) -> Result<Id, Error> {
        IdPath::parse(text)
            .map(|id| Id::from_path(&id))
            .map_err(Error::new)
    }
}

impl Serialize for Id {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_newtype_struct(ID_TOKEN, &self.to_string())
    }
}

impl<'de> Deserialize<'de> for Id {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct IdVisitor;

        impl<'de> Visitor<'de> for IdVisitor {
            type Value = Id;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an id (`@name`)")
            }

            fn visit_newtype_struct<D: Deserializer<'de>>(self, d: D) -> Result<Id, D::Error> {
                d.deserialize_str(self)
            }

            // Text, from the CROL deserializer, a buffered value (`flatten`,
            // `untagged`) or another format: an id only if it starts with `@`.
            fn visit_str<E: de::Error>(self, text: &str) -> Result<Id, E> {
                if !text.starts_with('@') {
                    return Err(E::invalid_value(de::Unexpected::Str(text), &self));
                }
                IdPath::parse(text)
                    .map(|id| Id::from_path(&id))
                    .map_err(E::custom)
            }
        }

        deserializer.deserialize_newtype_struct(ID_TOKEN, IdVisitor)
    }
}

/// The text an id counts as where it stands for a name, as in `OneOf`
/// (`SPEC.md` 10.12): its key for `@web`, else its syntax, `@a.b`.
pub(crate) fn id_text(id: &IdPath) -> std::borrow::Cow<'_, str> {
    match id.name() {
        Some(name) => name.into(),
        None => id.to_string().into(),
    }
}

/// An id that the deserializer produced (`SPEC.md` 10.5): which id, where
/// it was written, and where in the Rust value it went.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct IdUse {
    /// The id as written.
    pub id: Id,
    /// Where it was written; for an id key, where its entry's value was.
    pub span: Span,
    /// The path in the deserialized value: `web.depends_on[0]`; for an id
    /// key, that of its entry, `grenade.bonus[@is_biological]`.
    pub rust_path: String,
    /// The place in the data the id resolved to, or `None` if it names
    /// nothing there (`SPEC.md` 10.5).
    pub target: Option<DataPath>,
}
