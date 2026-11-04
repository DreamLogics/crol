//! The composer: layers in, Rust values out (`SPEC.md` 7, 10.7).

use std::borrow::Cow;
use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};

use crol_eval::{
    Context, ContextData, DataOptions, Diagnostic, ExplainStep, Key, Layer, Sources, Value,
    ValueKind, VariantKeys, Variants, evaluate_data,
};
use crol_syntax::TextRange;
use crol_syntax::ast::{self, AstNode};
use serde::Deserialize;

use crate::de;
use crate::error::Error;
use crate::id::IdUse;
use crate::loader::{self, FileLoader, FileLoaders, FsLoader, Loaded, Origin, SourceId};
use crate::mode::Mode;

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
}

/// The variants an application declares (`SPEC.md` 8.1). Data cannot add
/// or change them.
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
    /// The most contexts evaluated without `contexts`. Default: 4096.
    pub max_contexts: usize,
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
    /// Warn when one key is patched from several documents of one layer.
    /// Default: `true`.
    pub warn_cross_file_patches: bool,
}

impl<T> Default for CrolConfig<T> {
    fn default() -> Self {
        Self {
            variants: CrolVariants::default(),
            root_path: std::env::current_dir().unwrap_or_default(),
            file_loaders: FileLoaders::new(),
            load_extends: false,
            contexts: None,
            max_contexts: 4096,
            variant_keys: VariantKeys::Any,
            implicit_some: true,
            allow_unknown_fields: false,
            require_complete: true,
            warn_cross_file_patches: true,
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
        Self { name, documents }
    }

    /// A layer of one file, or of every `.crod` file below a directory
    /// (`SPEC.md` 7.1). Hidden files and directories are skipped. A `.crov`
    /// or `.crol` file, given or in the directory, is an error (`SPEC.md` 4).
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
    /// The sources of documents loaded, for `load_extends`.
    loaded: HashSet<SourceId>,
    /// An error from `load_layer` or `with_variant`, reported by `compose`.
    error: Option<Error>,
    /// The schema, for `&default`.
    schema: Option<crol_eval::Defaults>,
    /// The schema's aliases and settings, for completeness.
    structs: Option<std::sync::Arc<de::Structs>>,
    /// The variants made active with `with_variant`.
    chosen: Vec<T>,
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
        self.schema = Some(crol_eval::Defaults(std::sync::Arc::new(
            crate::schema::SchemaDefaults(schema),
        )));
        self
    }

    /// Makes a variant active in the context to compose (`SPEC.md` 8.1).
    /// Fails for a variant that is not declared (when variants are
    /// declared), or a second choice in a group that is not an ancestor or
    /// descendant of the first.
    pub fn with_variant(mut self, variant: impl Into<T>) -> Result<Self, Error> {
        let variant = variant.into();
        let declared = self.config.variants.build()?;
        if !self.config.variants.is_empty() {
            if declared.id(variant.name()).is_none() {
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
    /// `in_progress` holds the sources whose extends are being loaded, to
    /// find cycles.
    fn add(&mut self, layer: CrolLayer, in_progress: &mut Vec<SourceId>) -> Result<(), Error> {
        if self.config.load_extends {
            // In document order: by path (`SPEC.md` 7.1).
            let mut documents: Vec<&Document> = layer.documents.iter().collect();
            documents.sort_by(|a, b| a.relative.cmp(&b.relative));
            for document in documents {
                for module_path in extends_of(&document.text) {
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
                    in_progress.push(id);
                    self.add(extended, in_progress)?;
                    in_progress.pop();
                    if document.source.is_some() {
                        in_progress.pop();
                    }
                }
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

    fn evaluate(self, contexts: Option<Vec<Vec<String>>>) -> Result<CrolData<T>, Error> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let variants = self.config.variants.build()?;
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
            .with_warn_cross_file_patches(self.config.warn_cross_file_patches)
            .with_variants(variants)
            .with_contexts(contexts)
            .with_max_contexts(self.config.max_contexts)
            .with_variant_keys(self.config.variant_keys)
            .with_defaults(self.schema.clone());
        let evaluation = evaluate_data(&sources, &layers, &options);
        let Some(base) = evaluation.value.clone() else {
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
        Ok(CrolData {
            sources,
            layers,
            data_options: options,
            base,
            contexts: evaluation.contexts,
            variants: evaluation.variants,
            composed,
            diagnostics: evaluation.diagnostics,
            options: de::Options {
                structs: self.structs.clone(),
                ..self.config.deserialize_options()
            },
            names: std::marker::PhantomData,
        })
    }
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

/// A context: a set of active variants (`SPEC.md` 8.1), from
/// [`CrolData::context`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CrolContext(Context);

/// Evaluated data (`SPEC.md` 10.7): the root keys and their values in every
/// evaluated context, and the warnings found.
#[derive(Debug, Clone)]
pub struct CrolData<T = String> {
    sources: Sources,
    /// The layers and options, for re-evaluating in `explain`.
    layers: Vec<Layer>,
    data_options: DataOptions,
    base: Value,
    contexts: Vec<ContextData>,
    variants: Variants,
    /// The context `with_variant` chose, for `get` and `deserialize_root`.
    composed: Context,
    diagnostics: Vec<Diagnostic>,
    options: de::Options,
    names: std::marker::PhantomData<T>,
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
        let key = Key::Str(key.to_owned());
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
        let mut ids = Vec::new();
        for variant in variants {
            let variant = variant.into();
            ids.push(
                self.variants
                    .id(variant.name())
                    .ok_or_else(|| Error::new(format!("`{}` is not a variant", variant.name())))?,
            );
        }
        let context = self.variants.context(&ids).map_err(Error::new)?;
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
                    Key::Str(s) => Some(s.clone()),
                    Key::Int(_) => None,
                })
                .collect(),
            _ => Vec::new(),
        }
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
        crate::deserialize(value, &self.sources, self.options.clone(), Some(key))
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
