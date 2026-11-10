//! The composer: layers in, Rust values out (`SPEC.md` 7, 10.7).

use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crol_eval::{
    Compiled, Context, ContextData, DataOptions, Diagnostic, ExplainStep, Key, Layer, Sources,
    Value, ValueKind, VariantKeys, Variants, evaluate_compiled, evaluate_contexts,
};
use crol_syntax::TextRange;
use crol_syntax::ast::{self, AstNode};
use serde::Deserialize;

use crate::de;
use crate::error::Error;
use crate::id::IdUse;
use crate::loader::{self, FileLoader, FileLoaders, FsLoader, Loaded, Origin, SourceId};
use crate::mode::Mode;
use crate::resolve::{DataPath, Resolver};

/// A variant name type: maps values to the names data writes in selectors,
/// and back (`SPEC.md` 8.1). Implemented for `String`, which accepts every
/// identifier; an application may use its own enum, so that the compiler
/// checks the variants it passes.
pub trait VariantName: Clone + Eq {
    /// The name data writes: `prod_eu`.
    fn name(&self) -> &str;
    /// The value for a name, if the name is one; a name it rejects is an
    /// unknown variant.
    fn from_name(name: &str) -> Option<Self>;
}

impl VariantName for String {
    fn name(&self) -> &str {
        self
    }

    fn from_name(name: &str) -> Option<Self> {
        Some(name.to_owned())
    }
}

/// A variant declaration (`SPEC.md` 8.1).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Variant<T = String> {
    /// A flag: on or off, independent of the rest.
    Discrete(T),
    /// Alternatives: at most one of them is active.
    Grouped(Vec<T>),
    /// Alternatives that data declares, with `#![variant(group: name)]`:
    /// the group's name. Only for variants named by strings.
    Open(String),
}

/// The variants an application declares (`SPEC.md` 8.1). Data adds
/// variants only to the groups the application opens.
///
/// ```
/// let variants = crol::CrolVariants::new()
///     .add_grouped(["dev", "prod", "prod_eu"])
///     .add_discrete("cache")
///     .add_parent("prod_eu", "prod");
/// ```
///
/// With a typed variant name, start from `CrolVariants::<Env>::default()`.
/// Declaration errors (a name declared twice, a parent cycle) are reported
/// when composing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrolVariants<T = String> {
    declarations: Vec<Variant<T>>,
    parents: Vec<(T, T)>,
}

impl<T> Default for CrolVariants<T> {
    fn default() -> Self {
        Self {
            declarations: Vec::new(),
            parents: Vec::new(),
        }
    }
}

impl CrolVariants<String> {
    /// No variants, named by strings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens a group of alternatives, at first empty, to which data adds
    /// with `#![variant(group: name)]`: for alternatives that only the data
    /// knows, such as the factions of a game that mods add to.
    pub fn add_open_group(mut self, group: impl Into<String>) -> Self {
        self.declarations.push(Variant::Open(group.into()));
        self
    }
}

impl<T: VariantName> CrolVariants<T> {
    /// Declares a flag.
    pub fn add_discrete(mut self, variant: impl Into<T>) -> Self {
        self.declarations.push(Variant::Discrete(variant.into()));
        self
    }

    /// Declares alternatives, of which at most one is active.
    pub fn add_grouped<V: Into<T>>(mut self, variants: impl IntoIterator<Item = V>) -> Self {
        self.declarations.push(Variant::Grouped(
            variants.into_iter().map(Into::into).collect(),
        ));
        self
    }

    /// Gives a variant a parent: the child being active makes the parent
    /// active too.
    pub fn add_parent(mut self, child: impl Into<T>, parent: impl Into<T>) -> Self {
        self.parents.push((child.into(), parent.into()));
        self
    }

    /// Whether no variant is declared.
    pub fn is_empty(&self) -> bool {
        self.declarations.is_empty()
    }

    pub(crate) fn build(&self) -> Result<Variants, Error> {
        let mut variants = Variants::new();
        let composer = |message: String| Error::new(format!("composer: {message}"));
        for declaration in &self.declarations {
            match declaration {
                Variant::Discrete(v) => variants.add_discrete(v.name()).map_err(composer)?,
                Variant::Grouped(vs) => variants
                    .add_grouped(vs.iter().map(VariantName::name))
                    .map_err(composer)?,
                Variant::Open(group) => variants.add_open_group(group).map_err(composer)?,
            }
        }
        for (child, parent) in &self.parents {
            variants
                .add_parent(child.name(), parent.name())
                .map_err(composer)?;
        }
        Ok(variants)
    }
}

/// How a [`CrolComposer`] loads and composes (`SPEC.md` 10.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrolConfig<T = String> {
    /// The variants (`SPEC.md` 8.1). Default: none declared, so every name
    /// the data uses is a `Discrete` variant.
    pub variants: CrolVariants<T>,
    /// Where `::` module paths start (`SPEC.md` 4.2). Default: the working
    /// directory.
    pub root_path: PathBuf,
    /// Where documents come from (`SPEC.md` 10.7). Default: the file system
    /// below `root_path`.
    pub file_loaders: FileLoaders,
    /// Follow `#![extends(...)]`: load the extended documents as layers
    /// below the one that extends them (`SPEC.md` 4.1). Default: `false`.
    pub load_extends: bool,
    /// The contexts `compose_data` evaluates, each by its chosen variants.
    /// Default: `None`, every valid context.
    pub contexts: Option<Vec<Vec<T>>>,
    /// With no `contexts`, evaluate the base and one context for each variant
    /// rather than every combination (`SPEC.md` 8.1).
    pub each_variant: bool,
    /// The most contexts evaluated without `contexts`. Default: 4096.
    pub max_contexts: usize,
    /// How many threads evaluate the contexts after the base. `None` decides
    /// from the machine and the size of the data, and `Some(1)` uses none.
    /// The result is the same either way; more threads take more memory.
    pub threads: Option<usize>,
    /// Which keys a context may have, compared with the base (`SPEC.md`
    /// 8.5). Default: `Any`.
    pub variant_keys: VariantKeys,
    /// Accept any value where an `Option` is expected, as `Some`.
    /// Default: `true`.
    pub implicit_some: bool,
    /// Ignore keys a struct does not have, instead of an error.
    /// Default: `false`.
    pub allow_unknown_fields: bool,
    /// Require every field of a struct unless it ends with `..`. A type's
    /// `#[crol(explicit_defaults)]` setting, seen through
    /// [`CrolComposer::schema`], comes first (`SPEC.md` 10.3).
    /// Default: `true`.
    pub require_complete: bool,
    /// Let a path go through a field left out, from its default. Needs a
    /// [schema](CrolComposer::schema) (`SPEC.md` 5.1).
    pub init_defaults: bool,
    /// Keep what layers lower to, so that [`CrolData::reload`] need not parse
    /// the ones that did not change: what `.crob` layers hold beyond their
    /// text, and from the first reload, what a text layer that stands alone
    /// lowers to (`SPEC.md` 7.8). Without it, data takes less memory. Default:
    /// `true`.
    pub keep_lowered: bool,
    /// Make an id that names nothing an error instead of a warning
    /// (`SPEC.md` 10.5). Default: `false`, since ids may name things outside
    /// the data.
    pub strict_ids: bool,
}

impl<T> Default for CrolConfig<T> {
    fn default() -> Self {
        Self {
            variants: CrolVariants::default(),
            root_path: std::env::current_dir().unwrap_or_default(),
            file_loaders: FileLoaders::new(),
            load_extends: false,
            contexts: None,
            each_variant: false,
            max_contexts: 4096,
            threads: None,
            variant_keys: VariantKeys::Any,
            implicit_some: true,
            allow_unknown_fields: false,
            require_complete: true,
            init_defaults: false,
            keep_lowered: true,
            strict_ids: false,
        }
    }
}

impl<T> CrolConfig<T> {
    fn deserialize_options(&self) -> de::Options {
        de::Options {
            implicit_some: self.implicit_some,
            allow_unknown_fields: self.allow_unknown_fields,
            require_complete: self.require_complete,
            structs: None,
        }
    }
}

/// A layer: a named, ordered set of documents (`SPEC.md` 7.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrolLayer {
    name: String,
    /// Each document's path relative to the layer, its name for messages,
    /// its text, and where it came from, for its module paths.
    documents: Vec<Document>,
    /// What lowering them gave, for a layer read from a `.crob` file
    /// (`SPEC.md` 7.8).
    lowered: Option<Arc<[u8]>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Document {
    relative: String,
    display: String,
    text: String,
    /// `None` for documents built in code.
    source: Option<SourceId>,
    /// The document's directory, as names from the root; `None` for a
    /// document that was not reached from the root, loaded by a file
    /// system path.
    directory: Option<Vec<String>>,
}

impl Document {
    fn origin(&self) -> Origin<'_> {
        Origin {
            from: self.source.as_ref(),
            directory: self.directory.as_deref(),
        }
    }

    fn read(
        relative: String,
        id: &SourceId,
        bytes: Vec<u8>,
        directory: Option<Vec<String>>,
    ) -> Result<Self, Error> {
        Ok(Self {
            relative,
            display: id.to_string(),
            text: loader::text(id, bytes)?,
            source: Some(id.clone()),
            directory,
        })
    }
}

impl CrolLayer {
    /// A layer of in-memory documents, for tests, editors and embedded data.
    /// Each document is a path relative to the layer and its text. Text is
    /// data mode whatever its path says: the call states the mode
    /// (`SPEC.md` 4).
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
                    source: None,
                    directory: Some(Vec::new()),
                }
            })
            .collect();
        Self {
            name,
            documents,
            lowered: None,
        }
    }

    /// A layer of one file, or of every `.crod` file below a directory
    /// (`SPEC.md` 7.1). Hidden files and directories are skipped. A `.crol`
    /// file, given or in the directory, is an error (`SPEC.md` 4).
    pub fn from_path(name: impl Into<String>, path: &Path) -> Result<Self, Error> {
        Self::from_path_except(name.into(), path, None)
    }

    /// [`from_path`](Self::from_path), for a directory that may hold the
    /// project file.
    fn from_path_except(
        name: String,
        path: &Path,
        project_file: Option<&SourceId>,
    ) -> Result<Self, Error> {
        let relative = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let loader = FsLoader::new(path);
        let id = SourceId::from_path(path);
        Self::read(name, &loader, &id, relative, None, project_file)
    }

    /// The layer a source holds. `file_name` is its relative path if it is
    /// a document, and `target` its names from the root, if known. A
    /// directory's documents leave out `project_file` (`SPEC.md` 12.4).
    pub(crate) fn read(
        name: String,
        loader: &dyn FileLoader,
        id: &SourceId,
        file_name: String,
        target: Option<&[String]>,
        project_file: Option<&SourceId>,
    ) -> Result<Self, Error> {
        let documents = match loader::load(loader, id)? {
            Loaded::Document(bytes) if is_binary(&id.to_string()) => {
                return Self::from_binary_at(name, &bytes, id);
            }
            Loaded::Document(bytes) => {
                loader::check_mode(id, Mode::Data)?;
                let directory = target.map(|t| t[..t.len().saturating_sub(1)].to_vec());
                vec![Document::read(file_name, id, bytes, directory)?]
            }
            Loaded::Directory(mut entries) => {
                // By path, whatever order the loader found them in
                // (`SPEC.md` 7.1).
                entries.sort_by(|(a, _), (b, _)| a.cmp(b));
                let mut documents = Vec::with_capacity(entries.len());
                for (relative, entry) in entries {
                    if project_file == Some(&entry) {
                        continue;
                    }
                    // A directory never hides a document it holds, so that
                    // one of another mode cannot be lost without a word.
                    loader::check_mode(&entry, Mode::Data)?;
                    if is_binary(&relative) {
                        return Err(Error::new(format!(
                            "{entry}: a `.crob` file is a layer of its own, not a document of one; move it out of {id}"
                        ))
                        .with_source_id(entry));
                    }
                    let mut components: Vec<&str> = relative.split('/').collect();
                    if components.iter().any(|c| matches!(*c, "" | "." | "..")) {
                        return Err(Error::new(format!(
                            "{id}: `{relative}` is not a relative path below it"
                        ))
                        .with_source_id(id.clone()));
                    }
                    components.pop();
                    let directory = target.map(|t| {
                        let components = components.iter().map(|c| (*c).to_owned());
                        t.iter().cloned().chain(components).collect()
                    });
                    let Loaded::Document(bytes) = loader::load(loader, &entry)? else {
                        return Err(Error::new(format!(
                            "{id}: {entry} is listed as a document, and is a directory"
                        ))
                        .with_source_id(entry));
                    };
                    documents.push(Document::read(relative, &entry, bytes, directory)?);
                }
                documents
            }
        };
        Ok(Self {
            name,
            documents,
            lowered: None,
        })
    }

    /// A layer from the bytes of a `.crob` file (`SPEC.md` 7.8).
    pub fn from_binary(name: impl Into<String>, bytes: &[u8]) -> Result<Self, Error> {
        let name = name.into();
        let id = SourceId::new(format!("{name}.crob"));
        Self::from_binary_at(name, bytes, &id)
    }

    fn from_binary_at(name: String, bytes: &[u8], id: &SourceId) -> Result<Self, Error> {
        let in_file =
            |message: String| Error::new(format!("{id}: {message}")).with_source_id(id.clone());
        let layer = crol_eval::binary::read_layer(bytes).map_err(in_file)?;
        let documents = layer
            .documents
            .into_iter()
            .map(|document| Document {
                display: format!("{id}:{}", document.relative),
                relative: document.relative,
                text: document.text,
                source: None,
                directory: None,
            })
            .collect();
        Ok(Self {
            name,
            documents,
            lowered: Some(layer.lowered),
        })
    }

    /// Whether `bytes` are a `.crob` file made from just these documents, by
    /// a version of CROL that reads it (`SPEC.md` 7.8). So a program can read
    /// a layer's text and use the file only while that holds, without
    /// compiling. Bytes that are no such file are not an error.
    pub fn compiled_in(&self, bytes: &[u8]) -> bool {
        let documents = self
            .documents
            .iter()
            .map(|d| (d.relative.as_str(), d.text.as_str()));
        crol_eval::binary::source_hash(bytes) == Some(crol_eval::binary::hash_documents(documents))
    }

    /// The bytes of the layer's `.crob` file (`SPEC.md` 7.8): for a layer
    /// that does not change, such as generated data, to load without
    /// parsing. It may not use a template of another layer.
    pub fn to_binary(&self) -> Result<Vec<u8>, Error> {
        let mut sources = Sources::new();
        let documents = self
            .documents
            .iter()
            .map(|d| {
                let file = sources.add(d.relative.clone(), d.text.as_str());
                (d.relative.clone(), file)
            })
            .collect();
        let layer = Layer {
            name: self.name.clone(),
            documents,
        };
        crol_eval::encode_layer(&sources, &layer)
            .map_err(|found| Error::from_diagnostics(&sources, &found))
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
    /// messages (its [`SourceId`], or `layer/path` for documents built in
    /// code), and its text.
    pub fn documents(&self) -> impl Iterator<Item = (&str, &str, &str)> {
        self.documents
            .iter()
            .map(|d| (d.relative.as_str(), d.display.as_str(), d.text.as_str()))
    }
}

/// What became of one `#![extends(...)]`, from
/// [`CrolComposer::load_layer_tolerant`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtendsLoad {
    /// The document that has the attribute, by its name for messages, as
    /// [`CrolLayer::documents`] gives it.
    pub document: String,
    /// The module path as written.
    pub path: String,
    /// Where the module path is written in the document.
    pub range: TextRange,
    /// The layers it added, as indices into
    /// [`layers`](CrolComposer::layers), lowest first. Empty when it names
    /// what is already loaded, and for those it could not load.
    pub layers: Range<usize>,
    /// Why it, or an `extends` further down, could not be followed.
    pub error: Option<Error>,
}

/// Where a layer comes from: a module path (`&str`, `String`) or a file
/// system path (`&Path`, `PathBuf`).
pub trait LayerSource {
    /// Loads the layer.
    #[doc(hidden)]
    fn load<T>(self, config: &CrolConfig<T>) -> Result<CrolLayer, Error>;
}

impl LayerSource for &str {
    fn load<T>(self, config: &CrolConfig<T>) -> Result<CrolLayer, Error> {
        config
            .file_loaders
            .resolve(self, &config.root_path)?
            .load(self)
    }
}

impl LayerSource for String {
    fn load<T>(self, config: &CrolConfig<T>) -> Result<CrolLayer, Error> {
        self.as_str().load(config)
    }
}

impl LayerSource for &Path {
    fn load<T>(self, config: &CrolConfig<T>) -> Result<CrolLayer, Error> {
        if !config.file_loaders.file_system() {
            return Err(Error::new(format!(
                "cannot read {}: the file system is turned off",
                self.display()
            )));
        }
        if !self.exists() {
            return Err(Error::new(format!("{} does not exist", self.display())));
        }
        let project_file = config.file_loaders.project_file();
        CrolLayer::from_path_except(self.display().to_string(), self, project_file)
    }
}

impl LayerSource for PathBuf {
    fn load<T>(self, config: &CrolConfig<T>) -> Result<CrolLayer, Error> {
        self.as_path().load(config)
    }
}

impl LayerSource for &PathBuf {
    fn load<T>(self, config: &CrolConfig<T>) -> Result<CrolLayer, Error> {
        self.as_path().load(config)
    }
}

impl LayerSource for CrolLayer {
    fn load<T>(self, _: &CrolConfig<T>) -> Result<CrolLayer, Error> {
        Ok(self)
    }
}

/// Loads layers and composes them into Rust values (`SPEC.md` 10.7).
///
/// ```
/// use crol::{CrolComposer, CrolLayer};
///
/// let base = CrolLayer::from_sources("base", [("web.crod", r#"
///     +port: 80,
///     +url: "http://web:" + &root.port,
/// "#)]);
/// let local = CrolLayer::from_sources("local", [("web.crod", "port: & + 1")]);
///
/// let data = CrolComposer::new().load_layer(base).load_layer(local).compose_data().unwrap();
/// let url: String = data.get("url").unwrap();
/// assert_eq!(url, "http://web:81");
/// ```
#[derive(Debug, Clone)]
pub struct CrolComposer<T = String> {
    config: CrolConfig<T>,
    layers: Vec<CrolLayer>,
    /// The sources of the documents loaded, and of the directories `extends`
    /// loaded, for `load_extends`.
    loaded: HashSet<SourceId>,
    /// An error from `load_layer` or `with_variant`, reported by `compose`.
    error: Option<Error>,
    /// The schema, for `&default`, constraints and sealed fields.
    schema: Option<crate::schema::SchemaFile>,
    /// The schema's aliases and settings, for completeness.
    structs: Option<std::sync::Arc<de::Structs>>,
    /// The variants made active with `with_variant`.
    chosen: Vec<T>,
    /// The document `load_active` named.
    active: Option<crate::db::Active>,
}

impl Default for CrolComposer<String> {
    fn default() -> Self {
        Self::new()
    }
}

impl CrolComposer<String> {
    /// A composer with the default configuration.
    pub fn new() -> Self {
        Self::with_cfg(CrolConfig::default())
    }
}

impl<T: VariantName> CrolComposer<T> {
    /// A composer with `config`.
    pub fn with_cfg(config: CrolConfig<T>) -> Self {
        Self {
            config,
            layers: Vec::new(),
            loaded: HashSet::new(),
            error: None,
            chosen: Vec::new(),
            active: None,
            schema: None,
            structs: None,
        }
    }

    /// Registers a file loader (`SPEC.md` 10.7), asked after those already
    /// registered and before the default [`FsLoader`], for what is loaded
    /// from now on.
    pub fn with_file_loader(mut self, loader: impl FileLoader + 'static) -> Self {
        self.config.file_loaders = self.config.file_loaders.with(loader);
        self
    }

    /// Turns off the default [`FsLoader`]: see
    /// [`FileLoaders::without_file_system`].
    pub fn without_file_system(mut self) -> Self {
        self.config.file_loaders = self.config.file_loaders.without_file_system();
        self
    }

    /// Adds a layer on top of those loaded so far (`SPEC.md` 7.1). It fails
    /// only when the source names nothing or cannot be read; every other
    /// error comes from [`compose`](Self::compose).
    pub fn load(mut self, source: impl LayerSource) -> Result<Self, Error> {
        let layer = source.load(&self.config)?;
        self.add(layer, &mut Vec::new())?;
        Ok(self)
    }

    /// Adds the active layer of a [`CrolDb`](crate::CrolDb) (`SPEC.md` 10.14):
    /// a `.crod` file the database owns, as the last layer. A file that does
    /// not exist is an empty layer, so a program's first run needs nothing
    /// else. A `load` after it is an error.
    pub fn load_active(mut self, file: impl AsRef<Path>) -> Result<Self, Error> {
        let file = file.as_ref();
        if !self.config.file_loaders.file_system() {
            return Err(Error::new(format!(
                "cannot read {}: the file system is turned off",
                file.display()
            )));
        }
        crate::db::check_document(file)?;
        let name = file.display().to_string();
        let read = crate::db::read_document(file)?;
        let text = read.clone().unwrap_or_default();
        let log = crate::db::Log::parse(&name, &text)?;
        let document = Document {
            relative: file
                .file_name()
                .map_or_else(|| name.clone(), |n| n.to_string_lossy().into_owned()),
            display: name.clone(),
            text,
            source: None,
            directory: None,
        };
        let active = crate::db::Active {
            layer: name.clone(),
            document: document.relative.clone(),
            file: Some(file.to_owned()),
            read,
            log,
        };
        self.add(
            CrolLayer {
                name,
                documents: vec![document],
                lowered: None,
            },
            &mut Vec::new(),
        )?;
        self.active = Some(active);
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

    /// Adds a layer as [`load_layer`](Self::load_layer) does, going on past
    /// an `extends` that fails, for tools that show what they can: the
    /// layer goes in on top of what the other `extends` loaded, and the
    /// result says what became of each. `load_layer` keeps the first error
    /// for `compose`, and this one reports none there.
    pub fn load_layer_tolerant(&mut self, layer: CrolLayer) -> Vec<ExtendsLoad> {
        let mut loads = Vec::new();
        if self.config.load_extends {
            let mut documents: Vec<&Document> = layer.documents.iter().collect();
            documents.sort_by(|a, b| a.relative.cmp(&b.relative));
            for document in documents {
                for (path, range) in extends_attributes(&document.text) {
                    // What is in the layer is put in order there, not loaded.
                    if names_in_layer(&layer, document, &path) {
                        continue;
                    }
                    let start = self.layers.len();
                    // The document with this `extends` alone, so that `add`
                    // follows it as it follows any other, and its failure
                    // leaves the rest.
                    let alone = CrolLayer {
                        name: layer.name.clone(),
                        documents: vec![Document {
                            text: format!("#![extends({path})]\n"),
                            ..document.clone()
                        }],
                        lowered: None,
                    };
                    let error = self.add(alone, &mut Vec::new()).err();
                    if error.is_none() {
                        self.layers.pop();
                    }
                    if let Some(source) = &document.source {
                        self.loaded.remove(source);
                    }
                    let end = if error.is_none() {
                        self.layers.len()
                    } else {
                        start
                    };
                    loads.push(ExtendsLoad {
                        document: document.display.clone(),
                        path,
                        range,
                        layers: start..end,
                        error,
                    });
                }
            }
        }
        let load_extends = std::mem::replace(&mut self.config.load_extends, false);
        let added = self.add(layer, &mut Vec::new());
        self.config.load_extends = load_extends;
        debug_assert!(added.is_ok(), "only `extends` can fail");
        loads
    }

    /// Gives the composer a schema, from which `&default` reads defaults
    /// (`SPEC.md` 6.1.3) and the deserializer learns which names are
    /// aliases of one field (10.2) and which types set
    /// `#[crol(explicit_defaults)]` (10.3), which serde does not tell it:
    /// `.schema(crol::schema::build::<Definition>())`.
    pub fn schema(mut self, schema: crate::schema::SchemaFile) -> Self {
        self.structs = Some(std::sync::Arc::new(de::Structs::from_schema(&schema)));
        self.schema = Some(schema);
        self
    }

    /// Makes a variant active in the context to compose (`SPEC.md` 8.1).
    /// Fails for a variant that is not declared (when variants are
    /// declared and no group is open), or a second choice in a group that is
    /// not an ancestor or descendant of the first.
    pub fn with_variant(mut self, variant: impl Into<T>) -> Result<Self, Error> {
        let variant = variant.into();
        let declared = self.config.variants.build()?;
        if !self.config.variants.is_empty() {
            // A variant the data may declare in an open group is checked when
            // composing.
            if declared.id(variant.name()).is_none() && declared.open_groups().next().is_none() {
                return Err(Error::new(format!(
                    "composer: `{}` is not a declared variant",
                    variant.name()
                )));
            }
            let mut ids: Vec<_> = self
                .chosen
                .iter()
                .filter_map(|v| declared.id(v.name()))
                .collect();
            ids.extend(declared.id(variant.name()));
            declared
                .context(&ids)
                .map_err(|message| Error::new(format!("composer: {message}")))?;
        }
        self.chosen.push(variant);
        Ok(self)
    }

    /// Adds `layer`, after the layers it extends if `load_extends` is set.
    /// A layer loaded for an `extends` leaves out the documents already
    /// loaded, which a directory may share with what came before it
    /// (`SPEC.md` 4.1), and is not added if that is all of them; one the
    /// application names stays whole. `in_progress` holds the sources whose
    /// extends are being loaded, to find cycles, and is empty for the latter.
    fn add(&mut self, mut layer: CrolLayer, in_progress: &mut Vec<SourceId>) -> Result<(), Error> {
        if let Some(active) = &self.active {
            return Err(Error::new(format!(
                "composer: the active layer `{}` is the last layer",
                active.layer
            )));
        }
        let for_extends = !in_progress.is_empty();
        if self.config.load_extends {
            // In document order: by path (`SPEC.md` 7.1).
            let mut documents: Vec<&Document> = layer.documents.iter().collect();
            documents.sort_by(|a, b| a.relative.cmp(&b.relative));
            for document in documents {
                if self.is_loaded(document) {
                    continue;
                }
                for module_path in extends_of(&document.text) {
                    // What is in the layer is put in order there (`SPEC.md`
                    // 7.1), not loaded below it.
                    if names_in_layer(&layer, document, &module_path) {
                        continue;
                    }
                    let resolved = self.config.file_loaders.resolve_from(
                        &module_path,
                        &self.config.root_path,
                        document.origin(),
                        Mode::Data,
                    )?;
                    let id = resolved.id().clone();
                    if in_progress.contains(&id) || document.source.as_ref() == Some(&id) {
                        return Err(Error::new(format!(
                            "{}: `extends` forms a cycle through {id}",
                            document.display
                        )));
                    }
                    if self.loaded.contains(&id) {
                        continue;
                    }
                    let extended = resolved.load(module_path.clone())?;
                    if let Some(source) = &document.source {
                        in_progress.push(source.clone());
                    }
                    in_progress.push(id.clone());
                    self.add(extended, in_progress)?;
                    in_progress.pop();
                    if document.source.is_some() {
                        in_progress.pop();
                    }
                    // A directory's own id is no document's.
                    self.loaded.insert(id);
                }
            }
        }
        if for_extends {
            let had_documents = !layer.documents.is_empty();
            let loaded = &self.loaded;
            layer
                .documents
                .retain(|d| !d.source.as_ref().is_some_and(|s| loaded.contains(s)));
            if had_documents && layer.documents.is_empty() {
                return Ok(());
            }
        }
        for document in &layer.documents {
            if let Some(source) = &document.source {
                self.loaded.insert(source.clone());
            }
        }
        self.layers.push(layer);
        Ok(())
    }

    fn is_loaded(&self, document: &Document) -> bool {
        document
            .source
            .as_ref()
            .is_some_and(|source| self.loaded.contains(source))
    }

    /// The layers loaded so far, lowest first, including those loaded for
    /// `extends`.
    pub fn layers(&self) -> &[CrolLayer] {
        &self.layers
    }

    /// Evaluates every key in the composed context (the variants given to
    /// `with_variant`) and deserializes the root as `T`: a struct whose
    /// fields are the root keys, or a map such as `BTreeMap<String,
    /// Definition>`. The error carries every diagnostic; warnings are
    /// dropped (`compose_data` keeps them).
    pub fn compose<R: for<'de> Deserialize<'de>>(self) -> Result<R, Error> {
        let chosen: Vec<String> = self.chosen.iter().map(|v| v.name().to_owned()).collect();
        self.evaluate(Some(vec![chosen]))?.deserialize_root(None)
    }

    /// Evaluates every key in every evaluated context (`SPEC.md` 8.1) and
    /// returns the data, for reading several values, other contexts, the
    /// warnings and the fingerprint.
    pub fn compose_data(self) -> Result<CrolData<T>, Error> {
        let contexts = self.config.contexts.as_ref().map(|contexts| {
            let mut names: Vec<Vec<String>> = contexts
                .iter()
                .map(|c| c.iter().map(|v| v.name().to_owned()).collect())
                .collect();
            names.push(self.chosen.iter().map(|v| v.name().to_owned()).collect());
            names
        });
        self.evaluate(contexts)
    }

    /// Evaluates as [`compose_data`](Self::compose_data) does, and returns a
    /// database with `R` as its root (`SPEC.md` 10.14). Without an active
    /// layer, transactions work, `save` is an error, and `save_as` starts one.
    pub fn compose_db<R: crate::Navigable>(mut self) -> Result<crate::CrolDb<R, T>, Error> {
        let active = match self.active.clone() {
            Some(active) => active,
            None => {
                let active = crate::db::Active::in_memory();
                let layer = CrolLayer::from_sources(
                    active.layer.clone(),
                    [(active.document.clone(), String::new())],
                );
                self.add(layer, &mut Vec::new())?;
                active
            }
        };
        let same_name = self.layers.iter().filter(|l| l.name == active.layer);
        if same_name.count() > 1 {
            return Err(Error::new(format!(
                "composer: a layer has the name of the active layer, `{}`",
                active.layer
            )));
        }
        Ok(crate::CrolDb::new(self.compose_data()?, active))
    }

    fn evaluate(mut self, contexts: Option<Vec<Vec<String>>>) -> Result<CrolData<T>, Error> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let variants = self.config.variants.build()?;
        let mut sources = Sources::new();
        let mut layers = Vec::with_capacity(self.layers.len());
        let mut lowered: Vec<Option<Arc<[u8]>>> = Vec::with_capacity(self.layers.len());
        // The texts move to `sources`, so that evaluation holds them once.
        for layer in std::mem::take(&mut self.layers) {
            let documents = layer
                .documents
                .into_iter()
                .map(|d| (d.relative, sources.add(d.display, d.text)))
                .collect();
            layers.push(Layer {
                name: layer.name,
                documents,
            });
            lowered.push(layer.lowered);
        }
        // A layer's `schema.crod` changes the schema (`SPEC.md` 10.12).
        let schema = match &self.schema {
            Some(schema) => {
                let (schema, found) = schema.patched(&mut sources, &layers);
                if found.iter().any(Diagnostic::is_error) {
                    return Err(Error::from_diagnostics(&sources, &found));
                }
                Some(schema.defaults())
            }
            None => {
                let patched = layers
                    .iter()
                    .flat_map(|layer| &layer.documents)
                    .any(|(path, _)| path == crol_eval::SCHEMA_DOCUMENT);
                if patched {
                    return Err(Error::new(format!(
                        "composer: a layer has a `{}`, which needs a schema: `CrolComposer::schema`",
                        crol_eval::SCHEMA_DOCUMENT
                    )));
                }
                None
            }
        };
        let options = DataOptions::default()
            .with_init_defaults(self.config.init_defaults)
            .with_variants(variants)
            .with_contexts(contexts)
            .with_each_variant(self.config.each_variant)
            .with_max_contexts(self.config.max_contexts)
            .with_threads(self.config.threads)
            .with_variant_keys(self.config.variant_keys)
            .with_defaults(schema);
        let (compiled, mut diagnostics) = compile_lowered(&sources, &layers, &lowered);
        let Some(compiled) = compiled else {
            return Err(Error::from_diagnostics(&sources, &diagnostics));
        };
        let keep_lowered = self.config.keep_lowered;
        if !keep_lowered {
            lowered.fill(None);
        }
        let lower_tried = vec![false; layers.len()];
        let mut evaluation = evaluate_compiled(&compiled, &options);
        diagnostics.append(&mut evaluation.diagnostics);
        evaluation.diagnostics = diagnostics;
        let Some(base) = evaluation.value.take() else {
            return Err(Error::from_diagnostics(&sources, &evaluation.diagnostics));
        };
        let chosen: Vec<_> = self
            .chosen
            .iter()
            .map(|v| {
                evaluation.variants.id(v.name()).ok_or_else(|| {
                    Error::new(format!("composer: the data uses no variant `{}`", v.name()))
                })
            })
            .collect::<Result<_, _>>()?;
        let composed = evaluation
            .variants
            .context(&chosen)
            .map_err(|message| Error::new(format!("composer: {message}")))?;
        let strict_ids = self.config.strict_ids;
        let mut diagnostics = evaluation.diagnostics;
        let ids = id_diagnostics(
            &base,
            &evaluation.contexts,
            &composed,
            self.schema.as_ref(),
            strict_ids,
        );
        if strict_ids && !ids.is_empty() {
            return Err(Error::from_diagnostics(&sources, &ids));
        }
        diagnostics.extend(ids);
        Ok(CrolData {
            sources,
            layers,
            compiled: Arc::new(compiled),
            lowered,
            lower_tried,
            keep_lowered,
            schema: self.schema.map(Arc::new),
            data_options: options,
            base,
            contexts: evaluation.contexts,
            variants: evaluation.variants,
            composed,
            diagnostics,
            options: de::Options {
                structs: self.structs.clone(),
                ..self.config.deserialize_options()
            },
            strict_ids,
            names: std::marker::PhantomData,
        })
    }
}

/// Whether a name is that of a `.crob` file.
fn is_binary(name: &str) -> bool {
    name.ends_with(".crob")
}

/// [`crol_eval::compile_binary`], for layers that may be from `.crob` files.
fn compile_lowered(
    sources: &Sources,
    layers: &[Layer],
    lowered: &[Option<Arc<[u8]>>],
) -> (Option<Compiled>, Vec<Diagnostic>) {
    let bytes: Vec<Option<&[u8]>> = lowered.iter().map(|l| l.as_deref()).collect();
    crol_eval::compile_binary(sources, layers, &bytes)
}

/// Whether an `extends` in `document` names documents of its own `layer`,
/// which it then puts in order rather than loads (`SPEC.md` 4.1).
fn names_in_layer(layer: &CrolLayer, document: &Document, module_path: &str) -> bool {
    let paths = layer.documents.iter().map(|d| d.relative.as_str());
    crol_eval::layer_target(paths, &document.relative, module_path).is_some()
}

/// The module paths a document's `#![extends(...)]` attributes name.
fn extends_of(text: &str) -> Vec<String> {
    extends_attributes(text)
        .into_iter()
        .map(|(path, _)| path)
        .collect()
}

/// The module paths a document's `#![extends(...)]` attributes name, each
/// with where it is written.
fn extends_attributes(text: &str) -> Vec<(String, TextRange)> {
    if !crate::schema::may_have_attributes(text) {
        return Vec::new();
    }
    let parse = crol_syntax::parse(text);
    parse
        .document()
        .attributes()
        .filter_map(|attribute| match attribute.kind() {
            Some(ast::AttributeKind::Extends(extends)) => extends.module_path(),
            _ => None,
        })
        .map(|path: ast::ModulePath| {
            let written = path.syntax().text().to_string();
            (
                written.split_whitespace().collect(),
                path.syntax().text_range(),
            )
        })
        .collect()
}

/// The root keys whose fingerprints differ between two data sets, or that
/// only one has, in byte order: from [`CrolData::key_fingerprints`].
pub fn changed_keys(
    before: &BTreeMap<String, [u8; 32]>,
    after: &BTreeMap<String, [u8; 32]>,
) -> Vec<KeyChange> {
    let mut changes = Vec::new();
    for (key, hash) in before {
        match after.get(key) {
            None => changes.push(KeyChange::Removed(key.clone())),
            Some(other) if other != hash => changes.push(KeyChange::Changed(key.clone())),
            Some(_) => {}
        }
    }
    for key in after.keys() {
        if !before.contains_key(key) {
            changes.push(KeyChange::Added(key.clone()));
        }
    }
    changes.sort_by(|a, b| a.key().as_bytes().cmp(b.key().as_bytes()));
    changes
}

/// How a root key differs between two data sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyChange {
    /// It is new.
    Added(String),
    /// It has another value, in at least one context.
    Changed(String),
    /// It is gone.
    Removed(String),
}

impl KeyChange {
    /// The root key.
    pub fn key(&self) -> &str {
        match self {
            KeyChange::Added(key) | KeyChange::Changed(key) | KeyChange::Removed(key) => key,
        }
    }
}

/// A change to one document, for [`CrolData::reload`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentChange<'a> {
    /// The layer's name.
    pub layer: &'a str,
    /// The document's path in the layer, as `/`-separated.
    pub path: &'a str,
    /// Its new text; `None` removes it. A path the layer does not have adds
    /// a document.
    pub text: Option<&'a str>,
}

/// What [`CrolData::reload`] changed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Reloaded {
    /// The root keys that differ now, in byte order.
    pub changed: Vec<KeyChange>,
    /// How many root keys were evaluated again, where only those whose
    /// operations changed and those that read them were (`SPEC.md` 7.9);
    /// `None` where everything was, and for a transaction's commit.
    pub evaluated: Option<usize>,
}

/// A context: a set of active variants (`SPEC.md` 8.1), from
/// [`CrolData::context`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CrolContext(Context);

impl CrolContext {
    pub(crate) fn new(context: Context) -> Self {
        Self(context)
    }

    pub(crate) fn base() -> Self {
        Self(Context::base())
    }

    /// Whether this is the base context, with no variant active.
    pub fn is_base(&self) -> bool {
        self.0.is_base()
    }
}

/// Evaluated data (`SPEC.md` 10.7): the root keys and their values in every
/// evaluated context, and the warnings found.
#[derive(Debug, Clone)]
pub struct CrolData<T = String> {
    sources: Sources,
    /// The layers and options, for re-evaluating in `explain`.
    layers: Vec<Layer>,
    /// For evaluating another context later.
    compiled: Arc<Compiled>,
    /// For each layer, what it lowers to, until it changes: what a `.crob`
    /// file holds, or what `lower_unchanged` found.
    lowered: Vec<Option<Arc<[u8]>>>,
    /// For each layer, whether `lower_unchanged` tried it since it changed.
    lower_tried: Vec<bool>,
    /// [`CrolConfig::keep_lowered`].
    keep_lowered: bool,
    /// The schema as given, for reloading a `schema.crod`.
    schema: Option<Arc<crate::schema::SchemaFile>>,
    data_options: DataOptions,
    base: Value,
    contexts: Vec<ContextData>,
    variants: Variants,
    /// The context `with_variant` chose, for `get` and `deserialize_root`.
    composed: Context,
    /// What evaluating found, and after it, what resolving the ids found.
    diagnostics: Vec<Diagnostic>,
    options: de::Options,
    /// [`CrolConfig::strict_ids`].
    strict_ids: bool,
    names: std::marker::PhantomData<T>,
}

/// Resolves every id of the composed context (`SPEC.md` 10.5): one that
/// names nothing is a warning, or with `strict` an error. These are found
/// after evaluating, and never reach the evaluator, so that they do not make
/// a reload evaluate everything (`SPEC.md` 7.9).
fn id_diagnostics(
    base: &Value,
    contexts: &[ContextData],
    composed: &Context,
    schema: Option<&crate::schema::SchemaFile>,
    strict: bool,
) -> Vec<Diagnostic> {
    let root = match contexts.iter().find(|data| data.context == *composed) {
        Some(data) if !composed.is_base() => Cow::Owned(data.root(base)),
        _ => Cow::Borrowed(base),
    };
    let resolver = Resolver::new(&root, schema);
    resolver
        .all()
        .into_iter()
        .filter(|resolved| resolved.target.is_none())
        .map(|resolved| {
            let span = crate::resolve::span_of(&resolved);
            let message = resolver.unresolved(&resolved.id, &resolved.location);
            if strict {
                Diagnostic::error(span, message)
            } else {
                Diagnostic::warning(span, message)
            }
        })
        .collect()
}

impl<T: VariantName> CrolData<T> {
    /// The changes of an evaluated context other than the base.
    fn changes_in(&self, context: &Context) -> Option<&ContextData> {
        self.contexts.iter().find(|data| data.context == *context)
    }

    fn is_evaluated(&self, context: &Context) -> bool {
        context.is_base() || self.changes_in(context).is_some()
    }

    /// A context's whole root, built from the base and its changes.
    fn root_in(&self, context: &Context) -> Option<Cow<'_, Value>> {
        if context.is_base() {
            return Some(Cow::Borrowed(&self.base));
        }
        self.changes_in(context)
            .map(|data| Cow::Owned(data.root(&self.base)))
    }

    fn composed_root(&self) -> Cow<'_, Value> {
        self.root_in(&self.composed)
            .unwrap_or(Cow::Borrowed(&self.base))
    }

    /// A root key's value in a context, without building the whole root.
    fn key_in(&self, key: &str, context: &Context) -> Option<&Value> {
        let key = Key::Str(key.into());
        match self.changes_in(context) {
            Some(data) => data.get(&self.base, &key),
            None if context.is_base() => Self::keyed(&self.base).entries.get(&key),
            None => None,
        }
    }

    fn keyed(value: &Value) -> &crol_eval::Keyed {
        match &value.kind {
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

    /// A context, by its variants; it must be valid and evaluated.
    pub fn context<V: Into<T>>(
        &self,
        variants: impl IntoIterator<Item = V>,
    ) -> Result<CrolContext, Error> {
        let context = self.context_of(variants)?;
        if !self.is_evaluated(&context) {
            return Err(Error::new(format!(
                "the context {} was not evaluated",
                self.variants.display(&context)
            )));
        }
        Ok(CrolContext(context))
    }

    /// Every evaluated context, the base first.
    pub fn contexts(&self) -> impl Iterator<Item = CrolContext> + '_ {
        std::iter::once(CrolContext(Context::base())).chain(
            self.contexts
                .iter()
                .map(|data| CrolContext(data.context.clone())),
        )
    }

    /// Evaluates a context of these variants, if it is not yet, without
    /// reading the layers again (`SPEC.md` 8.1). It then counts as evaluated
    /// everywhere, in the fingerprint as well. Its errors are returned, and
    /// the data stays as it was.
    pub fn evaluate<V: Into<T>>(
        &mut self,
        variants: impl IntoIterator<Item = V>,
    ) -> Result<CrolContext, Error> {
        let context = self.context_of(variants)?;
        if self.is_evaluated(&context) {
            return Ok(CrolContext(context));
        }
        let names = self
            .variants
            .chosen_names(&context)
            .into_iter()
            .map(str::to_owned)
            .collect();
        let options = self.data_options.clone().with_contexts(Some(vec![names]));
        let evaluation = evaluate_contexts(&self.compiled, &options, &self.base);
        if evaluation.has_errors() {
            return Err(Error::from_diagnostics(
                &self.sources,
                &evaluation.diagnostics,
            ));
        }
        let evaluated = evaluation
            .contexts
            .into_iter()
            .find(|data| data.context == context)
            .ok_or_else(|| Error::new("this context was not evaluated"))?;
        self.contexts.push(evaluated);
        Ok(CrolContext(context))
    }

    pub(crate) fn deserialize_options(&self) -> &de::Options {
        &self.options
    }

    pub(crate) fn defaults(&self) -> Option<&crol_eval::Defaults> {
        self.data_options.defaults.as_ref()
    }

    /// The context of these variants, evaluated or not.
    pub(crate) fn context_of<V: Into<T>>(
        &self,
        variants: impl IntoIterator<Item = V>,
    ) -> Result<Context, Error> {
        let mut ids = Vec::new();
        for variant in variants {
            let variant = variant.into();
            ids.push(
                self.variants
                    .id(variant.name())
                    .ok_or_else(|| Error::new(format!("`{}` is not a variant", variant.name())))?,
            );
        }
        self.variants.context(&ids).map_err(Error::new)
    }

    /// The names of a context's chosen variants.
    pub fn context_names(&self, context: &CrolContext) -> Vec<&str> {
        self.variants.chosen_names(&context.0)
    }

    /// The evaluated value of a root key in a context.
    pub fn value(&self, key: &str, context: &CrolContext) -> Option<&Value> {
        self.key_in(key, &context.0)
    }

    /// The root in the composed context, as a keyed value from root keys to
    /// their values.
    pub fn root_value(&self) -> Value {
        self.composed_root().into_owned()
    }

    /// The root keys in the composed context, in root order.
    pub fn keys(&self) -> Vec<String> {
        self.keys_in(&CrolContext(self.composed.clone()))
    }

    /// The root keys in a context, in root order.
    pub fn keys_in(&self, context: &CrolContext) -> Vec<String> {
        let Some(root) = self.root_in(&context.0) else {
            return Vec::new();
        };
        match &root.kind {
            ValueKind::Keyed(keyed) => keyed
                .entries
                .keys()
                .filter_map(|k| match k {
                    Key::Str(s) => Some(s.to_string()),
                    Key::Int(_) => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Whether something is at `path` in the composed context: what an id
    /// resolved to, [`IdUse::target`] (`SPEC.md` 10.5).
    pub fn contains_path(&self, path: &DataPath) -> bool {
        path.value_in(&self.composed_root()).is_some()
    }

    /// The type name of the value at `path` in the composed context, such as
    /// what an id resolved to.
    pub fn type_name_at(&self, path: &DataPath) -> Option<String> {
        let root = self.composed_root();
        Resolver::new(&root, None)
            .type_name_at(path)
            .map(str::to_owned)
    }

    /// Whether a root key exists in the composed context, for checking ids
    /// (`SPEC.md` 10.5).
    pub fn contains(&self, id: &str) -> bool {
        self.key_in(id, &self.composed).is_some()
    }

    /// The type name of a root key's value in the composed context.
    pub fn type_name(&self, id: &str) -> Option<&str> {
        match &self.key_in(id, &self.composed)?.kind {
            ValueKind::Keyed(keyed) => keyed.name.as_deref(),
            ValueKind::NamedTuple(name, _) | ValueKind::UnitName(name) => Some(name),
            _ => None,
        }
    }

    /// Deserializes a root key's value in the composed context.
    pub fn get<R: for<'de> Deserialize<'de>>(&self, key: &str) -> Result<R, Error> {
        self.get_with_ids(key).map(|(value, _)| value)
    }

    /// Deserializes a root key's value in a context.
    pub fn get_in<R: for<'de> Deserialize<'de>>(
        &self,
        key: &str,
        context: &CrolContext,
    ) -> Result<R, Error> {
        let value = self
            .key_in(key, &context.0)
            .ok_or_else(|| Error::new(format!("there is no root key `{key}` in this context")))?;
        crate::deserialize(value, &self.sources, self.options.clone(), Some(key)).map(|(v, _)| v)
    }

    /// Deserializes a root key's value in the composed context, and returns
    /// the ids it uses.
    pub fn get_with_ids<R: for<'de> Deserialize<'de>>(
        &self,
        key: &str,
    ) -> Result<(R, Vec<IdUse>), Error> {
        let value = self
            .key_in(key, &self.composed)
            .ok_or_else(|| Error::new(format!("there is no root key `{key}`")))?;
        // Ids resolve in the composed context's whole root (`SPEC.md` 10.5).
        let root = self.composed_root();
        let resolver = Resolver::new(&root, self.schema.as_deref());
        crate::deserialize_resolved(
            value,
            &self.sources,
            self.options.clone(),
            Some(key),
            Some(resolver),
        )
    }

    /// Deserializes the whole root as `R`, in a context (`None`: the
    /// composed one).
    pub fn deserialize_root<R: for<'de> Deserialize<'de>>(
        &self,
        context: Option<&CrolContext>,
    ) -> Result<R, Error> {
        let root = match context {
            None => self.composed_root(),
            Some(context) => self
                .root_in(&context.0)
                .ok_or_else(|| Error::new("this context was not evaluated"))?,
        };
        crate::deserialize(&root, &self.sources, self.options.clone(), None).map(|(value, _)| value)
    }

    /// The data as a [`DataView`](crate::DataView), for [`crate::check`].
    pub fn view(&self) -> crate::DataView<'_> {
        crate::DataView {
            base: &self.base,
            contexts: &self.contexts,
            variants: &self.variants,
        }
    }

    /// The fingerprint of all evaluated data (`SPEC.md` 7.6): BLAKE3-256 of
    /// its canonical encoding.
    pub fn fingerprint(&self) -> [u8; 32] {
        crol_eval::fingerprint::fingerprint(&self.base, &self.contexts, &self.variants)
    }

    /// A fingerprint for each root key, over its value in every evaluated
    /// context (`SPEC.md` 7.6). Where the [`fingerprint`](Self::fingerprint)s
    /// of two data sets differ, comparing these says which keys do:
    /// [`changed_keys`].
    pub fn key_fingerprints(&self) -> BTreeMap<String, [u8; 32]> {
        crol_eval::fingerprint::key_fingerprints(&self.base, &self.contexts, &self.variants)
    }

    /// Applies changes to documents and evaluates again, in the contexts
    /// evaluated so far (`SPEC.md` 10.7): a program reloading a file it
    /// watches. If the result does not load, the error is returned and the
    /// data stays as it was. Unchanged documents are not read again.
    pub fn reload(&mut self, changes: &[DocumentChange<'_>]) -> Result<Reloaded, Error> {
        let changed: Vec<&str> = changes.iter().map(|change| change.layer).collect();
        self.lower_unchanged(&changed);
        let mut sources = self.sources.clone();
        let mut layers = self.layers.clone();
        let mut lowered = self.lowered.clone();
        let mut lower_tried = self.lower_tried.clone();
        for change in changes {
            let index = layers
                .iter()
                .position(|layer| layer.name == change.layer)
                .ok_or_else(|| Error::new(format!("there is no layer `{}`", change.layer)))?;
            let layer = &mut layers[index];
            // What the file says no longer holds: its text does.
            if let Some(slot) = lowered.get_mut(index) {
                *slot = None;
            }
            lower_tried[index] = false;
            let at = layer.documents.iter().position(|(p, _)| p == change.path);
            match (change.text, at) {
                (Some(text), Some(at)) => {
                    let name = sources.file(layer.documents[at].1).name().to_owned();
                    layer.documents[at].1 = sources.add(name, text);
                }
                (Some(text), None) => {
                    let file = sources.add(change.path.to_owned(), text);
                    layer.documents.push((change.path.to_owned(), file));
                }
                (None, Some(at)) => {
                    layer.documents.remove(at);
                }
                (None, None) => {
                    return Err(Error::new(format!(
                        "the layer `{}` has no document `{}`",
                        change.layer, change.path
                    )));
                }
            }
        }
        let mut options = self.data_options.clone();
        if let Some(schema) = &self.schema {
            let (schema, found) = schema.patched(&mut sources, &layers);
            if found.iter().any(Diagnostic::is_error) {
                return Err(Error::from_diagnostics(&sources, &found));
            }
            options = options.with_defaults(Some(schema.defaults()));
        }
        // What the layers from bytes decoded to is kept from before.
        let bytes: Vec<Option<&[u8]>> = lowered.iter().map(|l| l.as_deref()).collect();
        let (compiled, mut diagnostics) =
            crol_eval::compile_binary_after(&sources, &layers, &bytes, &self.compiled);
        let Some(compiled) = compiled else {
            return Err(Error::from_diagnostics(&sources, &diagnostics));
        };
        let names: Vec<Vec<String>> = self
            .contexts
            .iter()
            .map(|data| {
                let names = self.variants.chosen_names(&data.context);
                names.into_iter().map(str::to_owned).collect()
            })
            .collect();
        let evaluated = options.clone().with_contexts(Some(names));
        // Where only some keys can change, only those are evaluated again,
        // unless the schema, which `evaluate_changed` does not see, changed.
        let schema_changed = self.schema.is_some()
            && changes
                .iter()
                .any(|change| change.path == crol_eval::SCHEMA_DOCUMENT);
        let update = (!schema_changed)
            .then(|| {
                crol_eval::evaluate_changed(
                    &self.compiled,
                    &compiled,
                    &evaluated,
                    &self.variants,
                    &self.contexts,
                )
            })
            .flatten();
        if let Some(update) = update {
            let keys: Vec<String> = update.keys().map(str::to_owned).collect();
            let evaluated = Some(keys.len());
            let names = || keys.iter().map(String::as_str);
            let fingerprints = |data: &Self| {
                crol_eval::fingerprint::key_fingerprints_of(
                    &data.base,
                    &data.contexts,
                    &data.variants,
                    names(),
                )
            };
            let before = fingerprints(self);
            if self.strict_ids {
                // An error keeps the data as it was, as any failed reload does.
                let (mut base, mut contexts) = (self.base.clone(), self.contexts.clone());
                update.apply(&mut base, &mut contexts);
                let ids = id_diagnostics(
                    &base,
                    &contexts,
                    &self.composed,
                    self.schema.as_deref(),
                    true,
                );
                if !ids.is_empty() {
                    return Err(Error::from_diagnostics(&sources, &ids));
                }
                (self.base, self.contexts) = (base, contexts);
            } else {
                update.apply(&mut self.base, &mut self.contexts);
            }
            diagnostics.extend(id_diagnostics(
                &self.base,
                &self.contexts,
                &self.composed,
                self.schema.as_deref(),
                false,
            ));
            self.sources = sources;
            self.layers = layers;
            self.lowered = lowered;
            self.lower_tried = lower_tried;
            self.compiled = Arc::new(compiled);
            self.data_options = options;
            self.diagnostics = diagnostics;
            return Ok(Reloaded {
                changed: changed_keys(&before, &fingerprints(self)),
                evaluated,
            });
        }
        let mut evaluation = evaluate_compiled(&compiled, &evaluated);
        diagnostics.append(&mut evaluation.diagnostics);
        let Some(base) = evaluation.value else {
            return Err(Error::from_diagnostics(&sources, &diagnostics));
        };
        let before = self.key_fingerprints();
        let composed = {
            let names = self.variants.chosen_names(&self.composed);
            let ids: Vec<_> = names
                .iter()
                .filter_map(|n| evaluation.variants.id(n))
                .collect();
            evaluation.variants.context(&ids).map_err(Error::new)?
        };
        let ids = id_diagnostics(
            &base,
            &evaluation.contexts,
            &composed,
            self.schema.as_deref(),
            self.strict_ids,
        );
        if self.strict_ids && !ids.is_empty() {
            return Err(Error::from_diagnostics(&sources, &ids));
        }
        diagnostics.extend(ids);
        self.sources = sources;
        self.layers = layers;
        self.lowered = lowered;
        self.lower_tried = lower_tried;
        self.compiled = Arc::new(compiled);
        self.data_options = options;
        self.base = base;
        self.contexts = evaluation.contexts;
        self.variants = evaluation.variants;
        self.composed = composed;
        self.diagnostics = diagnostics;
        Ok(Reloaded {
            changed: changed_keys(&before, &self.key_fingerprints()),
            evaluated: None,
        })
    }

    /// Lowers each text layer but those named, once, so that evaluating them
    /// again need not parse them. A layer that does not stand alone stays as
    /// it is: a template can reach it from any layer (`SPEC.md` 7.8).
    pub(crate) fn lower_unchanged(&mut self, changing: &[&str]) {
        if !self.keep_lowered {
            return;
        }
        for (index, layer) in self.layers.iter().enumerate() {
            if self.lower_tried[index]
                || self.lowered[index].is_some()
                || changing.contains(&layer.name.as_str())
            {
                continue;
            }
            self.lower_tried[index] = true;
            self.lowered[index] = crol_eval::lower_layer(&self.sources, layer).map(Arc::from);
        }
    }

    /// [`explain`](Self::explain) for several paths, working out what they
    /// share once.
    pub(crate) fn explain_all(
        &self,
        paths: &[&str],
        context: &CrolContext,
    ) -> Result<Vec<Vec<ExplainStep>>, Error> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let failed =
            |diagnostics: Vec<Diagnostic>| Error::from_diagnostics(&self.sources, &diagnostics);
        let names: Vec<String> = self
            .variants
            .chosen_names(&context.0)
            .into_iter()
            .map(str::to_owned)
            .collect();
        let plan = crol_eval::explain_plan(&self.compiled, &self.variants, &self.data_options)
            .map_err(failed)?;
        paths
            .iter()
            .map(|path| {
                crol_eval::explain_compiled(&self.compiled, &plan, &self.data_options, path, &names)
                    .map_err(failed)
            })
            .collect()
    }

    /// The operations that produced a value, in order, with the value after
    /// each (`SPEC.md` 12.3): `data.explain("web.limits.memory_mb", &ctx)`.
    pub fn explain(&self, path: &str, context: &CrolContext) -> Result<Vec<ExplainStep>, Error> {
        let names: Vec<String> = self
            .variants
            .chosen_names(&context.0)
            .into_iter()
            .map(str::to_owned)
            .collect();
        crol_eval::explain(
            &self.sources,
            &self.layers,
            &self.data_options,
            path,
            &names,
        )
        .map_err(|diagnostics| Error::from_diagnostics(&self.sources, &diagnostics))
    }
}
