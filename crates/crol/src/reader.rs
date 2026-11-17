//! Reading value-mode documents with options: a schema and constants.

use crol_eval::{Constants, Sources, evaluate_value_with};
use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::id::IdUse;
use crate::schema::SchemaFile;
use crate::{de, resolve};

/// Reads value-mode documents (`SPEC.md` 4) with what [`crate::from_str`]
/// has no place for: a schema, and constants the document reads as `$name`
/// (`SPEC.md` 6.1.4).
///
/// ```
/// #[derive(serde::Deserialize)]
/// struct Config { data_dir: String, workers: u32 }
///
/// let config: Config = crol::CrolReader::new()
///     .with_constant("home", "/home/me")?
///     .from_str(r#"data_dir: $home + "/data", workers: $workers ?? 4"#)?;
/// assert_eq!(config.data_dir, "/home/me/data");
/// assert_eq!(config.workers, 4);
/// # Ok::<(), crol::Error>(())
/// ```
///
/// [`crate::from_str`], [`crate::from_str_with_schema`] and
/// [`crate::from_str_with_ids`] are shorthands for it, with no constants:
/// every `$name` there is missing.
#[derive(Debug, Clone, Default)]
pub struct CrolReader {
    schema: Option<SchemaFile>,
    constants: Constants,
    name: Option<String>,
}

impl CrolReader {
    /// A reader with no schema and no constants.
    pub fn new() -> Self {
        Self::default()
    }

    /// Gives the reader a schema, as [`crate::from_str_with_schema`] does:
    /// the deserializer learns which names are aliases of one field and each
    /// type's `#[crol(explicit_defaults)]`, and the constants it declares
    /// must be given and fit their types. `&default` stays an error, as in
    /// every value-mode document.
    pub fn schema(mut self, schema: SchemaFile) -> Self {
        self.schema = Some(schema);
        self
    }

    /// The name errors give the text, such as its file's; `<string>` by
    /// default.
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Gives the document a constant, which it reads as `$name` (`SPEC.md`
    /// 6.1.4). It fails as
    /// [`CrolComposer::with_constant`](crate::CrolComposer::with_constant)
    /// does.
    pub fn with_constant(mut self, name: &str, value: impl Serialize) -> Result<Self, Error> {
        if self.constants.get(name).is_some() {
            return Err(Error::new(format!("reader: constant `{name}` is given twice")));
        }
        let value = crate::constants::constant_value(name, &value, self.schema.as_ref())?;
        self.constants.insert(name, value);
        Ok(self)
    }

    /// Reads `text` and deserializes it as `T`.
    pub fn from_str<T: for<'de> Deserialize<'de>>(&self, text: &str) -> Result<T, Error> {
        self.from_str_with_ids(text).map(|(value, _)| value)
    }

    /// Like [`from_str`](Self::from_str), and also returns every id the
    /// deserializer produced, so that the application can check what they
    /// name (`SPEC.md` 10.5).
    pub fn from_str_with_ids<T: for<'de> Deserialize<'de>>(
        &self,
        text: &str,
    ) -> Result<(T, Vec<IdUse>), Error> {
        let mut sources = Sources::new();
        let file = sources.add(self.name.as_deref().unwrap_or("<string>"), text);
        let mut constants = self.constants.clone();
        let mut diagnostics =
            crate::constants::check_declared(&mut constants, self.schema.as_ref());
        let mut evaluation = evaluate_value_with(&sources, file, &constants);
        diagnostics.append(&mut evaluation.diagnostics);
        let value = evaluation
            .value
            .filter(|_| !diagnostics.iter().any(crol_eval::Diagnostic::is_error));
        let Some(value) = value else {
            return Err(Error::from_diagnostics(&sources, &diagnostics));
        };
        let options = de::Options {
            structs: self
                .schema
                .as_ref()
                .map(|schema| std::sync::Arc::new(de::Structs::from_schema(schema))),
            ..de::Options::default()
        };
        // A value document has no schema for scopes, so only `#[scope]`
        // makes them.
        let resolver = resolve::Resolver::new(&value, None);
        crate::deserialize_resolved(&value, &sources, options, None, Some(resolver))
    }
}
