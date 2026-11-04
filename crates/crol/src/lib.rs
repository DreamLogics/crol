//! CROL, the Composable Rust Object Language: configuration and data files
//! that combine with fixed rules, and load into your types through serde.
//!
//! CROL looks like RON, but a document holds _operations_ as well as values:
//! it can declare, replace, patch and remove definitions, including ones
//! written in other files. At a high level, this crate provides:
//!
//! * Functions for [reading and writing one document][one-document], much like
//!   `serde_json`.
//! * A [composer][layers] that stacks files in layers and loads the result into
//!   your types.
//! * [Variants][variants], for differences per environment, region or feature.
//! * [Schemas][schemas], generated from your Rust types, which let tools check
//!   data without running your program.
//!
//! Guide-level documentation is found in the [guide].
//!
//! [one-document]: #reading-one-document
//! [layers]: #layers
//! [variants]: #variants
//! [schemas]: #schemas
//! [guide]: https://github.com/DreamLogics/crol/blob/main/docs/README.md
//!
//! # A tour of CROL
//!
//! This section is a brief tour of the crate's main APIs and when to use them.
//! CROL is not on crates.io yet, so add it from the repository, along with
//! serde:
//!
//! ```toml
//! [dependencies]
//! crol = { git = "https://github.com/DreamLogics/crol" }
//! serde = { version = "1", features = ["derive"] }
//! ```
//!
//! ## Reading one document
//!
//! If your program reads a single settings file, a `.crol` file, [`from_str`]
//! is all you need. The file is evaluated first, so it can compute values, and
//! your type only sees the result:
//!
//! ```
//! use serde::Deserialize;
//!
//! #[derive(Debug, PartialEq, Deserialize)]
//! struct Config {
//!     listen: String,
//!     admin: String,
//!     workers: Option<u32>,
//! }
//!
//! let config: Config = crol::from_str(r#"
//!     #![type(Config)]
//!
//!     port := 8080
//!     listen: "0.0.0.0:" + &port
//!     admin: "127.0.0.1:" + (&port + 1)
//!     ..
//! "#)?;
//!
//! assert_eq!(config.admin, "127.0.0.1:8081");
//! assert_eq!(config.workers, None);
//! # Ok::<(), crol::Error>(())
//! ```
//!
//! Every field of a struct must be written, `Option` fields included, so that a
//! typo cannot quietly turn into a default. The `..` at the end says that
//! leaving out `workers` is on purpose. A type can also decide this for itself,
//! with `#[crol(explicit_defaults)]`; the loader then needs its schema, through
//! `from_str_with_schema` or [`CrolComposer::schema`].
//!
//! [`to_string`] and [`to_string_pretty`] write any `Serialize` value back:
//!
//! ```
//! # use serde::Serialize;
//! #[derive(Serialize)]
//! struct Config {
//!     port: u16,
//! }
//!
//! let text = crol::to_string_pretty(&Config { port: 80 }, crol::PrettyConfig::default())?;
//! assert_eq!(text, "Config { port: 80 }\n");
//! # Ok::<(), crol::Error>(())
//! ```
//!
//! ## Layers
//!
//! To combine several files, `.crod` files, use [`CrolComposer`]. Each [`load`]
//! adds a layer on top of the ones before it, from a directory, a file, or a
//! [`CrolLayer`] built in code, and [`compose`] evaluates them and deserializes
//! the result:
//!
//! ```
//! use std::collections::BTreeMap;
//!
//! use crol::{CrolComposer, CrolLayer};
//! use serde::Deserialize;
//!
//! #[derive(Deserialize)]
//! struct Service {
//!     port: u16,
//!     url: String,
//! }
//!
//! let base = CrolLayer::from_sources("base", [("services.crod", r#"
//!     +web: Service {
//!         port: 80,
//!         url: "http://web:" + &self.port,
//!     }
//! "#)]);
//! let local = CrolLayer::from_sources("local", [("overrides.crod", "web.port: 8080")]);
//!
//! let services: BTreeMap<String, Service> = CrolComposer::new()
//!     .load_layer(base)
//!     .load_layer(local)
//!     .compose()?;
//!
//! assert_eq!(services["web"].url, "http://web:8080");
//! # assert_eq!(services["web"].port, 8080);
//! # Ok::<(), crol::Error>(())
//! ```
//!
//! `url` changed even though the `local` layer never mentions it: `&self.port`
//! reads the final value, after all layers.
//!
//! A composer reads files in _data mode_, where a file is a list of operations
//! on shared data, and [`from_str`] in _value mode_, where a file is one value.
//! [Data mode and value mode][modes] explains the difference.
//!
//! Documents do not have to be files: a `FileLoader`, registered with
//! `with_file_loader`, can serve module paths from anywhere. See
//! [the recipe][loaders], which also explains `without_file_system`, for
//! documents you do not trust.
//!
//! To read more than one value, use [`compose_data`], which returns a
//! [`CrolData`]. It also gives you warnings, a [fingerprint] of the data, and
//! the [history][explain] of any value.
//!
//! [`load`]: CrolComposer::load
//! [`compose`]: CrolComposer::compose
//! [`compose_data`]: CrolComposer::compose_data
//! [fingerprint]: CrolData::fingerprint
//! [explain]: CrolData::explain
//! [modes]: https://github.com/DreamLogics/crol/blob/main/docs/modes.md
//! [loaders]: https://github.com/DreamLogics/crol/blob/main/docs/recipes.md#loading-documents-from-somewhere-else
//!
//! ## Variants
//!
//! A variant is a named alternative of the data, such as an environment. Your
//! program declares them with [`CrolVariants`], and chooses the active ones
//! with [`with_variant`]:
//!
//! ```
//! use crol::{CrolComposer, CrolConfig, CrolLayer, CrolVariants};
//! use serde::Deserialize;
//!
//! #[derive(Deserialize)]
//! struct Root {
//!     replicas: u32,
//! }
//!
//! let variants = CrolVariants::new()
//!     .add_grouped(["dev", "prod", "prod_eu"])
//!     .add_parent("prod_eu", "prod");
//!
//! let layer = CrolLayer::from_sources("base", [("root.crod", r#"
//!     replicas: 2
//!     <prod> {
//!         replicas: & * 4,
//!     }
//!     <prod_eu> {
//!         replicas: & + 1,
//!     }
//! "#)]);
//!
//! let root: Root = CrolComposer::with_cfg(CrolConfig { variants, ..Default::default() })
//!     .load_layer(layer)
//!     .with_variant("prod_eu")?
//!     .compose()?;
//!
//! assert_eq!(root.replicas, 9);
//! # Ok::<(), crol::Error>(())
//! ```
//!
//! With the `derive` feature, `#[derive(crol::VariantName)]` lets you use an
//! enum of your own instead of strings, so the compiler checks what you pass.
//!
//! [`with_variant`]: CrolComposer::with_variant
//!
//! ## Ids
//!
//! An id, `@web`, names a root key. Deserialize one into [`Id`], which accepts
//! only ids. CROL does not check what an id points at:
//! [`get_with_ids`](CrolData::get_with_ids) reports the ids it saw, so that you
//! can, and [`check()`] does it for you with a schema.
//!
//! ## Schemas
//!
//! Loading needs no schema, but tools do: a schema tells `crol check` and the
//! language server what your types look like. You do not write one. With the
//! `derive` feature, derive it, and write the file from a test:
//!
//! ```no_run
//! use serde::Deserialize;
//! # use crol_derive::Schema;
//!
//! /// A service that runs one container image.
//! #[derive(Deserialize, Schema)] // `crol::Schema`
//! pub struct Service {
//!     /// The port it listens on.
//!     pub port: u16,
//! }
//!
//! #[derive(Deserialize, Schema)]
//! pub enum Definition {
//!     Service(Service),
//! }
//!
//! // In a test. It fails if the file is out of date;
//! // `CROL_UPDATE_SCHEMA=1 cargo test` updates it.
//! crol::schema::check_file::<Definition>("crol/schema.crol").unwrap();
//! ```
//!
//! The [`schema`] module has the model and functions, and [`check()`] applies
//! a schema to evaluated data from Rust. The [`project`] module reads
//! `project.crol`, the file that tells tools about your layers. The guide's
//! pages on [schemas][schema-guide] and [projects][project-guide] explain how
//! it all fits together.
//!
//! [schema-guide]: https://github.com/DreamLogics/crol/blob/main/docs/schemas.md
//! [project-guide]: https://github.com/DreamLogics/crol/blob/main/docs/projects.md
//!
//! # Errors
//!
//! Everything fails with [`Error`], whose `Display` says what went wrong and
//! where. An error from evaluating carries every problem found, in
//! [`Error::diagnostics`]:
//!
//! ```
//! let error = crol::from_str::<u16>("-1").unwrap_err();
//! assert_eq!(error.to_string(), "expected u16, found -1 (<string>:1:1)");
//! ```
//!
//! # Feature flags
//!
//! CROL has no default features, and needs none to load data.
//!
//! - `derive`: `#[derive(crol::Schema)]` and `#[derive(crol::VariantName)]`.
//! - `remote-schemas`: fetch schemas named by URL or crate, with a blocking
//!   client, for tools of your own.
//! - `remote-schemas-async`: the same, without blocking, using Tokio and
//!   reqwest.
//!
//! # Where to go next
//!
//! - [Getting started](https://github.com/DreamLogics/crol/blob/main/docs/getting-started.md)
//! - [The language by example](https://github.com/DreamLogics/crol/blob/main/docs/language.md)
//! - [Layers](https://github.com/DreamLogics/crol/blob/main/docs/layers.md)
//!   and [variants](https://github.com/DreamLogics/crol/blob/main/docs/variants.md)
//! - [Loading data in Rust](https://github.com/DreamLogics/crol/blob/main/docs/rust.md)
//! - [The command-line tool](https://github.com/DreamLogics/crol/blob/main/docs/cli.md)
//!   and [editor setup](https://github.com/DreamLogics/crol/blob/main/docs/editors.md)

#![forbid(unsafe_code)]

mod check;
mod compose;
mod de;
mod error;
mod fill;
mod id;
mod loader;
mod mode;
mod module_path;
pub mod project;
mod raw;
#[cfg(feature = "_remote")]
mod remote;
pub mod schema;
mod ser;
mod view;
mod writer;

use crol_eval::evaluate_value;
use serde::{Deserialize, Serialize};

pub use check::{CheckOptions, DataView, check, check_value, check_view, value_type};
pub use compose::{
    CrolComposer, CrolConfig, CrolContext, CrolData, CrolLayer, CrolVariants, ExtendsLoad,
    LayerSource, Variant, VariantName,
};
pub use crol_eval::{ExplainStep, StepKind, VariantKeys};
pub use crol_eval::{Key, Keyed, Sources, Span, Value, ValueKind};
pub use error::Error;
pub use fill::{fill_defaults, fill_defaults_value};

/// The error type of `crol`, as the specification names it.
pub type CrolError = Error;
pub use id::{Id, IdUse};
pub use loader::{
    FileLoader, FileLoaders, FsLoader, Loaded, LoaderError, ModulePath, ResolvedSource, SourceId,
};
pub use mode::Mode;
pub use raw::RawValue;
pub use schema::Schema;

#[cfg(feature = "derive")]
pub use crol_derive::{Schema, VariantName, View};
pub use ser::PrettyConfig;
pub use view::{Query, Variation, View, ViewSource};
pub use writer::DataWriter;

/// The result type of this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Reads a value-mode document (`SPEC.md` 4) and deserializes it as `T`.
///
/// This sees only what serde gives it: `#[crol(explicit_defaults)]` and
/// which names are aliases of one field are in the schema, so a struct
/// must write every field or `..`, whatever its type says. Use
/// [`from_str_with_schema`] for those (`SPEC.md` 10.3).
pub fn from_str<T: for<'de> Deserialize<'de>>(text: &str) -> Result<T> {
    from_str_with_ids(text).map(|(value, _)| value)
}

/// [`from_str`] with a schema, as `CrolComposer::schema` gives one to the
/// composer: the deserializer learns which names are aliases of one field
/// and each type's `#[crol(explicit_defaults)]` (`SPEC.md` 10.2, 10.3).
/// `&default` stays an error, as in every value-mode document.
pub fn from_str_with_schema<T: for<'de> Deserialize<'de>>(
    text: &str,
    schema: &schema::SchemaFile,
) -> Result<T> {
    let options = de::Options {
        structs: Some(std::sync::Arc::new(de::Structs::from_schema(schema))),
        ..de::Options::default()
    };
    from_text("<string>", text, options).map(|(value, _)| value)
}

/// [`from_str`] for text that has a name, such as a file's, which errors
/// then point at.
pub(crate) fn from_named_str<T: for<'de> Deserialize<'de>>(name: &str, text: &str) -> Result<T> {
    from_text(name, text, de::Options::default()).map(|(value, _)| value)
}

/// Like [`from_str`], and also returns every id the deserializer produced,
/// so that the application can check what they name (`SPEC.md` 10.5).
pub fn from_str_with_ids<T: for<'de> Deserialize<'de>>(text: &str) -> Result<(T, Vec<IdUse>)> {
    from_text("<string>", text, de::Options::default())
}

fn from_text<T: for<'de> Deserialize<'de>>(
    name: &str,
    text: &str,
    options: de::Options,
) -> Result<(T, Vec<IdUse>)> {
    let mut sources = Sources::new();
    let file = sources.add(name, text);
    let evaluation = evaluate_value(&sources, file);
    let Some(value) = evaluation.value else {
        return Err(Error::from_diagnostics(&sources, &evaluation.diagnostics));
    };
    deserialize(&value, &sources, options, None)
}

/// Deserializes an evaluated value as `T`. `sources` are the files the
/// value was read from, for the locations in errors.
pub fn from_value<T: for<'de> Deserialize<'de>>(value: &Value, sources: &Sources) -> Result<T> {
    deserialize(value, sources, de::Options::default(), None).map(|(value, _)| value)
}

pub use module_path::directory_of;

/// Deserializes `value`, which is at the root key `key` if given, for the
/// paths in errors.
/// Resolves a module path (`SPEC.md` 4.2) to the file or directory it names.
/// A module path has no extension: `mode` says which one the file has.
/// `here` is the directory of the document it is written in; `None` starts
/// relative paths from `root`.
pub fn resolve_module_path(
    module_path: &str,
    mode: Mode,
    root: &std::path::Path,
    here: Option<&std::path::Path>,
) -> Result<std::path::PathBuf> {
    module_path::resolve(module_path, root, here, mode)
}

pub(crate) fn deserialize<T: for<'de> Deserialize<'de>>(
    value: &Value,
    sources: &Sources,
    options: de::Options,
    key: Option<&str>,
) -> Result<(T, Vec<IdUse>)> {
    let context = de::Context {
        sources,
        options,
        ids: Default::default(),
    };
    let deserializer = match key {
        Some(key) => de::ValueDeserializer::at_key(value, &context, key),
        None => de::ValueDeserializer::new(value, &context),
    };
    let result = T::deserialize(deserializer)?;
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
