//! What the server knows: the latest evaluation of the project, or of each
//! open document on its own (`SPEC.md` 12.5).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crol::project::Project;
use crol::schema::SchemaFile;
use crol::{
    CheckOptions, CrolComposer, CrolConfig, CrolLayer, ExtendsLoad, FileLoader, FileLoaders,
    FsLoader, Loaded, LoaderError, Mode, ModulePath, SourceId,
};
use crol_eval::{DataOptions, Diagnostic, Evaluation, FileId, Layer, Severity, Sources};
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
type LeftOut = Arc<Mutex<Vec<Stray>>>;

/// A file left out of a directory.
struct Stray {
    problem: Problem,
    /// Whether it may be data that is now missing, as a `.crol` file that
    /// was not renamed. A `.crov` file never was any.
    lost_data: bool,
}

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
                    let lost_data = crol::is_retired(&entry.to_string());
                    let problem = Problem {
                        message: error.to_string(),
                        file: entry.as_path().map(Path::to_path_buf),
                    };
                    // A directory may be read twice: as a layer, and for an
                    // `extends`.
                    if left_out.iter().all(|s| s.problem.file != problem.file) {
                        left_out.push(Stray { problem, lost_data });
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
    /// While the layers do not evaluate (as while a document is being
    /// typed): the latest evaluation of them that did, for completion.
    pub last_good: Option<Evaluation>,
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

    /// The layers and options a data document was evaluated with: the
    /// project's for a file in its layers, else its own (`SPEC.md` 4.1).
    /// Nothing for a document that is not data.
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
    /// Notes what was left out of the directories read while `layer` was
    /// loaded.
    fn left_out(&mut self, left_out: &LeftOut, layer: usize) {
        let mut strays = left_out.lock().unwrap_or_else(|e| e.into_inner());
        for stray in strays.drain(..) {
            if stray.lost_data {
                let why = "a file of this layer or a lower one was left out";
                self.gap.get_or_insert((layer, why.to_owned()));
            }
            self.problems.push(stray.problem);
        }
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
    loading.left_out(&left_out, 0);
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
) -> Snapshot {
    let mut snapshot = Snapshot {
        sources: Sources::new(),
        layers: Vec::new(),
        options: DataOptions::default(),
        evaluation: Evaluation::default(),
        last_good: None,
        documents: Vec::new(),
        extra: Vec::new(),
        schema: None,
        root_type: None,
        own_schemas: HashMap::new(),
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
        (None, Some(root)) => Project::find(root).unwrap_or_else(|error| {
            problems.push(Problem::from(error));
            None
        }),
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
        // `&default` reads the project's schema (`SPEC.md` 6.1.3).
        snapshot.options = snapshot
            .options
            .clone()
            .with_defaults(snapshot.schema.as_ref().map(SchemaFile::defaults));
        snapshot.evaluation =
            crol_eval::evaluate_data(&snapshot.sources, &snapshot.layers, &snapshot.options);
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
            let found = crol::check_view(view, schema, snapshot.root_type.as_deref(), &check);
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
        // A retired file is not evaluated: it states no mode (`SPEC.md` 4).
        if crol::is_retired(&path.to_string_lossy()) {
            let message = "the `.crol` extension is retired; \
                           rename it to `.crod` if it is data, or to `.crov` if it is a value document";
            let span = crol_eval::Span::new(file, Default::default());
            snapshot.extra.push(Diagnostic::error(span, message));
            snapshot.documents.push((file, Evaluation::default()));
            continue;
        }
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
        let schema = if is_project_file {
            Some(Ok(crol::schema::build::<Project>()))
        } else {
            document_schema(text, path, root_dir, remote, &mut snapshot.remote).or_else(|| {
                extended_schema(
                    &mut snapshot,
                    chain.as_deref().unwrap_or_default(),
                    root_dir,
                    remote,
                )
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
                let mut root = None;
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
                            // A top-level type name chooses the type
                            // (`SPEC.md` 4.1).
                            root = crol::value_type(value, &schema);
                            snapshot.extra.extend(crol::check_value(
                                value,
                                &schema,
                                root.as_deref(),
                                &CheckOptions::default(),
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
        loading.left_out(&left_out, index);
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
    Ok((schema, project.root.clone(), check))
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
            root.join("project.crov"),
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
        let snapshot = evaluate(Some(&root), &settings, &open, None, &remote);
        assert_eq!(snapshot.problem, None);
        assert_eq!(snapshot.layers.len(), 1);
        let file = snapshot.file_of(&root.join("data/web.crod")).unwrap();
        assert_eq!(snapshot.sources.file(file).text(), "+port: 81,");
        assert!(snapshot.in_layers(file));

        // Closed, the file is read again.
        let snapshot = evaluate(Some(&root), &settings, &HashMap::new(), None, &remote);
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
            errors(&root, &root.join("a.crov"), VALUE),
            Vec::<String>::new()
        );
        assert!(!errors(&root, &root.join("a.crov"), DATA).is_empty());
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
            errors(&root, &root.join("other.crov"), VALUE),
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
        std::fs::write(root.join("data/stray.crov"), "1").unwrap();
        let snapshot = evaluate(
            Some(&root),
            &Settings::default(),
            &HashMap::new(),
            None,
            &RemoteStatus::new(),
        );
        assert!(snapshot.problem.is_some());
        let file = snapshot.file_of(&root.join("data/stray.crov")).unwrap();
        assert!(snapshot.extra.iter().any(|d| d.span.file == file));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_project_file_may_lie_in_a_layer() {
        let root = directory("project-in-layer");
        std::fs::write(
            root.join("project.crov"),
            "Project { layers: List([{ name: \"base\", dir: \"data\", .. }]), .. }",
        )
        .unwrap();
        let data = root.join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("web.crod"), "+port: 80,").unwrap();
        std::fs::write(
            data.join("project.crov"),
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
        );
        assert_eq!(snapshot.problem, None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
