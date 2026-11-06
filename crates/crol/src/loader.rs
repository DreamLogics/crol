//! File loaders (`SPEC.md` 10.7): what turns a module path into a source,
//! and a source into text. [`FsLoader`] reads the file system, and is the
//! default; an application registers others to read documents from anywhere
//! else.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::compose::CrolLayer;
use crate::error::Error;
use crate::mode::{self, Mode};
use crate::module_path;

/// What a [`FileLoader`] fails with: any error. `crol` adds the source or
/// module path to the message, so that it need not say which.
pub type LoaderError = Box<dyn std::error::Error + Send + Sync>;

/// A source's identity: which file, directory or other thing a module path
/// came to. Sources are compared by it, to load a document once and to find
/// cycles of `extends`, and it names them in messages and provenance.
///
/// It is a name of the loader's choosing, or a file system path. A name
/// identifies its source among those of every loader, so it starts with
/// something of the loader's own: `embedded:data/base.crod`,
/// `https://example.org/base.crod`. Its extension states the document's mode
/// as a file's does (`SPEC.md` 4); without `.crod` or `.crol`, the API that
/// loads it does.
#[derive(Debug, Clone)]
pub struct SourceId(Repr);

#[derive(Debug, Clone)]
enum Repr {
    Name(Arc<str>),
    Path {
        /// As written, for messages and for the directory it is in, which
        /// for a symbolic link is not the canonical one's.
        path: PathBuf,
        /// What identifies it.
        canonical: PathBuf,
    },
}

impl SourceId {
    /// The source called `name`.
    pub fn new(name: impl Into<Arc<str>>) -> Self {
        Self(Repr::Name(name.into()))
    }

    /// The file or directory at `path`. It reads the file system: every way
    /// of writing a path to one file is one source.
    pub fn from_path(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
        Self(Repr::Path { path, canonical })
    }

    /// The name, if the source is not a file system path.
    pub fn as_name(&self) -> Option<&str> {
        match &self.0 {
            Repr::Name(name) => Some(name),
            Repr::Path { .. } => None,
        }
    }

    /// The file system path, as written, if the source is one.
    pub fn as_path(&self) -> Option<&Path> {
        match &self.0 {
            Repr::Name(_) => None,
            Repr::Path { path, .. } => Some(path),
        }
    }

    /// The canonical file system path, if the source is one.
    pub fn canonical_path(&self) -> Option<&Path> {
        match &self.0 {
            Repr::Name(_) => None,
            Repr::Path { canonical, .. } => Some(canonical),
        }
    }
}

impl PartialEq for SourceId {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Repr::Name(a), Repr::Name(b)) => a == b,
            (Repr::Path { canonical: a, .. }, Repr::Path { canonical: b, .. }) => a == b,
            _ => false,
        }
    }
}

impl Eq for SourceId {}

impl Hash for SourceId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match &self.0 {
            Repr::Name(name) => name.hash(state),
            Repr::Path { canonical, .. } => canonical.hash(state),
        }
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Repr::Name(name) => f.write_str(name),
            Repr::Path { path, .. } => write!(f, "{}", path.display()),
        }
    }
}

/// A module path for a [`FileLoader`] to resolve (`SPEC.md` 4.2), already
/// applied to where it is written.
#[derive(Debug, Clone, Copy)]
pub struct ModulePath<'a> {
    written: &'a str,
    segments: &'a [String],
    from: Option<&'a SourceId>,
    mode: Mode,
}

impl<'a> ModulePath<'a> {
    /// The names from the root to what the path names, with `super` and
    /// the document's directory applied: `super::defaults` written in
    /// `data/base/web.crod` is `["data", "defaults"]`.
    ///
    /// A path cannot leave the root this way: `crol` rejects one that goes
    /// above it before any loader sees it, and no segment is empty, `.`,
    /// `..` or holds a `/`.
    pub fn segments(&self) -> &'a [String] {
        self.segments
    }

    /// The path as it was written: `super::defaults`.
    pub fn written(&self) -> &'a str {
        self.written
    }

    /// The document the path is written in; `None` in code and in documents
    /// built in code.
    pub fn from(&self) -> Option<&'a SourceId> {
        self.from
    }

    /// What is looked for: [`Mode::Data`] for a layer or an `extends`,
    /// [`Mode::Value`] for a schema or project file, which
    /// [`FileLoaders::resolve_value`] finds. A path has no extension, so
    /// this is what says which one the file has (`SPEC.md` 4.2).
    pub fn mode(&self) -> Mode {
        self.mode
    }
}

impl fmt::Display for ModulePath<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for segment in self.segments {
            write!(f, "::{segment}")?;
        }
        Ok(())
    }
}

/// What a source holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loaded {
    /// One document: its text, which must be UTF-8 (`SPEC.md` 2.1).
    Document(Vec<u8>),
    /// Every document below a directory (`SPEC.md` 7.1): each one's path
    /// relative to the directory, with `/` separators, and its source. The
    /// order does not matter: `crol` sorts them.
    Directory(Vec<(String, SourceId)>),
}

/// Resolves module paths to sources and loads them (`SPEC.md` 10.7): where
/// documents come from. Implement it to compose documents that are not
/// files, and register it with
/// [`CrolComposer::with_file_loader`](crate::CrolComposer::with_file_loader).
///
/// It is used while loading, never while evaluating, and `load` must always
/// give the same text for one source for a composition to be reproducible
/// (`SPEC.md` 7.6). It is `Send + Sync`, as the composer that holds it is;
/// it is blocking.
///
/// ```
/// use std::collections::BTreeMap;
///
/// use crol::{CrolComposer, FileLoader, Loaded, LoaderError, ModulePath, SourceId};
///
/// /// Documents built into the program, by their paths.
/// struct Embedded(BTreeMap<&'static str, &'static str>);
///
/// impl FileLoader for Embedded {
///     fn resolve(&self, path: &ModulePath<'_>) -> Result<Option<SourceId>, LoaderError> {
///         let file = format!("{}.crod", path.segments().join("/"));
///         Ok(self
///             .0
///             .contains_key(file.as_str())
///             .then(|| SourceId::new(format!("embedded:{file}"))))
///     }
///
///     fn load(&self, source: &SourceId) -> Result<Loaded, LoaderError> {
///         let file = source.as_name().and_then(|n| n.strip_prefix("embedded:"));
///         match file.and_then(|file| self.0.get(file)) {
///             Some(text) => Ok(Loaded::Document(text.as_bytes().to_vec())),
///             None => Err("no such document".into()),
///         }
///     }
/// }
///
/// let embedded = Embedded(BTreeMap::from([
///     ("data/base.crod", "+port: 80,"),
///     ("data/local.crod", "#![extends(base)]\n\nport: & + 1,"),
/// ]));
/// let data = CrolComposer::with_cfg(crol::CrolConfig::<String> {
///     load_extends: true,
///     ..Default::default()
/// })
/// .with_file_loader(embedded)
/// .load("::data::local")?
/// .compose_data()?;
/// assert_eq!(data.get::<u16>("port")?, 81);
/// # Ok::<(), crol::Error>(())
/// ```
pub trait FileLoader: Send + Sync {
    /// The source `path` names, or `None` if this loader has nothing by
    /// that name: the next loader is then asked. An error stops the search.
    fn resolve(&self, path: &ModulePath<'_>) -> Result<Option<SourceId>, LoaderError>;

    /// What a source this loader gave holds: those from
    /// [`resolve`](Self::resolve), and those a [`Loaded::Directory`] lists,
    /// which must be documents. The error is reported with the source; a
    /// [`crol::Error`](crate::Error) is reported as it is.
    ///
    /// A source whose name ends in `.crol` is refused as a layer's document,
    /// and one that ends in `.crod` as a value document, however it is loaded
    /// (`SPEC.md` 4).
    fn load(&self, source: &SourceId) -> Result<Loaded, LoaderError>;
}

/// The file loader that reads the file system: module paths name
/// `name.crod` or `name/` below its root, and `name.crol` for a value
/// document (`SPEC.md` 4.2), and a directory holds every `.crod` file below
/// it (`SPEC.md` 7.1). It lists `.crol` files as well, which then fail: a
/// directory never hides a document in it.
///
/// One for [`CrolConfig::root_path`](crate::CrolConfig::root_path) is the
/// default, after every registered loader. Register another for a second
/// tree, whose files then come first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsLoader {
    root: PathBuf,
}

impl FsLoader {
    /// A loader for the tree at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl FileLoader for FsLoader {
    fn resolve(&self, path: &ModulePath<'_>) -> Result<Option<SourceId>, LoaderError> {
        let Some((last, directories)) = path.segments.split_last() else {
            return Ok(None);
        };
        // A directory another loader listed may have any name, which must
        // not mean anything to this file system.
        if path.segments.iter().any(|s| s.contains(['/', '\\'])) {
            return Ok(None);
        }
        let base = directories
            .iter()
            .fold(self.root.clone(), |base, directory| base.join(directory));
        if !module_path::exists(&base, last) {
            return Ok(None);
        }
        let found = module_path::named(path.written, &base, last, path.mode)?;
        Ok(Some(SourceId::from_path(found)))
    }

    fn load(&self, source: &SourceId) -> Result<Loaded, LoaderError> {
        let Some(path) = source.as_path() else {
            return Err("it is not a file system path".into());
        };
        if path.is_dir() {
            let mut documents = Vec::new();
            read_directory(path, path, &mut documents)?;
            return Ok(Loaded::Directory(documents));
        }
        Ok(Loaded::Document(std::fs::read(path)?))
    }
}

fn read_directory(
    base: &Path,
    dir: &Path,
    documents: &mut Vec<(String, SourceId)>,
) -> Result<(), Error> {
    let failed = |e: std::io::Error| Error::new(format!("cannot read {}: {e}", dir.display()));
    for entry in std::fs::read_dir(dir).map_err(failed)? {
        let entry = entry.map_err(failed)?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        // Most entries need no look at the file itself, and no path of their
        // own, which matters for a directory that also holds thousands of
        // other files. A symbolic link is followed.
        let kind = entry.file_type().map_err(failed)?;
        if !(kind.is_dir() || kind.is_symlink() || mode::is_document(&name)) {
            continue;
        }
        let path = entry.path();
        let is_dir = if kind.is_symlink() {
            path.is_dir()
        } else {
            kind.is_dir()
        };
        if is_dir {
            read_directory(base, &path, documents)?;
        } else if mode::is_document(&name) && (kind.is_file() || path.is_file()) {
            let relative = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            documents.push((relative, SourceId::from_path(path)));
        }
    }
    Ok(())
}

/// The file loaders of a composer, in the order they are asked: those
/// registered, first the first, then the default [`FsLoader`] unless it was
/// turned off.
#[derive(Clone)]
pub struct FileLoaders {
    registered: Vec<Arc<dyn FileLoader>>,
    file_system: bool,
    /// What reads files in place of an [`FsLoader`], if anything does.
    file_reader: Option<Arc<dyn FileLoader>>,
    /// The project file, which a layer's directory may hold.
    project_file: Option<SourceId>,
}

impl Default for FileLoaders {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for FileLoaders {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileLoaders")
            .field("registered", &self.registered.len())
            .field("file_system", &self.file_system)
            .field("file_reader", &self.file_reader.is_some())
            .field("project_file", &self.project_file)
            .finish()
    }
}

impl PartialEq for FileLoaders {
    fn eq(&self, other: &Self) -> bool {
        self.file_system == other.file_system
            && self.project_file == other.project_file
            && match (&self.file_reader, &other.file_reader) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (a, b) => a.is_none() && b.is_none(),
            }
            && self.registered.len() == other.registered.len()
            && self
                .registered
                .iter()
                .zip(&other.registered)
                .all(|(a, b)| Arc::ptr_eq(a, b))
    }
}

impl Eq for FileLoaders {}

/// Where a module path is written.
#[derive(Clone, Copy)]
pub(crate) struct Origin<'a> {
    /// The document, if it has a source.
    pub(crate) from: Option<&'a SourceId>,
    /// Its directory, as names from the root; `None` for a document that
    /// was not reached from the root, loaded by a file system path.
    pub(crate) directory: Option<&'a [String]>,
}

impl Origin<'_> {
    /// Written in code: at the root, in no document.
    const CODE: Self = Self {
        from: None,
        directory: Some(&[]),
    };
}

impl FileLoaders {
    /// The default: the file system alone.
    pub fn new() -> Self {
        Self {
            registered: Vec::new(),
            file_system: true,
            file_reader: None,
            project_file: None,
        }
    }

    /// Adds a loader, asked after those added so far and before the file
    /// system.
    pub fn with(mut self, loader: impl FileLoader + 'static) -> Self {
        self.registered.push(Arc::new(loader));
        self
    }

    /// Turns off the default [`FsLoader`]: what the registered loaders do
    /// not have is an error, and nothing is read from the file system, even
    /// by a `&Path` given to the composer. For documents that are not
    /// trusted with the application's files.
    pub fn without_file_system(mut self) -> Self {
        self.file_system = false;
        self
    }

    /// Reads the files of the file system through `reader`: paths still
    /// resolve on the file system, and `reader`'s
    /// [`load`](FileLoader::load) says what each source holds, however it
    /// was reached, also from a document loaded by a file system path. For
    /// what puts something over the files, as an editor's unsaved buffers.
    pub fn with_file_reader(mut self, reader: impl FileLoader + 'static) -> Self {
        self.file_reader = Some(Arc::new(reader));
        self
    }

    /// Whether the default [`FsLoader`] is on.
    pub fn file_system(&self) -> bool {
        self.file_system
    }

    /// Names the project file (`SPEC.md` 12.4). It is the one `.crol` file a
    /// layer's directory may hold: it is left out of the layer, where any
    /// other is an error.
    pub fn with_project_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.project_file = Some(SourceId::from_path(path));
        self
    }

    /// The project file, if one was named.
    pub(crate) fn project_file(&self) -> Option<&SourceId> {
        self.project_file.as_ref()
    }

    /// Resolves a module path written in code to a layer: a `.crod` file or
    /// a directory. Relative paths start from `root`, which is also the
    /// default loader's root.
    pub fn resolve(&self, module_path: &str, root: &Path) -> Result<ResolvedSource, Error> {
        self.resolve_from(module_path, root, Origin::CODE, Mode::Data)
    }

    /// [`resolve`](Self::resolve) for a value document: a `.crol` file, such
    /// as a schema or project file (`SPEC.md` 4.2).
    pub fn resolve_value(&self, module_path: &str, root: &Path) -> Result<ResolvedSource, Error> {
        self.resolve_from(module_path, root, Origin::CODE, Mode::Value)
    }

    /// The layer document at a file system path, which need not be below
    /// `root`, or have a name a module path can write. Like
    /// [`CrolLayer::from_path`], with the file read as
    /// [`with_file_reader`](Self::with_file_reader) says: its relative
    /// module paths start from its directory.
    pub fn resolve_path(&self, path: &Path, root: &Path) -> ResolvedSource {
        ResolvedSource {
            loader: self.file_system_loader(root),
            id: SourceId::from_path(path),
            file_name: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            target: None,
            mode: Mode::Data,
            project_file: self.project_file.clone(),
        }
    }

    /// What reads the file system below `root`.
    fn file_system_loader(&self, root: &Path) -> Arc<dyn FileLoader> {
        match &self.file_reader {
            Some(reader) => Arc::clone(reader),
            None => Arc::new(FsLoader::new(root)),
        }
    }

    /// Resolves a module path written at `origin`, to a document of `mode`.
    pub(crate) fn resolve_from(
        &self,
        written: &str,
        root: &Path,
        origin: Origin<'_>,
        mode: Mode,
    ) -> Result<ResolvedSource, Error> {
        let parsed = module_path::parse(written)?;
        let last = parsed.segments.last().expect("not empty");
        let file_name = format!("{last}.{}", mode.extension());
        let base = if parsed.rooted {
            Some(&[][..])
        } else {
            origin.directory
        };
        let target: Option<Vec<String>> = match base {
            Some(base) => {
                let kept = base
                    .len()
                    .checked_sub(parsed.supers)
                    .ok_or_else(|| module_path::above_root(written, root))?;
                let segments = parsed.segments.iter().map(|s| (*s).to_owned());
                Some(base[..kept].iter().cloned().chain(segments).collect())
            }
            None => None,
        };
        if let Some(target) = &target {
            let path = ModulePath {
                written,
                segments: target,
                from: origin.from,
                mode,
            };
            for loader in &self.registered {
                let found = loader
                    .resolve(&path)
                    .map_err(|e| resolve_error(written, e))?;
                if let Some(id) = found {
                    return Ok(ResolvedSource {
                        loader: Arc::clone(loader),
                        id,
                        file_name,
                        target: Some(target.clone()),
                        mode,
                        project_file: self.project_file.clone(),
                    });
                }
            }
        }
        if !self.file_system {
            return Err(Error::new(format!(
                "the module path `{written}` names nothing: no file loader has it"
            )));
        }
        let path = match (origin.from, &target) {
            // A document that is not a file has no directory to start
            // from: its paths are those of the tree.
            (Some(from), Some(target)) if from.as_path().is_none() => {
                module_path::resolve(&format!("::{}", target.join("::")), root, None, mode)
            }
            _ => {
                let here = origin.from.and_then(SourceId::as_path);
                module_path::resolve(written, root, here.and_then(Path::parent), mode)
            }
        }?;
        Ok(ResolvedSource {
            loader: self.file_system_loader(root),
            id: SourceId::from_path(path),
            file_name,
            target,
            mode,
            project_file: self.project_file.clone(),
        })
    }
}

fn resolve_error(written: &str, error: LoaderError) -> Error {
    match error.downcast::<Error>() {
        Ok(error) => *error,
        Err(error) => Error::new(format!("cannot resolve `{written}`: {error}")),
    }
}

/// What a module path came to, from [`FileLoaders::resolve`]: a source, and
/// the loader that has it.
#[derive(Clone)]
pub struct ResolvedSource {
    loader: Arc<dyn FileLoader>,
    id: SourceId,
    /// The relative path of the document, if it is one.
    file_name: String,
    /// Its names from the root, if known.
    target: Option<Vec<String>>,
    /// What was looked for.
    mode: Mode,
    /// The file a directory may hold that is no document of the layer.
    project_file: Option<SourceId>,
}

impl fmt::Debug for ResolvedSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedSource")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl ResolvedSource {
    /// The source's identity.
    pub fn id(&self) -> &SourceId {
        &self.id
    }

    /// Loads the source as a layer called `name`.
    pub fn load(&self, name: impl Into<String>) -> Result<CrolLayer, Error> {
        CrolLayer::read(
            name.into(),
            &*self.loader,
            &self.id,
            self.file_name.clone(),
            self.target.as_deref(),
            self.project_file.as_ref(),
        )
    }

    /// The text of the source, for one that is a single document, such as
    /// a schema file or a project file that is not on the file system. Its
    /// extension must not contradict what was resolved (`SPEC.md` 4).
    pub fn text(&self) -> Result<String, Error> {
        check_mode(&self.id, self.mode)?;
        match load(&*self.loader, &self.id)? {
            Loaded::Document(bytes) => text(&self.id, bytes),
            Loaded::Directory(_) => {
                Err(Error::new(format!("{} is a directory", self.id))
                    .with_source_id(self.id.clone()))
            }
        }
    }
}

/// Checks a source's extension against the mode it is read in (`SPEC.md` 4).
pub(crate) fn check_mode(id: &SourceId, mode: Mode) -> Result<(), Error> {
    mode.check(&id.to_string())
        .map_err(|error| error.with_source_id(id.clone()))
}

/// Asks `loader` for a source; the error says which.
pub(crate) fn load(loader: &dyn FileLoader, id: &SourceId) -> Result<Loaded, Error> {
    loader
        .load(id)
        .map_err(|error| match error.downcast::<Error>() {
            Ok(error) => *error,
            Err(error) => Error::new(format!("cannot read {id}: {error}")),
        })
        .map_err(|error| error.with_source_id(id.clone()))
}

/// A document's bytes as text.
pub(crate) fn text(id: &SourceId, bytes: Vec<u8>) -> Result<String, Error> {
    String::from_utf8(bytes)
        .map_err(|_| Error::new(format!("{id} is not valid UTF-8")).with_source_id(id.clone()))
}
