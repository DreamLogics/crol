//! The composer: layers in, Rust values out (`SPEC.md` 7, 10.7).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crol_eval::{DataOptions, Diagnostic, Key, Layer, Sources, Value, ValueKind, evaluate_data};
use crol_syntax::ast::{self, AstNode};
use serde::Deserialize;

use crate::de;
use crate::error::Error;
use crate::id::IdUse;
use crate::module_path;

/// How a [`CrolComposer`] loads and composes (`SPEC.md` 10.7).
///
/// Fields for variants and contexts come with variant support.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrolConfig {
    /// Where `::` module paths start (`SPEC.md` 4.2). Default: the working
    /// directory.
    pub root_path: PathBuf,
    /// Follow `#![extends(...)]`: load the extended documents as layers
    /// below the one that extends them (`SPEC.md` 4.1). Default: `false`.
    pub load_extends: bool,
    /// Accept any value where an `Option` is expected, as `Some`.
    /// Default: `true`.
    pub implicit_some: bool,
    /// Ignore keys a struct does not have, instead of an error.
    /// Default: `false`.
    pub allow_unknown_fields: bool,
    /// Require every field of a struct unless it ends with `..`.
    /// Default: `true`.
    pub require_complete: bool,
    /// Warn when one key is patched from several documents of one layer.
    /// Default: `true`.
    pub warn_cross_file_patches: bool,
}

impl Default for CrolConfig {
    fn default() -> Self {
        Self {
            root_path: std::env::current_dir().unwrap_or_default(),
            load_extends: false,
            implicit_some: true,
            allow_unknown_fields: false,
            require_complete: true,
            warn_cross_file_patches: true,
        }
    }
}

impl CrolConfig {
    fn deserialize_options(&self) -> de::Options {
        de::Options {
            implicit_some: self.implicit_some,
            allow_unknown_fields: self.allow_unknown_fields,
            require_complete: self.require_complete,
        }
    }
}

/// A layer: a named, ordered set of documents (`SPEC.md` 7.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrolLayer {
    name: String,
    /// Each document's path relative to the layer, its name for messages,
    /// its text, and the directory it is in, for its module paths.
    documents: Vec<Document>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Document {
    relative: String,
    display: String,
    text: String,
    directory: Option<PathBuf>,
    canonical: Option<PathBuf>,
}

impl CrolLayer {
    /// A layer of in-memory documents, for tests, editors and embedded data.
    /// Each document is a path relative to the layer and its text.
    pub fn from_sources<P, T>(
        name: impl Into<String>,
        documents: impl IntoIterator<Item = (P, T)>,
    ) -> Self
    where
        P: Into<String>,
        T: Into<String>,
    {
        let name = name.into();
        let documents = documents
            .into_iter()
            .map(|(path, text)| {
                let relative = path.into();
                Document {
                    display: format!("{name}/{relative}"),
                    relative,
                    text: text.into(),
                    directory: None,
                    canonical: None,
                }
            })
            .collect();
        Self { name, documents }
    }

    /// A layer of one file, or of every `.crol` file below a directory
    /// (`SPEC.md` 7.1). Hidden files and directories are skipped.
    pub fn from_path(name: impl Into<String>, path: &Path) -> Result<Self, Error> {
        let name = name.into();
        let mut documents = Vec::new();
        if path.is_dir() {
            read_directory(path, path, &mut documents)?;
        } else {
            let relative = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            documents.push(read_document(path, relative)?);
        }
        Ok(Self { name, documents })
    }

    /// The layer's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The layer, renamed.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// The documents: each one's path relative to the layer, its name for
    /// messages (a file system path, or `layer/path` in memory), and its
    /// text.
    pub fn documents(&self) -> impl Iterator<Item = (&str, &str, &str)> {
        self.documents
            .iter()
            .map(|d| (d.relative.as_str(), d.display.as_str(), d.text.as_str()))
    }
}

fn read_document(path: &Path, relative: String) -> Result<Document, Error> {
    let bytes = std::fs::read(path)
        .map_err(|e| Error::new(format!("cannot read {}: {e}", path.display())))?;
    let text = String::from_utf8(bytes)
        .map_err(|_| Error::new(format!("{} is not valid UTF-8", path.display())))?;
    Ok(Document {
        relative,
        display: path.display().to_string(),
        text,
        directory: path.parent().map(Path::to_path_buf),
        canonical: path.canonicalize().ok(),
    })
}

fn read_directory(base: &Path, dir: &Path, documents: &mut Vec<Document>) -> Result<(), Error> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| Error::new(format!("cannot read {}: {e}", dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| Error::new(format!("cannot read {}: {e}", dir.display())))?;
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        // `is_dir` and `is_file` follow symbolic links.
        if path.is_dir() {
            read_directory(base, &path, documents)?;
        } else if path.is_file() && path.extension().is_some_and(|e| e == "crol") {
            let relative = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            documents.push(read_document(&path, relative)?);
        }
    }
    Ok(())
}

/// Where a layer comes from: a module path (`&str`, `String`) or a file
/// system path (`&Path`, `PathBuf`).
pub trait LayerSource {
    /// Loads the layer.
    #[doc(hidden)]
    fn load(self, config: &CrolConfig) -> Result<CrolLayer, Error>;
}

impl LayerSource for &str {
    fn load(self, config: &CrolConfig) -> Result<CrolLayer, Error> {
        let path = module_path::resolve(self, &config.root_path, None)?;
        CrolLayer::from_path(self, &path)
    }
}

impl LayerSource for String {
    fn load(self, config: &CrolConfig) -> Result<CrolLayer, Error> {
        self.as_str().load(config)
    }
}

impl LayerSource for &Path {
    fn load(self, _: &CrolConfig) -> Result<CrolLayer, Error> {
        if !self.exists() {
            return Err(Error::new(format!("{} does not exist", self.display())));
        }
        CrolLayer::from_path(self.display().to_string(), self)
    }
}

impl LayerSource for PathBuf {
    fn load(self, config: &CrolConfig) -> Result<CrolLayer, Error> {
        self.as_path().load(config)
    }
}

impl LayerSource for &PathBuf {
    fn load(self, config: &CrolConfig) -> Result<CrolLayer, Error> {
        self.as_path().load(config)
    }
}

impl LayerSource for CrolLayer {
    fn load(self, _: &CrolConfig) -> Result<CrolLayer, Error> {
        Ok(self)
    }
}

/// Loads layers and composes them into Rust values (`SPEC.md` 10.7).
///
/// ```
/// use crol::{CrolComposer, CrolLayer};
///
/// let base = CrolLayer::from_sources("base", [("web.crol", r#"
///     +port: 80,
///     +url: "http://web:" + &root.port,
/// "#)]);
/// let local = CrolLayer::from_sources("local", [("web.crol", "port: & + 1")]);
///
/// let data = CrolComposer::new().load_layer(base).load_layer(local).compose_data().unwrap();
/// let url: String = data.get("url").unwrap();
/// assert_eq!(url, "http://web:81");
/// ```
#[derive(Debug, Clone)]
pub struct CrolComposer {
    config: CrolConfig,
    layers: Vec<CrolLayer>,
    /// Canonical paths of documents loaded, for `load_extends`.
    loaded: HashSet<PathBuf>,
    /// An error from `load_layer`, reported by `compose`.
    error: Option<Error>,
}

impl Default for CrolComposer {
    fn default() -> Self {
        Self::new()
    }
}

impl CrolComposer {
    /// A composer with the default configuration.
    pub fn new() -> Self {
        Self::with_cfg(CrolConfig::default())
    }

    /// A composer with `config`.
    pub fn with_cfg(config: CrolConfig) -> Self {
        Self {
            config,
            layers: Vec::new(),
            loaded: HashSet::new(),
            error: None,
        }
    }

    /// Adds a layer on top of those loaded so far (`SPEC.md` 7.1). It fails
    /// only when the source names nothing or cannot be read; every other
    /// error comes from [`compose`](Self::compose).
    pub fn load(mut self, source: impl LayerSource) -> Result<Self, Error> {
        let layer = source.load(&self.config)?;
        self.add(layer, &mut Vec::new())?;
        Ok(self)
    }

    /// Adds a layer built in code. In-memory documents have no directory,
    /// so their relative module paths start from the root directory.
    pub fn load_layer(mut self, layer: CrolLayer) -> Self {
        if let Err(error) = self.add(layer, &mut Vec::new()) {
            self.error.get_or_insert(error);
        }
        self
    }

    /// Adds `layer`, after the layers it extends if `load_extends` is set.
    /// `in_progress` holds the documents whose extends are being loaded, to
    /// find cycles.
    fn add(&mut self, layer: CrolLayer, in_progress: &mut Vec<PathBuf>) -> Result<(), Error> {
        if self.config.load_extends {
            for document in &layer.documents {
                for module_path in extends_of(&document.text) {
                    let here = document.directory.as_deref();
                    let path = module_path::resolve(&module_path, &self.config.root_path, here)?;
                    let canonical = path.canonicalize().unwrap_or(path.clone());
                    if in_progress.contains(&canonical)
                        || document.canonical.as_ref() == Some(&canonical)
                    {
                        return Err(Error::new(format!(
                            "{}: `extends` forms a cycle through {}",
                            document.display,
                            path.display()
                        )));
                    }
                    if self.loaded.contains(&canonical) {
                        continue;
                    }
                    let extended = CrolLayer::from_path(module_path.clone(), &path)?;
                    if let Some(canonical) = &document.canonical {
                        in_progress.push(canonical.clone());
                    }
                    in_progress.push(canonical.clone());
                    self.add(extended, in_progress)?;
                    in_progress.pop();
                    if document.canonical.is_some() {
                        in_progress.pop();
                    }
                }
            }
        }
        for document in &layer.documents {
            if let Some(canonical) = &document.canonical {
                self.loaded.insert(canonical.clone());
            }
        }
        self.layers.push(layer);
        Ok(())
    }

    /// The layers loaded so far, lowest first, including those loaded for
    /// `extends`.
    pub fn layers(&self) -> &[CrolLayer] {
        &self.layers
    }

    /// Evaluates every key and deserializes the root as `T`: a struct whose
    /// fields are the root keys, or a map such as `BTreeMap<String,
    /// Definition>`. The error carries every diagnostic; warnings are
    /// dropped (`compose_data` keeps them).
    pub fn compose<T: for<'de> Deserialize<'de>>(self) -> Result<T, Error> {
        self.compose_data()?.deserialize_root()
    }

    /// Evaluates every key and returns the data, for reading several
    /// values and the warnings.
    pub fn compose_data(self) -> Result<CrolData, Error> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let mut sources = Sources::new();
        let mut layers = Vec::with_capacity(self.layers.len());
        for layer in &self.layers {
            let documents = layer
                .documents
                .iter()
                .map(|d| {
                    (
                        d.relative.clone(),
                        sources.add(d.display.clone(), d.text.as_str()),
                    )
                })
                .collect();
            layers.push(Layer {
                name: layer.name.clone(),
                documents,
            });
        }
        let options = DataOptions::default()
            .with_warn_cross_file_patches(self.config.warn_cross_file_patches);
        let evaluation = evaluate_data(&sources, &layers, &options);
        match evaluation.value {
            Some(root) => Ok(CrolData {
                sources,
                root,
                diagnostics: evaluation.diagnostics,
                options: self.config.deserialize_options(),
            }),
            None => Err(Error::from_diagnostics(&sources, &evaluation.diagnostics)),
        }
    }
}

/// The module paths a document's `#![extends(...)]` attributes name.
fn extends_of(text: &str) -> Vec<String> {
    let parse = crol_syntax::parse(text);
    parse
        .document()
        .attributes()
        .filter_map(|attribute| match attribute.kind() {
            Some(ast::AttributeKind::Extends(extends)) => extends.module_path(),
            _ => None,
        })
        .map(|path: ast::ModulePath| {
            path.syntax()
                .text()
                .to_string()
                .split_whitespace()
                .collect()
        })
        .collect()
}

/// Evaluated data (`SPEC.md` 10.7): the root keys and their values, and the
/// warnings found.
#[derive(Debug, Clone)]
pub struct CrolData {
    sources: Sources,
    root: Value,
    diagnostics: Vec<Diagnostic>,
    options: de::Options,
}

impl CrolData {
    fn root(&self) -> &crol_eval::Keyed {
        match &self.root.kind {
            ValueKind::Keyed(keyed) => keyed,
            _ => unreachable!("the root is a keyed value"),
        }
    }

    /// The warnings, as `file:line:col: message`.
    pub fn warnings(&self) -> Vec<String> {
        self.diagnostics
            .iter()
            .filter(|d| !d.is_error())
            .map(|d| d.display(&self.sources).to_string())
            .collect()
    }

    /// Every diagnostic, with [`sources`](Self::sources) for their files.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// The files the data was read from.
    pub fn sources(&self) -> &Sources {
        &self.sources
    }

    /// The evaluated value of a root key.
    pub fn value(&self, key: &str) -> Option<&Value> {
        self.root().entries.get(&Key::Str(key.to_owned()))
    }

    /// The root, as a keyed value from root keys to their values.
    pub fn root_value(&self) -> &Value {
        &self.root
    }

    /// The root keys, in root order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.root().entries.keys().map(|k| match k {
            Key::Str(s) => s.as_str(),
            Key::Int(_) => unreachable!("root keys are strings"),
        })
    }

    /// Whether a root key exists, for checking ids (`SPEC.md` 10.5).
    pub fn contains(&self, id: &str) -> bool {
        self.value(id).is_some()
    }

    /// The type name of a root key's value, for checking ids.
    pub fn type_name(&self, id: &str) -> Option<&str> {
        match &self.value(id)?.kind {
            ValueKind::Keyed(keyed) => keyed.name.as_deref(),
            ValueKind::NamedTuple(name, _) | ValueKind::UnitName(name) => Some(name),
            _ => None,
        }
    }

    /// Deserializes a root key's value.
    pub fn get<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Result<T, Error> {
        self.get_with_ids(key).map(|(value, _)| value)
    }

    /// Deserializes a root key's value, and returns the ids it uses.
    pub fn get_with_ids<T: for<'de> Deserialize<'de>>(
        &self,
        key: &str,
    ) -> Result<(T, Vec<IdUse>), Error> {
        let value = self
            .value(key)
            .ok_or_else(|| Error::new(format!("there is no root key `{key}`")))?;
        crate::deserialize(value, &self.sources, self.options.clone(), Some(key))
    }

    /// Deserializes the whole root as `T`.
    pub fn deserialize_root<T: for<'de> Deserialize<'de>>(&self) -> Result<T, Error> {
        crate::deserialize(&self.root, &self.sources, self.options.clone(), None)
            .map(|(value, _)| value)
    }
}
