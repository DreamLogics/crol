//! CROL, the Composable Rust Object Language.
//!
//! CROL is a text format for configuration and data files that load into
//! Rust data models through serde. It looks like RON, but a document holds
//! operations as well as values: it can declare, replace, patch and remove
//! definitions, including ones written in other files and in
//! lower-priority layers. See `SPEC.md` in the repository for the format.
//!
//! This release reads and writes single documents (value mode, `SPEC.md`
//! 4); layers, variants and the composer come in later releases.
//!
//! ```
//! use serde_derive::{Deserialize, Serialize};
//!
//! #[derive(Debug, PartialEq, Deserialize, Serialize)]
//! struct Config {
//!     listen: String,
//!     admin: String,
//!     workers: Option<u32>,
//! }
//!
//! let config: Config = crol::from_str(r#"
//!     #![type(Config)]
//!
//!     port := 8080,
//!     listen: "0.0.0.0:" + &port,
//!     admin: "127.0.0.1:" + (&port + 1),
//!     ..
//! "#).unwrap();
//! assert_eq!(config.admin, "127.0.0.1:8081");
//! assert_eq!(config.workers, None);
//!
//! let text = crol::to_string_pretty(&config, crol::PrettyConfig::default()).unwrap();
//! assert_eq!(text, "Config { listen: \"0.0.0.0:8080\", admin: \"127.0.0.1:8081\", workers: None }\n");
//! ```

#![forbid(unsafe_code)]

mod de;
mod error;
mod id;
mod ser;

use crol_eval::evaluate_value;
use serde::{Deserialize, Serialize};

pub use crol_eval::{Key, Keyed, Sources, Span, Value, ValueKind};
pub use error::Error;
pub use id::{Id, IdUse};
pub use ser::PrettyConfig;

/// The result type of this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Reads a value-mode document (`SPEC.md` 4) and deserializes it as `T`.
pub fn from_str<T: for<'de> Deserialize<'de>>(text: &str) -> Result<T> {
    from_str_with_ids(text).map(|(value, _)| value)
}

/// Like [`from_str`], and also returns every id the deserializer produced,
/// so that the application can check what they name (`SPEC.md` 10.5).
pub fn from_str_with_ids<T: for<'de> Deserialize<'de>>(text: &str) -> Result<(T, Vec<IdUse>)> {
    let mut sources = Sources::new();
    let file = sources.add("<string>", text);
    let evaluation = evaluate_value(&sources, file);
    let Some(value) = evaluation.value else {
        return Err(Error::from_diagnostics(&sources, &evaluation.diagnostics));
    };
    from_value_with_ids(&value, &sources)
}

/// Deserializes an evaluated value as `T`. `sources` are the files the
/// value was read from, for the locations in errors.
pub fn from_value<T: for<'de> Deserialize<'de>>(value: &Value, sources: &Sources) -> Result<T> {
    let context = de::Context {
        sources,
        options: de::Options::default(),
        ids: Default::default(),
    };
    T::deserialize(de::ValueDeserializer::new(value, &context))
}

fn from_value_with_ids<T: for<'de> Deserialize<'de>>(
    value: &Value,
    sources: &Sources,
) -> Result<(T, Vec<IdUse>)> {
    let context = de::Context {
        sources,
        options: de::Options::default(),
        ids: Default::default(),
    };
    let result = T::deserialize(de::ValueDeserializer::new(value, &context))?;
    Ok((result, context.ids.into_inner()))
}

/// Writes a value as a value-mode document on one line.
pub fn to_string<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    let value = ser::to_value(value, &PrettyConfig::default())?;
    Ok(crol_eval::print::to_text(&value))
}

/// Writes a value as a formatted value-mode document (`SPEC.md` 12.2).
pub fn to_string_pretty<T: Serialize + ?Sized>(value: &T, config: PrettyConfig) -> Result<String> {
    let value = ser::to_value(value, &config)?;
    let format = crol_syntax::format::FormatConfig::default().with_max_width(config.max_width);
    Ok(crol_eval::print::to_document(&value, &format))
}

/// Converts a value to an evaluated [`Value`], as the serializer writes it.
pub fn to_value<T: Serialize + ?Sized>(value: &T) -> Result<Value> {
    ser::to_value(value, &PrettyConfig::default())
}
