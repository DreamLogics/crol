//! What the server knows: the latest evaluation of the project, or of each
//! open document on its own (`SPEC.md` 12.5).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crol::project::Project;
use crol::schema::{DataRoot, SchemaFile};
use crol::{
    CheckOptions, CrolComposer, CrolConfig, CrolLayer, ExtendsLoad, FileLoader, FileLoaders,
    FsLoader, Loaded, LoaderError, Mode, ModulePath, SourceId,
};
use crol_eval::{
    Compiled, DataOptions, Diagnostic, Evaluation, ExplainPlan, ExplainStep, FileId, Layer,
    Severity, Sources,
};
use lsp_types::Uri;

use crate::Settings;
use crate::convert::{self, Encoding};

/// What the server knows of schemas named by URL or crate that are being
/// fetched or failed to: a message for each, by source and directory.
pub(crate) type RemoteStatus = HashMap<(String, PathBuf), String>;

/// The open buffers, by canonical path.
type Buffers = Arc<HashMap<PathBuf, String>>;

/// The file system with the open buffers over it: a buffer replaces its
/// file, saved or not (`SPEC.md` 12.5). Paths resolve as on the file system
/// ([`FileLoaders::with_file_reader`]).
///
/// A file a directory should not hold is left out of the directory, and
/// noted in `left_out`, where `crol check` stops at it: the rest of a layer
/// is no less meaningful for it (`SPEC.md` 12.5).
struct BufferLoader {
    files: FsLoader,
    open: Buffers,
    project_file: Option<SourceId>,
    left_out: LeftOut,
}

/// What was left out of the directories read.
type LeftOut = Arc<Mutex<Vec<Problem>>>;

impl BufferLoader {
    /// The loaders that read below `root`, with the buffers over the files,
    /// as the composer of `crol check` has them on the file system.
    fn loaders(
        root: &Path,
        open: &Buffers,
        project_file: Option<&Path>,
        left_out: &LeftOut,
    ) -> FileLoaders {
        let loaders = FileLoaders::new().with_file_reader(Self {
            files: FsLoader::new(root),
            open: Arc::clone(open),
            project_file: project_file.map(SourceId::from_path),
            left_out: Arc::clone(left_out),
        });
        match project_file {
            Some(file) => loaders.with_project_file(file),
            None => loaders,
        }
    }
}

impl FileLoader for BufferLoader {
    fn resolve(&self, path: &ModulePath<'_>) -> Result<Option<SourceId>, LoaderError> {
        self.files.resolve(path)
    }

    fn load(&self, source: &SourceId) -> Result<Loaded, LoaderError> {
        if let Some(text) = source.canonical_path().and_then(|p| self.open.get(p)) {
            return Ok(Loaded::Document(text.clone().into_bytes()));
        }
        let loaded = self.files.load(source)?;
        let Loaded::Directory(entries) = loaded else {
            return Ok(loaded);
        };
        let mut left_out = self.left_out.lock().unwrap_or_else(|e| e.into_inner());
        let mut kept = Vec::with_capacity(entries.len());
        for (relative, entry) in entries {
            let refused = Mode::Data.check(&entry.to_string());
            match refused {
                Err(error) if self.project_file.as_ref() != Some(&entry) => {
                    let problem = Problem {
                        message: error.to_string(),
                        file: entry.as_path().map(Path::to_path_buf),
                    };
                    // A directory may be read twice: as a layer, and for an
                    // `extends`.
                    if left_out.iter().all(|p| p.file != problem.file) {
                        left_out.push(problem);
                    }
                }
                _ => kept.push((relative, entry)),
            }
        }
        Ok(Loaded::Directory(kept))
    }
}

/// An application's own checks: they receive the latest evaluation and
/// return diagnostics, published with the others (`SPEC.md` 12.5).
pub type Checks = Box<dyn Fn(&Snapshot) -> Vec<Diagnostic> + Send>;

/// The latest evaluation.
pub struct Snapshot {
    /// Every file read.
    pub sources: Sources,
    /// The project's layers; empty without a project.
    pub layers: Vec<Layer>,
    /// The options the layers were evaluated with.
    pub options: DataOptions,
    /// The project's evaluation; empty without a project.
    pub evaluation: Evaluation,
    /// The project's layers compiled, for looking up one key at a time;
    /// `None` if they do not compile.
    compiled: Option<Compiled>,
    /// What looking up keys in `compiled` takes.
    explain_plan: OnceLock<Result<ExplainPlan, Vec<Diagnostic>>>,
    /// The doc comments of root keys in the project's layers.
    root_docs: OnceLock<HashMap<String, String>>,
    /// Whether any document of the project's layers has a doc comment.
    has_docs: OnceLock<bool>,
    /// While the layers do not evaluate (as while a document is being
    /// typed): the latest evaluation of them that did, for completion.
    pub last_good: Option<Evaluation>,
    /// While a document on its own does not evaluate: the latest evaluation
    /// of it that did, by its path (ids do not outlive a snapshot).
    last_good_documents: HashMap<PathBuf, Evaluation>,
    /// Documents evaluated on their own, each in the mode its extension
    /// states (`SPEC.md` 4): a name that states none is value mode.
    pub documents: Vec<(FileId, Evaluation)>,
    /// Diagnostics from the schema and the application's checks.
    pub extra: Vec<Diagnostic>,
    /// The project's schema, if it names one.
    pub schema: Option<SchemaFile>,
    /// The type path of the project's root type, with the schema.
    pub root_type: Option<String>,
    /// The schemas of documents evaluated on their own, with the type of
    /// each one's value: the one it names, or the project file's.
    own_schemas: HashMap<FileId, OwnSchema>,
    /// The project, with its directory, for what its `files` say of a
    /// document (`SPEC.md` 12.4).
    project: Option<(Project, PathBuf)>,
    /// What each `.crod` document evaluated on its own was evaluated with.
    chains: HashMap<FileId, Chain>,
    /// The path of each file, by its id.
    paths: Vec<PathBuf>,
    /// The id of each file, by its canonical path: a file read twice, as
    /// when two documents extend it, is one file.
    by_path: HashMap<PathBuf, FileId>,
    /// Files of read-only layers, which get no diagnostics.
    read_only: HashSet<FileId>,
    /// A message for the project as a whole, such as a project file that
    /// cannot be read.
    pub problem: Option<String>,
    /// The schemas named by URL or crate, by source and directory, for
    /// fetching them.
    pub(crate) remote: Vec<(String, PathBuf)>,
}

/// A `.crod` document on its own, on top of what it extends (`SPEC.md` 4.1).
struct Chain {
    /// Lowest first; the document is the last.
    layers: Vec<Layer>,
    options: DataOptions,
}

/// An `#![extends(...)]`, followed.
struct Followed {
    /// The document it is written in.
    file: FileId,
    range: crol_syntax::TextRange,
    written: String,
    /// The files it brought in.
    files: Vec<FileId>,
    /// Why it could not be followed.
    error: Option<String>,
}

/// The schema of a document evaluated on its own.
struct OwnSchema {
    file: SchemaFile,
    root: Option<String>,
    mode: Mode,
}

/// What is wrong with the project as a whole or a part of it, and the file
/// to say it on, if one is to blame.
struct Problem {
    message: String,
    file: Option<PathBuf>,
}

impl From<String> for Problem {
    fn from(message: String) -> Self {
        Self {
            message,
            file: None,
        }
    }
}

impl From<crol::Error> for Problem {
    fn from(error: crol::Error) -> Self {
        Self {
            message: error.to_string(),
            file: error
                .source_id()
                .and_then(SourceId::as_path)
                .map(Path::to_path_buf),
        }
    }
}

/// The schema a document is written against.
pub(crate) struct DocumentSchema<'a> {
    pub(crate) file: &'a SchemaFile,
    /// The type path of the root type, if not the schema's.
    pub(crate) root: Option<&'a str>,
    /// Whether the document is a value of the root type (a value document)
    /// rather than data whose root values are (`SPEC.md` 4.1).
    pub(crate) value_mode: bool,
}

impl DocumentSchema<'_> {
    pub(crate) fn root_type(&self) -> &str {
        self.root.unwrap_or(&self.file.root)
    }

    /// Whether the document as a whole is a value of the root type: a value
    /// document, or data whose root type is a struct or map, where a root
    /// key is a field of it (`SPEC.md` 4.1).
    pub(crate) fn is_whole_value(&self) -> bool {
        self.value_mode || self.file.data_root(self.root) == DataRoot::Document
    }
}

impl Snapshot {
    /// The schema of a file: its own, for a document on its own, else the
    /// project's.
    pub(crate) fn schema_for(&self, file: Option<FileId>) -> Option<DocumentSchema<'_>> {
        if let Some(own) = file.and_then(|f| self.own_schemas.get(&f)) {
            return Some(DocumentSchema {
                file: &own.file,
                root: own.root.as_deref(),
                value_mode: own.mode == Mode::Value,
            });
        }
        if file.is_some_and(|f| !self.in_layers(f)) {
            return None;
        }
        Some(DocumentSchema {
            file: self.schema.as_ref()?,
            root: self.root_type.as_deref(),
            value_mode: false,
        })
    }

    /// The file a path is, if it was read.
    pub fn file_of(&self, path: &Path) -> Option<FileId> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.by_path.get(&canonical).copied()
    }

    /// The file at a path, added with `text` if it was not read yet: what
    /// several evaluations read is one file, so that diagnostics and
    /// locations agree on it.
    fn intern(&mut self, display: &str, text: &str) -> FileId {
        let path = PathBuf::from(display);
        let canonical = path.canonicalize().unwrap_or(path);
        if let Some(file) = self.by_path.get(&canonical) {
            return *file;
        }
        let file = self.sources.add(display, text);
        self.paths.push(canonical.clone());
        self.by_path.insert(canonical, file);
        file
    }

    /// The composer's layers, as the evaluator takes them.
    fn intern_layers(&mut self, layers: &[CrolLayer]) -> Vec<Layer> {
        layers
            .iter()
            .map(|layer| Layer {
                name: layer.name().to_owned(),
                documents: layer
                    .documents()
                    .map(|(relative, display, text)| {
                        (relative.to_owned(), self.intern(display, text))
                    })
                    .collect(),
            })
            .collect()
    }

    /// The composer's account of `extends`, with the files it brought in.
    fn followed(&mut self, loads: Vec<ExtendsLoad>, layers: &[Layer]) -> Vec<Followed> {
        loads
            .into_iter()
            .map(|load| Followed {
                file: self.intern(&load.document, ""),
                range: load.range,
                files: layers[load.layers]
                    .iter()
                    .flat_map(|l| l.documents.iter().map(|(_, file)| *file))
                    .collect(),
                written: load.path,
                error: load.error.map(|e| e.to_string()),
            })
            .collect()
    }

    /// The latest evaluation that had a value of a file, if the one it
    /// belongs to now has none: of the layers, or of a document on its own.
    pub(crate) fn last_good_of(&self, file: Option<FileId>) -> Option<&Evaluation> {
        match file {
            Some(file) if !self.in_layers(file) => self.last_good_documents.get(self.path(file)?),
            _ => self.last_good.as_ref(),
        }
    }

    /// Takes over from `previous` what still stands while something does
    /// not evaluate: a document being typed mostly does not.
    pub(crate) fn keep_last_good(&mut self, mut previous: Snapshot) {
        for (file, evaluation) in std::mem::take(&mut previous.documents) {
            if evaluation.value.is_some()
                && let Some(path) = previous.path(file).map(Path::to_path_buf)
            {
                previous.last_good_documents.insert(path, evaluation);
            }
        }
        for (file, evaluation) in &self.documents {
            if evaluation.value.is_none()
                && let Some(path) = self.path(*file)
                && let Some(good) = previous.last_good_documents.remove(path)
            {
                self.last_good_documents.insert(path.to_path_buf(), good);
            }
        }
        if self.evaluation.value.is_none() {
            // Better than what is below a layer that did not load, if that
            // is all there is now.
            self.last_good = match previous.evaluation.value {
                Some(_) => Some(previous.evaluation),
                None => self.last_good.take().or(previous.last_good),
            };
        }
    }

    /// What the project's `files` say of a value document: the project's
    /// schema, with the type to check it as; nothing if no glob matches it
    /// (`SPEC.md` 12.4).
    #[allow(clippy::type_complexity)]
    fn project_files(
        &self,
        path: &Path,
        mode: Mode,
    ) -> Option<(
        Result<SchemaFile, (crol_syntax::TextRange, String)>,
        Option<String>,
    )> {
        let (project, dir) = self.project.as_ref()?;
        if mode != Mode::Value {
            return None;
        }
        let type_name = project.file_type(dir, path)?;
        let failed = |why: String| {
            let message =
                format!("project: `files` gives this document the type `{type_name}`, {why}");
            Some((Err((Default::default(), message)), None))
        };
        let Some(schema) = &self.schema else {
            return failed(match project.schema {
                Some(_) => "and the project's schema did not load".to_owned(),
                None => "and the project has no schema".to_owned(),
            });
        };
        match schema.type_path(type_name) {
            Some(root) => Some((Ok(schema.clone()), Some(root.to_owned()))),
            None => failed("which the schema does not have".to_owned()),
        }
    }

    /// The layers and options a data document was evaluated with: the
    /// project's for a file in its layers, else its own (`SPEC.md` 4.1).
    /// Nothing for a document that is not data.
    /// What [`crol_eval::explain`] says of `path` in `layers`, which are the
    /// project's or a [`Snapshot::chain_of`]: from what is compiled already,
    /// if they are the project's.
    pub(crate) fn explain(
        &self,
        layers: &[Layer],
        options: &DataOptions,
        path: &str,
        context: &[String],
    ) -> Result<Vec<ExplainStep>, Vec<Diagnostic>> {
        if !std::ptr::eq(layers, self.layers.as_slice()) {
            return crol_eval::explain(&self.sources, layers, options, path, context);
        }
        if self.evaluation.has_errors() {
            return Err(self.evaluation.diagnostics.clone());
        }
        let Some(compiled) = &self.compiled else {
            return Err(Vec::new());
        };
        let plan = self
            .explain_plan
            .get_or_init(|| crol_eval::explain_plan(compiled, &self.evaluation.variants, options));
        match plan {
            Ok(plan) => crol_eval::explain_compiled(compiled, plan, options, path, context),
            Err(diagnostics) => Err(diagnostics.clone()),
        }
    }

    /// Whether every document of a layer is read only.
    fn is_read_only_layer(&self, layer: &Layer) -> bool {
        !layer.documents.is_empty()
            && (layer.documents.iter()).all(|(_, file)| self.read_only.contains(file))
    }

    /// Whether any document of the project's layers has a doc comment.
    pub(crate) fn project_has_docs(&self, find: impl FnOnce() -> bool) -> bool {
        *self.has_docs.get_or_init(find)
    }

    /// The doc comments of root keys in the project's layers.
    pub(crate) fn project_root_docs(
        &self,
        find: impl FnOnce() -> HashMap<String, String>,
    ) -> &HashMap<String, String> {
        self.root_docs.get_or_init(find)
    }

    pub(crate) fn chain_of(&self, file: FileId) -> Option<(&[Layer], &DataOptions)> {
        if self.in_layers(file) {
            return Some((&self.layers, &self.options));
        }
        let chain = self.chains.get(&file)?;
        Some((&chain.layers, &chain.options))
    }

    /// A file's path.
    pub fn path(&self, file: FileId) -> Option<&Path> {
        self.paths.get(file.index()).map(PathBuf::as_path)
    }

    /// Whether the file is evaluated as part of the project's layers.
    pub fn in_layers(&self, file: FileId) -> bool {
        self.layers
            .iter()
            .any(|l| l.documents.iter().any(|(_, f)| *f == file))
    }

    /// Whether the file is in a read-only layer, which gets no diagnostics
    /// or code actions.
    pub fn is_read_only(&self, file: FileId) -> bool {
        self.read_only.contains(&file)
    }

    /// The evaluation a file belongs to.
    pub fn evaluation_of(&self, file: FileId) -> Option<&Evaluation> {
        if self.in_layers(file) {
            return Some(&self.evaluation);
        }
        self.documents
            .iter()
            .find(|(f, _)| *f == file)
            .map(|(_, e)| e)
    }

    /// Every diagnostic, as LSP diagnostics by file.
    pub(crate) fn diagnostics(&self, encoding: Encoding) -> Vec<(Uri, Vec<lsp_types::Diagnostic>)> {
        let mut by_file: HashMap<FileId, Vec<lsp_types::Diagnostic>> = HashMap::new();
        let all = self
            .evaluation
            .diagnostics
            .iter()
            .chain(self.documents.iter().flat_map(|(_, e)| &e.diagnostics))
            .chain(&self.extra);
        for diagnostic in all {
            if diagnostic.span.is_detached() || self.read_only.contains(&diagnostic.span.file) {
                continue;
            }
            let file = self.sources.file(diagnostic.span.file);
            let index = crol_syntax::LineIndex::new(file.text());
            let related = diagnostic
                .related
                .iter()
                .filter(|r| !r.span.is_detached())
                .filter_map(|r| {
                    let file = self.sources.file(r.span.file);
                    let index = crol_syntax::LineIndex::new(file.text());
                    Some(lsp_types::DiagnosticRelatedInformation {
                        location: lsp_types::Location::new(
                            convert::path_to_uri(self.path(r.span.file)?)?,
                            convert::range(file.text(), &index, r.span.range, encoding),
                        ),
                        message: r.message.clone(),
                    })
                })
                .collect::<Vec<_>>();
            let mut message = diagnostic.message.clone();
            if let Some(context) = &diagnostic.context {
                message.push_str(&format!("\n(while evaluating {context})"));
            }
            by_file
                .entry(diagnostic.span.file)
                .or_default()
                .push(lsp_types::Diagnostic {
                    range: convert::range(file.text(), &index, diagnostic.span.range, encoding),
                    severity: Some(match diagnostic.severity {
                        Severity::Error => lsp_types::DiagnosticSeverity::ERROR,
                        Severity::Warning => lsp_types::DiagnosticSeverity::WARNING,
                    }),
                    source: Some("crol".to_owned()),
                    message,
                    related_information: (!related.is_empty()).then_some(related),
                    ..Default::default()
                });
        }
        // Every file of a writable layer gets its diagnostics, even none, so
        // that fixed errors disappear.
        let mut out = Vec::new();
        for (id, _) in self.sources.files() {
            if self.read_only.contains(&id) {
                continue;
            }
            if let Some(uri) = self.path(id).and_then(convert::path_to_uri) {
                out.push((uri, by_file.remove(&id).unwrap_or_default()));
            }
        }
        out
    }
}

/// Read-only layers in their binary form (`SPEC.md` 7.8), by name, kept
/// between evaluations: they are the bulk of a large project, and do not
/// change as a document is edited, so they need not be parsed each time. They
/// get no diagnostics, which is all that is lost.
#[derive(Default)]
pub(crate) struct Binaries {
    /// What lowering a layer gave, with what identifies its documents.
    layers: HashMap<String, ([u8; 32], Arc<[u8]>)>,
    /// Layers that have no binary form, as they use a template of another,
    /// by what identifies their documents.
    refused: HashSet<[u8; 32]>,
}

impl Binaries {
    /// For each of the snapshot's layers, what is kept of it, if that is
    /// made from just these documents.
    fn for_layers<'a>(&'a self, snapshot: &Snapshot) -> Vec<Option<&'a [u8]>> {
        let of_layer = |layer: &Layer| {
            let (hash, lowered) = self.layers.get(&layer.name)?;
            (*hash == hash_of(snapshot, layer)).then_some(&**lowered)
        };
        snapshot.layers.iter().map(of_layer).collect()
    }

    /// Encodes the snapshot's read-only layers that are not kept yet, from
    /// what [`Binaries::wanted`] said.
    pub(crate) fn keep(&mut self, wanted: Wanted) {
        for (layer, hash) in &wanted.layers {
            let bytes = crol_eval::encode_layer(&wanted.sources, layer);
            let read = bytes.ok().map(|b| crol_eval::binary::read_layer(&b));
            match read {
                Some(Ok(binary)) => {
                    self.layers
                        .insert(layer.name.clone(), (*hash, binary.lowered));
                }
                _ => {
                    self.refused.insert(*hash);
                }
            }
        }
    }

    /// What to encode of a snapshot, if anything, to do after it is sent.
    pub(crate) fn wanted(&self, snapshot: &Snapshot) -> Wanted {
        let layers = (snapshot.layers.iter())
            .filter(|layer| snapshot.is_read_only_layer(layer))
            .map(|layer| (layer, hash_of(snapshot, layer)))
            .filter(|(layer, hash)| {
                let kept = self.layers.get(&layer.name).map(|(kept, _)| kept);
                kept != Some(hash) && !self.refused.contains(hash)
            })
            .map(|(layer, hash)| (layer.clone(), hash))
            .collect();
        Wanted {
            sources: snapshot.sources.clone(),
            layers,
        }
    }
}

/// Layers to encode, with their text.
pub(crate) struct Wanted {
    sources: Sources,
    layers: Vec<(Layer, [u8; 32])>,
}

/// What identifies a layer's documents in its binary form.
fn hash_of(snapshot: &Snapshot, layer: &Layer) -> [u8; 32] {
    let documents = layer
        .documents
        .iter()
        .map(|(relative, file)| (relative.as_str(), snapshot.sources.file(*file).text()));
    crol_eval::binary::hash_documents(documents)
}

/// What loading found, besides the layers.
#[derive(Default)]
struct Loading {
    followed: Vec<Followed>,
    /// Problems to put on the files to blame.
    problems: Vec<Problem>,
    /// The lowest layer that may be wrong for what did not load, with what
    /// did not: the layers from that one up.
    gap: Option<(usize, String)>,
}

impl Loading {
    /// Notes what was left out of the directories read.
    fn left_out(&mut self, left_out: &LeftOut) {
        let mut strays = left_out.lock().unwrap_or_else(|e| e.into_inner());
        self.problems.append(&mut strays);
    }
}

/// Loads a `.crod` document that is in no layer as a layer of its own, on
/// top of what it extends, through the composer `crol check` uses, so that
/// what it reports is what `crol check` does (`SPEC.md` 4.1).
fn load_chain(
    snapshot: &mut Snapshot,
    path: &Path,
    root: &Path,
    open: &Buffers,
    project_file: Option<&Path>,
) -> Result<(Vec<Layer>, Loading), crol::Error> {
    let left_out = LeftOut::default();
    let file_loaders = BufferLoader::loaders(root, open, project_file, &left_out);
    let layer = file_loaders
        .resolve_path(path, root)
        .load(path.display().to_string())?;
    let mut composer = CrolComposer::with_cfg(CrolConfig::<String> {
        load_extends: true,
        root_path: root.to_path_buf(),
        file_loaders,
        ..Default::default()
    });
    let loads = composer.load_layer_tolerant(layer);
    let layers = snapshot.intern_layers(composer.layers());
    let mut loading = Loading {
        followed: snapshot.followed(loads, &layers),
        ..Default::default()
    };
    loading.left_out(&left_out);
    Ok((layers, loading))
}

/// What the attributes say of what was followed: why one could not be, and
/// the first error found among `diagnostics` in what each brought in. Those
/// are the only ones shown of an extended document (`SPEC.md` 4.1).
fn extends_problems(
    followed: &[Followed],
    diagnostics: &[Diagnostic],
    sources: &Sources,
) -> Vec<Diagnostic> {
    let mut found = Vec::new();
    for extends in followed {
        let span = crol_eval::Span::new(extends.file, extends.range);
        if let Some(error) = &extends.error {
            found.push(Diagnostic::error(span, format!("extends: {error}")));
        }
        let broken = diagnostics
            .iter()
            .find(|d| d.is_error() && extends.files.contains(&d.span.file));
        if let Some(diagnostic) = broken {
            let message = format!(
                "extends: `{}` has errors: {}",
                extends.written,
                diagnostic.display(sources)
            );
            found.push(Diagnostic::error(span, message));
        }
    }
    found
}

/// Makes warnings of the errors in `diagnostics` that `suspect` says may
/// only be there for what could not be loaded, which `why` says.
fn soften(diagnostics: &mut [Diagnostic], why: &str, suspect: impl Fn(FileId) -> bool) {
    for diagnostic in diagnostics {
        if diagnostic.is_error() && suspect(diagnostic.span.file) {
            diagnostic.severity = Severity::Warning;
            diagnostic.message.push_str(&format!(" ({why})"));
        }
    }
}

/// Keeps the diagnostics of `file`, those of the document on its own.
fn only_in(diagnostics: &mut Vec<Diagnostic>, file: FileId) {
    diagnostics.retain(|d| d.span.is_detached() || d.span.file == file);
}

/// Puts a problem on the file to blame, or the project file, so that the
/// editor shows it, and records the first on the snapshot.
fn report_problem(snapshot: &mut Snapshot, problem: Problem, project: Option<&Path>) {
    let blamed = problem.file.as_deref().or(project);
    if let Some(path) = blamed {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let text = std::fs::read_to_string(&canonical).unwrap_or_default();
        let file = snapshot.intern(&canonical.display().to_string(), &text);
        let span = crol_eval::Span::new(file, Default::default());
        snapshot
            .extra
            .push(Diagnostic::error(span, problem.message.clone()));
    }
    snapshot.problem.get_or_insert(problem.message);
}

/// Evaluates the workspace: the project found from `root`, and each open
/// document in no layer on its own; open buffers replace their files.
pub(crate) fn evaluate(
    root: Option<&Path>,
    settings: &Settings,
    open: &HashMap<PathBuf, String>,
    checks: Option<&crate::Checks>,
    remote: &RemoteStatus,
    binaries: &Binaries,
) -> Snapshot {
    let mut snapshot = Snapshot {
        sources: Sources::new(),
        layers: Vec::new(),
        options: DataOptions::default(),
        evaluation: Evaluation::default(),
        compiled: None,
        explain_plan: OnceLock::new(),
        root_docs: OnceLock::new(),
        has_docs: OnceLock::new(),
        last_good: None,
        last_good_documents: HashMap::new(),
        documents: Vec::new(),
        extra: Vec::new(),
        schema: None,
        root_type: None,
        own_schemas: HashMap::new(),
        project: None,
        chains: HashMap::new(),
        paths: Vec::new(),
        by_path: HashMap::new(),
        read_only: HashSet::new(),
        problem: None,
        remote: Vec::new(),
    };
    let canonical_open: Buffers = Arc::new(
        open.iter()
            .map(|(p, t)| (p.canonicalize().unwrap_or_else(|_| p.clone()), t.clone()))
            .collect(),
    );
    let mut problems = Vec::new();
    let project_path = match (&settings.project, root) {
        (Some(project), _) => Some(project.clone()),
        (None, Some(root)) => Project::find(root),
        (None, None) => None,
    };
    let mut check = CheckOptions::default();
    let mut loading = Loading::default();
    if let Some(project_path) = &project_path {
        let loaded = load_project(
            &mut snapshot,
            project_path,
            &canonical_open,
            remote,
            &mut loading,
        );
        match loaded {
            Ok((schema, root_type, options)) => {
                snapshot.schema = schema;
                snapshot.root_type = root_type;
                check = options;
            }
            Err(found) => problems.push(found),
        }
    }
    problems.append(&mut loading.problems);
    if !snapshot.layers.is_empty() {
        // A layer's `schema.crod` changes the schema (`SPEC.md` 10.12).
        if let Some(schema) = snapshot.schema.take() {
            let (schema, found) = schema.patched(&mut snapshot.sources, &snapshot.layers);
            snapshot.extra.extend(found);
            snapshot.schema = Some(schema);
        }
        // `&default` reads the project's schema (`SPEC.md` 6.1.3).
        snapshot.options = snapshot.options.clone().with_defaults(
            snapshot
                .schema
                .as_ref()
                .map(|s| s.defaults_for(snapshot.root_type.as_deref())),
        );
        let lowered = binaries.for_layers(&snapshot);
        let (mut compiled, mut diagnostics) =
            crol_eval::compile_binary(&snapshot.sources, &snapshot.layers, &lowered);
        // What is wrong may be what was kept, and it would not show: there
        // are no diagnostics in a read-only layer.
        if compiled.is_none() && lowered.iter().any(Option::is_some) {
            (compiled, diagnostics) = crol_eval::compile(&snapshot.sources, &snapshot.layers);
        }
        snapshot.evaluation = match &compiled {
            Some(compiled) => {
                let mut evaluation = crol_eval::evaluate_compiled(compiled, &snapshot.options);
                diagnostics.append(&mut evaluation.diagnostics);
                evaluation.diagnostics = diagnostics;
                evaluation
            }
            None => Evaluation {
                diagnostics,
                ..Evaluation::default()
            },
        };
        snapshot.compiled = compiled;
        // What does not evaluate above a gap says little: keep what is below
        // it, until there is better.
        if let Some((gap, _)) = &loading.gap
            && snapshot.evaluation.value.is_none()
        {
            let below = &snapshot.layers[..*gap];
            let evaluation = crol_eval::evaluate_data(&snapshot.sources, below, &snapshot.options);
            snapshot.last_good = evaluation.value.is_some().then_some(evaluation);
        }
        if let (Some(schema), Some(base)) = (&snapshot.schema, &snapshot.evaluation.value) {
            let view = crol::DataView {
                base,
                contexts: &snapshot.evaluation.contexts,
                variants: &snapshot.evaluation.variants,
            };
            let mut found = crol::check_view(view, schema, snapshot.root_type.as_deref(), &check);
            // What is wrong with the root as a whole is on the top document
            // (`SPEC.md` 4.1). A read-only one gets no diagnostics, so it
            // goes on the top one that does.
            let writable = (snapshot.layers.iter().rev())
                .flat_map(|layer| layer.documents.iter().rev())
                .map(|(_, file)| *file)
                .find(|file| !snapshot.read_only.contains(file));
            for diagnostic in &mut found {
                if let Some(file) = writable
                    && diagnostic.span.range.is_empty()
                    && snapshot.read_only.contains(&diagnostic.span.file)
                {
                    diagnostic.span.file = file;
                }
            }
            snapshot.extra.extend(found);
        }
        // Above a gap, an error may only say what is missing.
        if let Some((gap, why)) = &loading.gap {
            let above: HashSet<FileId> = snapshot.layers[*gap..]
                .iter()
                .flat_map(|l| l.documents.iter().map(|(_, file)| *file))
                .collect();
            let why = format!("{why}, which may be why");
            let suspect = |file| above.contains(&file);
            soften(&mut snapshot.evaluation.diagnostics, &why, suspect);
            soften(&mut snapshot.extra, &why, suspect);
        }
        let found = extends_problems(
            &loading.followed,
            &snapshot.evaluation.diagnostics,
            &snapshot.sources,
        );
        snapshot.extra.extend(found);
        // What is extended and is no lower layer goes below this one all
        // the same, as in `crol check`, where the project's layers should
        // have said so (`SPEC.md` 4.1).
        for extends in loading.followed.iter().filter(|e| !e.files.is_empty()) {
            let span = crol_eval::Span::new(extends.file, extends.range);
            snapshot.extra.push(Diagnostic::warning(
                span,
                format!(
                    "extends: `{}` is not a lower layer of the project; add it to the project's layers",
                    extends.written
                ),
            ));
        }
    }
    // Open documents that are in no layer are evaluated on their own, each
    // time in one order.
    let mut open_files: Vec<(&PathBuf, &String)> = canonical_open.iter().collect();
    open_files.sort();
    for (path, text) in open_files {
        if snapshot
            .file_of(path)
            .is_some_and(|f| snapshot.in_layers(f))
        {
            continue;
        }
        let file = snapshot.intern(&path.display().to_string(), text);
        let mode = Mode::of_path(path).unwrap_or(Mode::Value);
        // The project file has the schema `crol` provides (`SPEC.md` 12.4).
        let is_project_file = project_path
            .as_ref()
            .is_some_and(|p| p.canonicalize().unwrap_or_else(|_| p.clone()) == *path);
        // `::` starts from the project's directory, else the workspace
        // folder, else the document's own (`SPEC.md` 4.2).
        let root_dir = project_path
            .as_ref()
            .and_then(|p| p.parent())
            .or(root)
            .or(path.parent())
            .unwrap_or(Path::new("."));
        let mut loading = Loading::default();
        let mut chain = None;
        if mode == Mode::Data {
            let project_file = project_path.as_deref();
            match load_chain(&mut snapshot, path, root_dir, &canonical_open, project_file) {
                Ok((layers, found)) => {
                    chain = Some(layers);
                    loading = found;
                }
                Err(error) => {
                    let span = crol_eval::Span::new(file, Default::default());
                    snapshot
                        .extra
                        .push(Diagnostic::error(span, error.to_string()));
                }
            }
        }
        // What the project's `files` give a value document that names no
        // schema (`SPEC.md` 12.4).
        let mut given_type = None;
        let schema = if is_project_file {
            Some(Ok(crol::schema::build::<Project>()))
        } else {
            document_schema(text, path, root_dir, remote, &mut snapshot.remote)
                .or_else(|| {
                    extended_schema(
                        &mut snapshot,
                        chain.as_deref().unwrap_or_default(),
                        root_dir,
                        remote,
                    )
                })
                .or_else(|| {
                    let (schema, root) = snapshot.project_files(path, mode)?;
                    given_type = root;
                    Some(schema)
                })
        };
        let mut evaluation = match (&chain, mode) {
            (Some(layers), _) => {
                // `&default` reads the document's schema, not the project's
                // (`SPEC.md` 6.1.3).
                let own = schema.as_ref().and_then(|s| s.as_ref().ok());
                let options = snapshot
                    .options
                    .clone()
                    .with_defaults(own.map(SchemaFile::defaults));
                let evaluation = crol_eval::evaluate_data(&snapshot.sources, layers, &options);
                let chain = Chain {
                    layers: layers.clone(),
                    options,
                };
                snapshot.chains.insert(file, chain);
                evaluation
            }
            (None, Mode::Data) => Evaluation::default(),
            (None, Mode::Value) => crol_eval::evaluate_value(&snapshot.sources, file),
        };
        problems.append(&mut loading.problems);
        let followed = &loading.followed;
        let found = extends_problems(followed, &evaluation.diagnostics, &snapshot.sources);
        snapshot.extra.extend(found);
        only_in(&mut evaluation.diagnostics, file);
        // Without what could not be loaded, this document may be wrong
        // where it is not.
        let incomplete = loading.gap.is_some() || followed.iter().any(|e| e.error.is_some());
        let why = "what it extends did not all load, which may be why";
        if incomplete {
            soften(&mut evaluation.diagnostics, why, |_| true);
        }
        match schema {
            Some(Ok(schema)) => {
                let by_project = given_type.is_some();
                let mut root = given_type;
                if let Some(value) = &evaluation.value {
                    match mode {
                        Mode::Data => {
                            let view = crol::DataView {
                                base: value,
                                contexts: &evaluation.contexts,
                                variants: &evaluation.variants,
                            };
                            let mut found = crol::check_view(view, &schema, None, &check);
                            only_in(&mut found, file);
                            if incomplete {
                                soften(&mut found, why, |_| true);
                            }
                            snapshot.extra.extend(found);
                        }
                        Mode::Value => {
                            // A top-level type name chooses the type,
                            // unless the project gives it one (`SPEC.md`
                            // 4.1, 12.4), whose options then count.
                            let plain = CheckOptions::default();
                            let options = if by_project { &check } else { &plain };
                            if !by_project {
                                root = crol::value_type(value, &schema);
                            }
                            snapshot.extra.extend(crol::check_value(
                                value,
                                &schema,
                                root.as_deref(),
                                options,
                            ));
                        }
                    }
                }
                let own = OwnSchema {
                    file: schema,
                    root,
                    mode,
                };
                snapshot.own_schemas.insert(file, own);
            }
            Some(Err((range, message))) => {
                snapshot.extra.push(Diagnostic::error(
                    crol_eval::Span::new(file, range),
                    message,
                ));
            }
            None => {}
        }
        snapshot.documents.push((file, evaluation));
    }
    for problem in problems {
        report_problem(&mut snapshot, problem, project_path.as_deref());
    }
    if let Some(checks) = checks {
        let found = checks(&snapshot);
        snapshot.extra.extend(found);
    }
    snapshot
}

/// The schema a document names with `#![schema(...)]`, read; an error at
/// the attribute if it cannot be read.
fn document_schema(
    text: &str,
    path: &Path,
    root: &Path,
    remote: &RemoteStatus,
    wanted: &mut Vec<(String, PathBuf)>,
) -> Option<Result<SchemaFile, (crol_syntax::TextRange, String)>> {
    // A schema file's schema is built in (`SPEC.md` 10.10).
    if crol::schema::is_schema_file(text) {
        return Some(Ok(crol::schema::build::<SchemaFile>()));
    }
    // One named by URL or crate comes from the cache; the server fetches it.
    let (range, schema_path) = crol::schema::attribute_path(text, path, root, false)?;
    let schema_path = match crol::schema::attribute_remote(text) {
        Some(source) => {
            let key = (
                source,
                path.parent().unwrap_or(Path::new(".")).to_path_buf(),
            );
            let status = remote.get(&key).cloned();
            wanted.push(key);
            schema_path.map_err(|e| status.unwrap_or_else(|| e.to_string()))
        }
        None => schema_path.map_err(|e| e.to_string()),
    };
    Some(
        schema_path
            .and_then(|p| SchemaFile::read(p).map_err(|e| e.to_string()))
            .map_err(|e| (range, e)),
    )
}

/// The schema the nearest extended document names, for a document that
/// names none, as `crol check` finds it among the layers (`SPEC.md` 4.1).
fn extended_schema(
    snapshot: &mut Snapshot,
    layers: &[Layer],
    root: &Path,
    remote: &RemoteStatus,
) -> Option<Result<SchemaFile, (crol_syntax::TextRange, String)>> {
    for layer in layers.iter().rev().skip(1) {
        for (_, file) in &layer.documents {
            let text = snapshot.sources.file(*file).text().to_owned();
            let path = snapshot.path(*file)?.to_path_buf();
            let found = document_schema(&text, &path, root, remote, &mut snapshot.remote);
            // Its problems are the extended document's.
            if let Some(Ok(schema)) = found {
                return Some(Ok(schema));
            }
        }
    }
    None
}

/// Loads the project's layers into the snapshot, through the composer `crol
/// check` uses, with what they extend; returns its schema, root type and
/// check options.
fn load_project(
    snapshot: &mut Snapshot,
    project_path: &Path,
    open: &Buffers,
    remote: &RemoteStatus,
    loading: &mut Loading,
) -> Result<(Option<SchemaFile>, Option<String>, CheckOptions), Problem> {
    let project = Project::read(project_path)?;
    let dir = project_path
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let mut options = DataOptions::default()
        .with_variants(project.variants().map_err(|e| e.to_string())?)
        .with_warn_cross_file_patches(project.options.warn_cross_file_patches)
        .with_init_defaults(project.options.init_defaults)
        .with_each_variant(project.options.each_variant)
        .with_max_contexts(project.options.max_contexts)
        .with_contexts(project.options.contexts.clone());
    options = options.with_variant_keys(project.options.variant_keys().map_err(|e| e.to_string())?);
    let left_out = LeftOut::default();
    let loaders = BufferLoader::loaders(&dir, open, Some(project_path), &left_out);
    let mut composer = CrolComposer::with_cfg(CrolConfig::<String> {
        load_extends: true,
        root_path: dir.clone(),
        file_loaders: loaders.clone(),
        ..Default::default()
    });
    let mut loads = Vec::new();
    // Each layer of the project, with where it is among the composer's.
    let mut listed = Vec::new();
    for layer in project.layer_list(&dir).map_err(|e| e.to_string())? {
        // A layer that does not load does not stop the ones after it. They
        // are evaluated without it, and may be wrong where they build on
        // it (`SPEC.md` 12.5).
        let index = composer.layers().len();
        let loaded = loaders
            .resolve(&layer.dir, &dir)
            .and_then(|source| source.load(layer.name.clone()));
        match loaded {
            Ok(loaded) => {
                let found = composer.load_layer_tolerant(loaded);
                let index = composer.layers().len() - 1;
                if found.iter().any(|load| load.error.is_some()) {
                    let why = format!("what layer `{}` extends did not all load", layer.name);
                    loading.gap.get_or_insert((index, why));
                }
                loads.extend(found);
                listed.push((index, layer.read_only));
            }
            Err(error) => {
                let found = Problem::from(error);
                let message = format!("layer `{}`: {}", layer.name, found.message);
                loading.problems.push(Problem { message, ..found });
                let why = format!("layer `{}` did not load", layer.name);
                loading.gap.get_or_insert((index, why));
            }
        }
        loading.left_out(&left_out);
    }
    snapshot.layers = snapshot.intern_layers(composer.layers());
    let layer_files = |snapshot: &Snapshot, index: usize| -> Vec<FileId> {
        let documents = &snapshot.layers[index].documents;
        documents.iter().map(|(_, file)| *file).collect()
    };
    // Extended documents get no diagnostics of their own (`SPEC.md` 4.1),
    // but for one the user is editing.
    let mut extended: HashSet<FileId> = snapshot
        .layers
        .iter()
        .flat_map(|l| l.documents.iter().map(|(_, file)| *file))
        .collect();
    for (index, read_only) in &listed {
        for file in layer_files(snapshot, *index) {
            extended.remove(&file);
            if *read_only {
                snapshot.read_only.insert(file);
            }
        }
    }
    extended.retain(|file| {
        snapshot
            .path(*file)
            .is_none_or(|path| !open.contains_key(path))
    });
    snapshot.read_only.extend(extended);
    let mut found = snapshot.followed(loads, &snapshot.layers.clone());
    for extends in &mut found {
        // Only what is no layer of the project is brought in by `extends`.
        extends.files.retain(|file| {
            !listed.iter().any(|(index, _)| {
                snapshot.layers[*index]
                    .documents
                    .iter()
                    .any(|d| d.1 == *file)
            })
        });
    }
    loading.followed.append(&mut found);
    snapshot.options = options;
    let schema_path = project.schema_path(&dir, false);
    let schema_path = match project.schema.clone() {
        Some(source) if source.starts_with("https://") || source.starts_with("crate:") => {
            let key = (source, dir.clone());
            let status = remote.get(&key).cloned();
            snapshot.remote.push(key);
            schema_path.map_err(|e| status.unwrap_or_else(|| e.to_string()))
        }
        _ => schema_path.map_err(|e| e.to_string()),
    };
    let schema_path = schema_path.map_err(Problem::from)?;
    // Every document in a layer should agree with the project's schema.
    if let Some(schema_path) = &schema_path {
        let files: Vec<FileId> = snapshot
            .layers
            .iter()
            .flat_map(|l| l.documents.iter().map(|(_, file)| *file))
            .collect();
        for file in files {
            let Some(path) = snapshot.path(file).map(Path::to_path_buf) else {
                continue;
            };
            let text = snapshot.sources.file(file).text();
            let remote = crol::schema::attribute_remote(text);
            let disagrees = crol::schema::attribute_disagrees(
                text,
                &path,
                &dir,
                schema_path,
                "the project's",
                false,
            );
            if let Some(source) = remote {
                let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
                snapshot.remote.push((source, dir));
            }
            if let Some((range, message)) = disagrees {
                let span = crol_eval::Span::new(file, range);
                snapshot.extra.push(Diagnostic::warning(span, message));
            }
        }
    }
    let schema = match schema_path {
        Some(path) => Some(SchemaFile::read(&path)?),
        None => None,
    };
    let mut check = CheckOptions::default();
    check.implicit_some = project.options.implicit_some;
    check.allow_unknown_fields = project.options.allow_unknown_fields;
    check.require_complete = project.options.require_complete;
    let root = project.root.clone();
    snapshot.project = Some((project, dir));
    Ok((schema, root, check))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project of one layer directory, with `web.crod` on disk.
    fn project(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("crol-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::write(
            root.join("project.crol"),
            "Project { layers: List([{ name: \"base\", dir: \"data\", .. }]), .. }",
        )
        .unwrap();
        std::fs::write(root.join("data/web.crod"), "+port: 80,").unwrap();
        root
    }

    #[test]
    fn an_open_buffer_replaces_its_file_in_a_layer() {
        let root = project("buffers");
        let settings = Settings::default();
        let remote = RemoteStatus::new();
        let open = HashMap::from([(root.join("data/web.crod"), "+port: 81,".to_owned())]);
        let snapshot = evaluate(
            Some(&root),
            &settings,
            &open,
            None,
            &remote,
            &Binaries::default(),
        );
        assert_eq!(snapshot.problem, None);
        assert_eq!(snapshot.layers.len(), 1);
        let file = snapshot.file_of(&root.join("data/web.crod")).unwrap();
        assert_eq!(snapshot.sources.file(file).text(), "+port: 81,");
        assert!(snapshot.in_layers(file));

        // Closed, the file is read again.
        let snapshot = evaluate(
            Some(&root),
            &settings,
            &HashMap::new(),
            None,
            &remote,
            &Binaries::default(),
        );
        let file = snapshot.file_of(&root.join("data/web.crod")).unwrap();
        assert_eq!(snapshot.sources.file(file).text(), "+port: 80,");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Every error found in `path` when it is opened with `text` next to
    /// the files of a workspace.
    fn errors(root: &Path, path: &Path, text: &str) -> Vec<String> {
        let open = HashMap::from([(path.to_path_buf(), text.to_owned())]);
        let snapshot = evaluate(
            Some(root),
            &Settings::default(),
            &open,
            None,
            &RemoteStatus::new(),
            &Binaries::default(),
        );
        let file = snapshot.file_of(path).unwrap();
        let evaluation = snapshot.evaluation_of(file).unwrap();
        evaluation
            .diagnostics
            .iter()
            .chain(&snapshot.extra)
            .filter(|d| d.is_error())
            .map(|d| d.message.clone())
            .collect()
    }

    /// An empty directory.
    fn directory(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("crol-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    const DATA: &str = "+port: 80\nport: & + 1\n<prod> { port: 443 }\n";
    const VALUE: &str = "#![type(Config)]\n\nport: 80\n..\n";

    #[test]
    fn the_extension_chooses_the_mode_of_a_document_on_its_own() {
        let root = directory("alone");
        assert_eq!(
            errors(&root, &root.join("a.crod"), DATA),
            Vec::<String>::new()
        );
        assert_eq!(
            errors(&root, &root.join("a.crol"), VALUE),
            Vec::<String>::new()
        );
        assert!(!errors(&root, &root.join("a.crol"), DATA).is_empty());
        assert!(!errors(&root, &root.join("a.crod"), VALUE).is_empty());
        // Nothing to go by: a document on its own is value mode.
        assert_eq!(
            errors(&root, &root.join("a.txt"), VALUE),
            Vec::<String>::new()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_extension_chooses_the_mode_outside_the_layers_of_a_project() {
        let root = project("outside");
        assert_eq!(
            errors(&root, &root.join("other.crod"), DATA),
            Vec::<String>::new()
        );
        assert_eq!(
            errors(&root, &root.join("other.crol"), VALUE),
            Vec::<String>::new()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_document_alone_is_evaluated_in_every_context() {
        let root = directory("alone-contexts");
        let path = root.join("a.crod");
        let open = HashMap::from([(path.clone(), DATA.to_owned())]);
        let snapshot = evaluate(
            Some(&root),
            &Settings::default(),
            &open,
            None,
            &RemoteStatus::new(),
            &Binaries::default(),
        );
        let file = snapshot.file_of(&path).unwrap();
        let evaluation = snapshot.evaluation_of(file).unwrap();
        assert!(evaluation.value.is_some());
        assert!(evaluation.variants.id("prod").is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_problem_with_the_project_lands_on_the_file_to_blame() {
        let root = project("stray");
        std::fs::write(root.join("data/stray.crol"), "1").unwrap();
        let snapshot = evaluate(
            Some(&root),
            &Settings::default(),
            &HashMap::new(),
            None,
            &RemoteStatus::new(),
            &Binaries::default(),
        );
        assert!(snapshot.problem.is_some());
        let file = snapshot.file_of(&root.join("data/stray.crol")).unwrap();
        assert!(snapshot.extra.iter().any(|d| d.span.file == file));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_project_file_may_lie_in_a_layer() {
        let root = directory("project-in-layer");
        std::fs::write(
            root.join("project.crol"),
            "Project { layers: List([{ name: \"base\", dir: \"data\", .. }]), .. }",
        )
        .unwrap();
        let data = root.join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("web.crod"), "+port: 80,").unwrap();
        std::fs::write(
            data.join("project.crol"),
            "Project { layers: List([{ name: \"base\", dir: \"sub\", .. }]), .. }",
        )
        .unwrap();
        std::fs::create_dir_all(data.join("sub")).unwrap();
        std::fs::write(data.join("sub/web.crod"), "+port: 80,").unwrap();
        // Found from `data`, the file there is the project file.
        let snapshot = evaluate(
            Some(&data),
            &Settings::default(),
            &HashMap::new(),
            None,
            &RemoteStatus::new(),
            &Binaries::default(),
        );
        assert_eq!(snapshot.problem, None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
