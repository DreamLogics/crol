//! `RawValue`: a CROL value inside a serde data structure, such as the
//! default values in a schema file (`SPEC.md` 10.10).

use std::fmt;

use serde::de::{self, Deserialize, Deserializer, Visitor};
use serde::ser::{Serialize, Serializer};

use crol_eval::{Sources, Value, evaluate_value, print};

/// The newtype name through which `RawValue` and the CROL serializer and
/// deserializer recognize each other. Other formats see the value's CROL
/// text, as a string.
pub(crate) const RAW_TOKEN: &str = "$crol::RawValue";

/// A CROL value, kept as it is through serde. Written by `crol::to_string`
/// as the value itself; written by other formats as its CROL text.
#[derive(Debug, Clone)]
pub struct RawValue(pub Value);

impl RawValue {
    /// The value's CROL text, on one line.
    pub fn to_text(&self) -> String {
        print::to_text(&self.0)
    }

    /// Reads a value from CROL text (a value-mode document).
    pub fn from_text(text: &str) -> Result<Self, String> {
        let mut sources = Sources::new();
        let file = sources.add("<value>", text);
        let evaluation = evaluate_value(&sources, file);
        match evaluation.value {
            Some(value) => Ok(RawValue(value)),
            None => Err(evaluation
                .diagnostics
                .first()
                .map_or_else(|| "invalid value".to_owned(), |d| d.message.clone())),
        }
    }
}

impl PartialEq for RawValue {
    fn eq(&self, other: &Self) -> bool {
        self.0.identical(&other.0)
    }
}

impl Serialize for RawValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_newtype_struct(RAW_TOKEN, &self.to_text())
    }
}

impl<'de> Deserialize<'de> for RawValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RawVisitor;

        impl<'de> Visitor<'de> for RawVisitor {
            type Value = RawValue;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a CROL value")
            }

            fn visit_newtype_struct<D: Deserializer<'de>>(
                self,
                d: D,
            ) -> Result<RawValue, D::Error> {
                let text = String::deserialize(d)?;
                RawValue::from_text(&text).map_err(de::Error::custom)
            }

            fn visit_str<E: de::Error>(self, text: &str) -> Result<RawValue, E> {
                RawValue::from_text(text).map_err(E::custom)
            }
        }

        deserializer.deserialize_newtype_struct(RAW_TOKEN, RawVisitor)
    }
}
