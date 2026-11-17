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
    Compiled, Constants, DataOptions, Diagnostic, Evaluation, ExplainPlan, ExplainStep, FileId,
    Layer, Severity, Sources,
};
use crol_syntax::ast::AstNode as _;
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

/// A layer stack of the project, evaluated (`SPEC.md` 12.4).
pub struct StackSnapshot {
    /// The stack's name.
    pub name: String,
    /// Its layers.
    pub layers: Vec<Layer>,
    /// The options the layers were evaluated with.
    pub options: DataOptions,
    /// The evaluation of the layers.
    pub evaluation: Evaluation,
    /// The layers compiled, for looking up one key at a time; `None` if
    /// they do not compile.
    compiled: Option<Compiled>,
    /// What looking up keys in `compiled` takes.
    explain_plan: OnceLock<Result<ExplainPlan, Vec<Diagnostic>>>,
    /// The doc comments of root keys in the layers.
    root_docs: OnceLock<HashMap<String, String>>,
    /// Whether any document of the layers has a doc comment.
    has_docs: OnceLock<bool>,
    /// While the layers do not evaluate (as while a document is being
    /// typed): the latest evaluation of them that did, for completion.
    pub last_good: Option<Evaluation>,
    /// The stack's schema, if it names one and it loaded.
    pub schema: Option<SchemaFile>,
    /// The type path of the stack's root type, with the schema.
    pub root_type: Option<String>,
    /// What checking against the schema takes of the stack's options.
    check: CheckOptions,
    /// The files of the layers the stack lists, not those `extends` brings in.
    listed: HashSet<FileId>,
}

impl StackSnapshot {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            layers: Vec::new(),
            options: DataOptions::default(),
            evaluation: Evaluation::default(),
            compiled: None,
            explain_plan: OnceLock::new(),
            root_docs: OnceLock::new(),
            has_docs: OnceLock::new(),
            last_good: None,
            schema: None,
            root_type: None,
            check: CheckOptions::default(),
            listed: HashSet::new(),
        }
    }

    /// Whether the file is in one of the stack's layers.
    pub fn holds(&self, file: FileId) -> bool {
        (self.layers.iter()).any(|l| l.documents.iter().any(|(_, f)| *f == file))
    }

    /// Whether any document of the layers has a doc comment.
    pub(crate) fn has_docs(&self, find: impl FnOnce() -> bool) -> bool {
        *self.has_docs.get_or_init(find)
    }

    /// The doc comments of root keys in the layers.
    pub(crate) fn root_docs(
        &self,
        find: impl FnOnce() -> HashMap<String, String>,
    ) -> &HashMap<String, String> {
        self.root_docs.get_or_init(find)
    }
}

/// The latest evaluation.
pub struct Snapshot {
    /// Every file read.
    pub sources: Sources,
    /// The project's layer stacks, each evaluated on its own; none without
    /// a project.
    pub stacks: Vec<StackSnapshot>,
    /// What stands for a stack where there is none.
    no_stack: StackSnapshot,
    /// What documents on their own are evaluated with of the project: its
    /// variants, and the options of its stack if it has only one.
    lone_options: DataOptions,
    /// What they are checked with: the options of its stack if it has only
    /// one.
    lone_check: CheckOptions,
    /// While a document on its own does not evaluate: the latest evaluation
    /// of it that did, by its path (ids do not outlive a snapshot).
    last_good_documents: HashMap<PathBuf, Evaluation>,
    /// Documents evaluated on their own, each in the mode its extension
    /// states (`SPEC.md` 4): a name that states none is value mode.
    pub documents: Vec<(FileId, Evaluation)>,
    /// Diagnostics from the schema and the application's checks.
    pub extra: Vec<Diagnostic>,
    /// The schemas of documents evaluated on their own, with the type of
    /// each one's value: the one it names, or the project file's.
    own_schemas: HashMap<FileId, OwnSchema>,
    /// The project, with its directory, for what its `files` say of a
    /// document (`SPEC.md` 12.4).
    project: Option<(Project, PathBuf)>,
    /// The project's stand-in values of the constants (`SPEC.md` 6.1.4,
    /// 12.4), which everything is evaluated with: a constant without one is
    /// a warning on each use, and what reads it fails quietly.
    constants: Constants,
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
    /// The stack whose layers hold the file, if any does: first one that
    /// lists it in its own layers, then one that `extends` brought it into.
    pub fn stack_of(&self, file: FileId) -> Option<&StackSnapshot> {
        (self
            .stacks
            .iter()
            .find(|stack| stack.listed.contains(&file)))
        .or_else(|| self.stacks.iter().find(|stack| stack.holds(file)))
    }

    /// The stack of a file, for a file in one; with no file, the first
    /// stack. What stands for none, else.
    pub(crate) fn stack_for(&self, file: Option<FileId>) -> &StackSnapshot {
        let stack = match file {
            Some(file) => self.stack_of(file),
            None => self.stacks.first(),
        };
        stack.unwrap_or(&self.no_stack)
    }

    /// The stack whose layers these are, if they are a stack's.
    pub(crate) fn stack_with_layers(&self, layers: &[Layer]) -> Option<&StackSnapshot> {
        (self.stacks.iter()).find(|stack| std::ptr::eq(layers, stack.layers.as_slice()))
    }

    /// The project's stand-in values of the constants (`SPEC.md` 12.4),
    /// which everything is evaluated with.
    pub(crate) fn constants(&self) -> &Constants {
        &self.constants
    }

    /// The schema of a file: its own, for a document on its own, else its
    /// stack's.
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
        let stack = self.stack_for(file);
        Some(DocumentSchema {
            file: stack.schema.as_ref()?,
            root: stack.root_type.as_deref(),
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
            _ => self.stack_for(file).last_good.as_ref(),
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
        for stack in &mut self.stacks {
            if stack.evaluation.value.is_some() {
                continue;
            }
            let Some(at) = previous.stacks.iter().position(|s| s.name == stack.name) else {
                continue;
            };
            let previous = previous.stacks.swap_remove(at);
            // Better than what is below a layer that did not load, if that
            // is all there is now.
            stack.last_good = match previous.evaluation.value {
                Some(_) => Some(previous.evaluation),
                None => stack.last_good.take().or(previous.last_good),
            };
        }
    }

    /// What the project's `files` say of a value document: the schema of
    /// the stack whose `files` match it, with the type to check it as and
    /// the stack's check options; nothing if no glob matches it (`SPEC.md`
    /// 12.4).
    #[allow(clippy::type_complexity)]
    fn project_files(
        &self,
        path: &Path,
        mode: Mode,
    ) -> Option<(
        Result<SchemaFile, (crol_syntax::TextRange, String)>,
        Option<String>,
        CheckOptions,
    )> {
        let (project, dir) = self.project.as_ref()?;
        if mode != Mode::Value {
            return None;
        }
        let (name, written, type_name) = project.file_stack(dir, path)?;
        let stack = self.stacks.iter().find(|s| s.name == name)?;
        let (of_stack, owner) = match project.stacks.len() {
            1 => (String::new(), "project".to_owned()),
            _ => (format!(" of the stack `{name}`"), format!("stack `{name}`")),
        };
        let failed = |why: String| {
            let message = format!(
                "project: `files`{of_stack} gives this document the type `{type_name}`, {why}"
            );
            Some((
                Err((Default::default(), message)),
                None,
                stack.check.clone(),
            ))
        };
        let Some(schema) = &stack.schema else {
            return failed(match written.schema {
                Some(_) => format!("and the {owner}'s schema did not load"),
                None => format!("and the {owner} has no schema"),
            });
        };
        match schema.type_path(type_name) {
            Some(root) => Some((
                Ok(schema.clone()),
                Some(root.to_owned()),
                stack.check.clone(),
            )),
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
        let Some(stack) = self.stack_with_layers(layers) else {
            return crol_eval::explain(&self.sources, layers, options, path, context);
        };
        if stack.evaluation.has_errors() {
            return Err(stack.evaluation.diagnostics.clone());
        }
        let Some(compiled) = &stack.compiled else {
            return Err(Vec::new());
        };
        let plan = stack
            .explain_plan
            .get_or_init(|| crol_eval::explain_plan(compiled, &stack.evaluation.variants, options));
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

    /// The evaluated data a file is in: its stack's for a file in a stack's
    /// layers, else the document's own.
    pub(crate) fn data_of(&self, file: FileId) -> Option<&crol_eval::Value> {
        if let Some(stack) = self.stack_of(file) {
            return stack.evaluation.value.as_ref();
        }
        let (_, evaluation) = self.documents.iter().find(|(f, _)| *f == file)?;
        evaluation.value.as_ref()
    }

    /// Every id in the data a file is evaluated in, and what it names
    /// (`SPEC.md` 10.5): the project's data for a file in its layers, else
    /// the document's own.
    pub(crate) fn resolved_ids(&self, file: FileId) -> Vec<crol::ResolvedId> {
        self.id_resolver(file)
            .map(|resolver| resolver.ids())
            .unwrap_or_default()
    }

    /// What the ids of the data a file is in resolve against.
    pub(crate) fn id_resolver(&self, file: FileId) -> Option<crol::IdResolver<'_>> {
        Some(self.id_resolver_in(file, self.data_of(file)?))
    }

    /// [`id_resolver`](Self::id_resolver) for the data `root`: that of the
    /// last evaluation that gave any, of a context, or that data after a
    /// change.
    pub(crate) fn id_resolver_in<'a>(
        &'a self,
        file: FileId,
        root: &'a crol_eval::Value,
    ) -> crol::IdResolver<'a> {
        let mut resolver = crol::IdResolver::new(root);
        // An id names a root key of its own stack only (`SPEC.md` 12.4).
        if let Some(stack) = self.stack_of(file)
            && let Some(schema) = &stack.schema
        {
            resolver = resolver.schema(schema);
            if let Some(root) = &stack.root_type {
                resolver = resolver.root_type(root);
            }
        }
        resolver
    }

    /// The keys an id written at `span` could name, in each place it is met:
    /// those of the scopes around it, innermost first.
    pub(crate) fn visible_keys(&self, span: crol_eval::Span) -> Vec<String> {
        let Some(resolver) = self.id_resolver(span.file) else {
            return Vec::new();
        };
        let mut out: Vec<String> = Vec::new();
        for id in resolver.ids().into_iter().filter(|id| id.span == span) {
            for key in resolver.visible(&id.location) {
                if !out.contains(&key) {
                    out.push(key);
                }
            }
        }
        out
    }

    pub(crate) fn chain_of(&self, file: FileId) -> Option<(&[Layer], &DataOptions)> {
        if let Some(stack) = self.stack_of(file) {
            return Some((&stack.layers, &stack.options));
        }
        let chain = self.chains.get(&file)?;
        Some((&chain.layers, &chain.options))
    }

    /// A file's path.
    pub fn path(&self, file: FileId) -> Option<&Path> {
        self.paths.get(file.index()).map(PathBuf::as_path)
    }

    /// Whether the file is evaluated as part of the layers of one of the
    /// project's stacks.
    pub fn in_layers(&self, file: FileId) -> bool {
        self.stack_of(file).is_some()
    }

    /// Whether the file is in a read-only layer, which gets no diagnostics
    /// or code actions.
    pub fn is_read_only(&self, file: FileId) -> bool {
        self.read_only.contains(&file)
    }

    /// The evaluation a file belongs to.
    pub fn evaluation_of(&self, file: FileId) -> Option<&Evaluation> {
        if let Some(stack) = self.stack_of(file) {
            return Some(&stack.evaluation);
        }
        self.documents
            .iter()
            .find(|(f, _)| *f == file)
            .map(|(_, e)| e)
    }

    /// Every diagnostic, as LSP diagnostics by file.
    pub(crate) fn diagnostics(&self, encoding: Encoding) -> Vec<(Uri, Vec<lsp_types::Diagnostic>)> {
        let mut by_file: HashMap<FileId, Vec<lsp_types::Diagnostic>> = HashMap::new();
        let all = (self.stacks.iter())
            .flat_map(|stack| &stack.evaluation.diagnostics)
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

/// A layer, by the names of its stack and of it.
type LayerKey = (String, String);

/// Read-only layers in their binary form (`SPEC.md` 7.8), by name, kept
/// between evaluations: they are the bulk of a large project, and do not
/// change as a document is edited, so they need not be parsed each time. They
/// get no diagnostics, which is all that is lost.
#[derive(Default)]
pub(crate) struct Binaries {
    /// What lowering a layer gave, with what identifies its documents, by
    /// the names of its stack and of it.
    layers: HashMap<LayerKey, ([u8; 32], Arc<[u8]>)>,
    /// Layers that have no binary form, as they use a template of another,
    /// by what identifies their documents.
    refused: HashSet<[u8; 32]>,
}

impl Binaries {
    /// For each of a stack's layers, what is kept of it, if that is made
    /// from just these documents.
    fn for_layers<'a>(
        &'a self,
        snapshot: &Snapshot,
        stack: &StackSnapshot,
    ) -> Vec<Option<&'a [u8]>> {
        let of_layer = |layer: &Layer| {
            let key = (stack.name.clone(), layer.name.clone());
            let (hash, lowered) = self.layers.get(&key)?;
            (*hash == hash_of(snapshot, layer)).then_some(&**lowered)
        };
        stack.layers.iter().map(of_layer).collect()
    }

    /// Encodes the snapshot's read-only layers that are not kept yet, from
    /// what [`Binaries::wanted`] said.
    pub(crate) fn keep(&mut self, wanted: Wanted) {
        for (key, layer, hash) in wanted.layers {
            let bytes = crol_eval::encode_layer(&wanted.sources, &layer);
            let read = bytes.ok().map(|b| crol_eval::binary::read_layer(&b));
            match read {
                Some(Ok(binary)) => {
                    self.layers.insert(key, (hash, binary.lowered));
                }
                _ => {
                    self.refused.insert(hash);
                }
            }
        }
    }

    /// What to encode of a snapshot, if anything, to do after it is sent.
    pub(crate) fn wanted(&self, snapshot: &Snapshot) -> Wanted {
        let layers = (snapshot.stacks.iter())
            .flat_map(|stack| stack.layers.iter().map(move |layer| (stack, layer)))
            .filter(|(_, layer)| snapshot.is_read_only_layer(layer))
            .map(|(stack, layer)| {
                let key = (stack.name.clone(), layer.name.clone());
                (key, layer, hash_of(snapshot, layer))
            })
            .filter(|(key, _, hash)| {
                let kept = self.layers.get(key).map(|(kept, _)| kept);
                kept != Some(hash) && !self.refused.contains(hash)
            })
            .map(|(key, layer, hash)| (key, layer.clone(), hash))
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
    layers: Vec<(LayerKey, Layer, [u8; 32])>,
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
        stacks: Vec::new(),
        no_stack: StackSnapshot::new(""),
        lone_options: DataOptions::default().with_constants(editor_constants(Constants::new())),
        lone_check: CheckOptions::default(),
        last_good_documents: HashMap::new(),
        documents: Vec::new(),
        extra: Vec::new(),
        own_schemas: HashMap::new(),
        project: None,
        constants: editor_constants(Constants::new()),
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
    if let Some(project_path) = &project_path {
        match load_project(&mut snapshot, project_path, &canonical_open, remote) {
            // Each stack is evaluated and checked on its own (`SPEC.md`
            // 12.4).
            Ok(stacks) => {
                for (mut stack, mut loading) in stacks {
                    problems.append(&mut loading.problems);
                    if !stack.layers.is_empty() {
                        evaluate_stack(&mut snapshot, &mut stack, &loading, binaries);
                    }
                    snapshot.stacks.push(stack);
                }
            }
            Err(found) => problems.push(found),
        }
        stand_in_problems(&mut snapshot, project_path, &canonical_open);
    }
    let check = snapshot.lone_check.clone();
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
        let mut check = check.clone();
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
                    let (schema, root, options) = snapshot.project_files(path, mode)?;
                    given_type = root;
                    check = options;
                    Some(schema)
                })
        };
        let mut evaluation = match (&chain, mode) {
            (Some(layers), _) => {
                // `&default` reads the document's schema, not the project's
                // (`SPEC.md` 6.1.3).
                let own = schema.as_ref().and_then(|s| s.as_ref().ok());
                let options = snapshot
                    .lone_options
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
            (None, Mode::Value) => {
                crol_eval::evaluate_value_with(&snapshot.sources, file, &snapshot.constants)
            }
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
        // What ids name (`SPEC.md` 10.5), here without a schema; with one,
        // its check below reports them.
        if !matches!(schema, Some(Ok(_)))
            && let Some(value) = &evaluation.value
        {
            let mut found = match mode {
                Mode::Data => {
                    let view = crol::DataView {
                        base: value,
                        contexts: &evaluation.contexts,
                        variants: &evaluation.variants,
                    };
                    crol::check_ids(view, None, None)
                }
                Mode::Value => crol::IdResolver::new(value)
                    .ids()
                    .into_iter()
                    .filter_map(|id| {
                        id.problem
                            .map(|problem| crol_eval::Diagnostic::warning(id.span, problem))
                    })
                    .collect(),
            };
            only_in(&mut found, file);
            if incomplete {
                soften(&mut found, why, |_| true);
            }
            snapshot.extra.extend(found);
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
                // The document's `#[type(...)]` binds (`SPEC.md` 6.1.2).
                let document = crol_syntax::parse(text).document();
                let found = crate::constants::undeclared(file, document.syntax(), &schema);
                crate::constants::drop_absent(&mut evaluation.diagnostics, &found);
                snapshot.extra.extend(found);
                let found = crol::typed::check_document(file, &document, &schema, &check);
                snapshot.extra.extend(found);
                // Where typed templates are used: of every layer below and
                // at this document, in data.
                let others: Vec<(FileId, crol_syntax::ast::Document)> = chain
                    .iter()
                    .flatten()
                    .flat_map(|layer| layer.documents.iter().map(|(_, f)| *f))
                    .filter(|f| *f != file)
                    .filter_map(|f| {
                        let source = snapshot.sources.get(f)?;
                        Some((f, crol_syntax::parse(source.text()).document()))
                    })
                    .collect();
                let templates = crol::typed::typed_templates(
                    others
                        .iter()
                        .map(|(f, d)| (*f, d))
                        .chain([(file, &document)]),
                    &schema,
                );
                let type_root = match mode {
                    Mode::Value => {
                        crol::typed::TypeRoot::Value(root.as_deref().unwrap_or(&schema.root))
                    }
                    Mode::Data => crol::typed::TypeRoot::Data {
                        root: root.as_deref(),
                        value: evaluation.value.as_ref(),
                    },
                };
                let found = crol::typed::check_template_uses(
                    file, &document, &schema, &templates, type_root,
                );
                snapshot.extra.extend(found);
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

/// Checks the project's stand-in values of the constants against what the
/// schemas of its stacks declare, on the project file (`SPEC.md` 12.4).
fn stand_in_problems(snapshot: &mut Snapshot, project_path: &Path, open: &Buffers) {
    let declares = |stack: &StackSnapshot| {
        stack
            .schema
            .as_ref()
            .is_some_and(|s| !s.constants.is_empty())
    };
    if !snapshot.stacks.iter().any(declares) || snapshot.constants.names().next().is_none() {
        return;
    }
    let canonical = (project_path.canonicalize()).unwrap_or_else(|_| project_path.to_path_buf());
    let text = match open.get(&canonical) {
        Some(text) => text.clone(),
        None => std::fs::read_to_string(&canonical).unwrap_or_default(),
    };
    let file = snapshot.intern(&canonical.display().to_string(), &text);
    let mut found: Vec<Diagnostic> = Vec::new();
    for schema in snapshot
        .stacks
        .iter()
        .filter_map(|stack| stack.schema.as_ref())
    {
        for problem in crate::constants::stand_in_problems(file, &text, &snapshot.constants, schema)
        {
            if !found.iter().any(|f| f.message == problem.message) {
                found.push(problem);
            }
        }
    }
    snapshot.extra.extend(found);
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

/// Evaluates a stack's layers, loaded, and checks them against its schema;
/// what that finds goes into the snapshot.
fn evaluate_stack(
    snapshot: &mut Snapshot,
    stack: &mut StackSnapshot,
    loading: &Loading,
    binaries: &Binaries,
) {
    // A layer's `schema.crod` changes the schema (`SPEC.md` 10.12).
    if let Some(schema) = stack.schema.take() {
        let (schema, found) = schema.patched(&mut snapshot.sources, &stack.layers);
        snapshot.extra.extend(found);
        stack.schema = Some(schema);
    }
    // `&default` reads the project's schema (`SPEC.md` 6.1.3).
    stack.options = stack.options.clone().with_defaults(
        stack
            .schema
            .as_ref()
            .map(|s| s.defaults_for(stack.root_type.as_deref())),
    );
    let lowered = binaries.for_layers(snapshot, stack);
    let (mut compiled, mut diagnostics) =
        crol_eval::compile_binary(&snapshot.sources, &stack.layers, &lowered);
    // What is wrong may be what was kept, and it would not show: there
    // are no diagnostics in a read-only layer.
    if compiled.is_none() && lowered.iter().any(Option::is_some) {
        (compiled, diagnostics) = crol_eval::compile(&snapshot.sources, &stack.layers);
    }
    stack.evaluation = match &compiled {
        Some(compiled) => {
            let mut evaluation = crol_eval::evaluate_compiled(compiled, &stack.options);
            diagnostics.append(&mut evaluation.diagnostics);
            evaluation.diagnostics = diagnostics;
            evaluation
        }
        None => Evaluation {
            diagnostics,
            ..Evaluation::default()
        },
    };
    stack.compiled = compiled;
    // What does not evaluate above a gap says little: keep what is below
    // it, until there is better.
    if let Some((gap, _)) = &loading.gap
        && stack.evaluation.value.is_none()
    {
        let below = &stack.layers[..*gap];
        let evaluation = crol_eval::evaluate_data(&snapshot.sources, below, &stack.options);
        stack.last_good = evaluation.value.is_some().then_some(evaluation);
    }
    if let (Some(schema), Some(base)) = (&stack.schema, &stack.evaluation.value) {
        let view = crol::DataView {
            base,
            contexts: &stack.evaluation.contexts,
            variants: &stack.evaluation.variants,
        };
        let mut found = crol::check_view(view, schema, stack.root_type.as_deref(), &stack.check);
        // What is wrong with the root as a whole is on the top document
        // (`SPEC.md` 4.1). A read-only one gets no diagnostics, so it
        // goes on the top one that does.
        let writable = (stack.layers.iter().rev())
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
    // `$name`s the schema does not declare (`SPEC.md` 6.1.4).
    if let Some(schema) = stack.schema.as_ref().filter(|s| !s.constants.is_empty()) {
        let files = (stack.layers.iter())
            .flat_map(|layer| layer.documents.iter().map(|(_, file)| *file))
            .filter(|file| !snapshot.read_only.contains(file));
        let mut found = Vec::new();
        for file in files {
            let text = snapshot.sources.file(file).text();
            if text.contains('$') {
                let root = crol_syntax::parse(text).syntax();
                found.extend(crate::constants::undeclared(file, &root, schema));
            }
        }
        crate::constants::drop_absent(&mut stack.evaluation.diagnostics, &found);
        snapshot.extra.extend(found);
    }
    // What ids name (`SPEC.md` 10.5): without a schema, those that name
    // nothing; with one, the check above reports those.
    if let Some(base) = &stack.evaluation.value {
        let view = crol::DataView {
            base,
            contexts: &stack.evaluation.contexts,
            variants: &stack.evaluation.variants,
        };
        let found = crol::check_ids(view, stack.schema.as_ref(), stack.root_type.as_deref());
        snapshot.extra.extend(found);
    }
    // Above a gap, an error may only say what is missing.
    if let Some((gap, why)) = &loading.gap {
        let above: HashSet<FileId> = stack.layers[*gap..]
            .iter()
            .flat_map(|l| l.documents.iter().map(|(_, file)| *file))
            .collect();
        let why = format!("{why}, which may be why");
        let suspect = |file| above.contains(&file);
        soften(&mut stack.evaluation.diagnostics, &why, suspect);
        soften(&mut snapshot.extra, &why, suspect);
    }
    let found = extends_problems(
        &loading.followed,
        &stack.evaluation.diagnostics,
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

/// Reads the project and loads the layers of each of its stacks into the
/// snapshot, each with what loading it found; a stack whose schema does
/// not load is evaluated without one.
fn load_project(
    snapshot: &mut Snapshot,
    project_path: &Path,
    open: &Buffers,
    remote: &RemoteStatus,
) -> Result<Vec<(StackSnapshot, Loading)>, Problem> {
    let project = Project::read(project_path)?;
    let dir = project_path
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let variants = project.variants().map_err(|e| e.to_string())?;
    snapshot.constants = editor_constants(project.constants()?);
    let constants = snapshot.constants.clone();
    let mut stacks = Vec::new();
    for (name, stack) in &project.stacks {
        let mut loading = Loading::default();
        let mut loaded = StackSnapshot::new(name);
        let options = (stack.options.variant_keys()).map(|keys| {
            data_options(&stack.options, variants.clone())
                .with_variant_keys(keys)
                .with_constants(constants.clone())
        });
        match options {
            Ok(options) => {
                let found = load_stack(
                    snapshot,
                    &mut loaded,
                    (project_path, &dir, &project),
                    stack,
                    options,
                    open,
                    remote,
                    &mut loading,
                );
                if let Err(problem) = found {
                    loading.problems.push(problem);
                }
            }
            Err(error) => loading.problems.push(error.into()),
        }
        stacks.push((loaded, loading));
    }
    // A document on its own is evaluated with the project's variants, and
    // with the options of its stack if it has only one.
    snapshot.lone_options = DataOptions::default()
        .with_variants(variants.clone())
        .with_constants(snapshot.constants.clone());
    if let [(stack, _)] = &stacks[..] {
        snapshot.lone_options = stack.options.clone();
        snapshot.lone_check = stack.check.clone();
    }
    snapshot.project = Some((project, dir));
    Ok(stacks)
}

/// What the editor evaluates with of the project's stand-in values of the
/// constants: a constant without one is a warning on each use, and what
/// reads it fails quietly, so that one does not flood a file with errors
/// (`SPEC.md` 6.1.4, 12.5).
fn editor_constants(constants: Constants) -> Constants {
    constants.with_absent_warning(true)
}

/// The data options a stack's options give, with the project's variants.
fn data_options(
    options: &crol::project::ProjectOptions,
    variants: crol_eval::Variants,
) -> DataOptions {
    DataOptions::default()
        .with_variants(variants)
        .with_init_defaults(options.init_defaults)
        .with_each_variant(options.each_variant)
        .with_max_contexts(options.max_contexts)
        .with_contexts(options.contexts.clone())
}

/// Loads a stack's layers into the snapshot, through the composer `crol
/// check` uses, with what they extend, and reads its schema.
#[allow(clippy::too_many_arguments)]
fn load_stack(
    snapshot: &mut Snapshot,
    loaded: &mut StackSnapshot,
    (project_path, dir, project): (&Path, &Path, &Project),
    stack: &crol::project::ProjectStack,
    options: DataOptions,
    open: &Buffers,
    remote: &RemoteStatus,
    loading: &mut Loading,
) -> Result<(), Problem> {
    let dir = dir.to_path_buf();
    let left_out = LeftOut::default();
    let loaders = BufferLoader::loaders(&dir, open, Some(project_path), &left_out);
    let mut composer = CrolComposer::with_cfg(CrolConfig::<String> {
        load_extends: true,
        root_path: dir.clone(),
        file_loaders: loaders.clone(),
        ..Default::default()
    });
    let mut loads = Vec::new();
    // Each layer of the stack, with where it is among the composer's.
    let mut listed = Vec::new();
    loaded.options = options;
    loaded.check = stack.options.check_options();
    loaded.root_type = stack.root.clone();
    for layer in stack.layer_list(&dir).map_err(|e| e.to_string())? {
        // A layer that does not load does not stop the ones after it. They
        // are evaluated without it, and may be wrong where they build on
        // it (`SPEC.md` 12.5).
        let index = composer.layers().len();
        let found = loaders
            .resolve(&layer.dir, &dir)
            .and_then(|source| source.load(layer.name.clone()));
        match found {
            Ok(found) => {
                let found = composer.load_layer_tolerant(found);
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
    loaded.layers = snapshot.intern_layers(composer.layers());
    let layers = &loaded.layers;
    let layer_files = |index: usize| -> Vec<FileId> {
        let documents = &layers[index].documents;
        documents.iter().map(|(_, file)| *file).collect()
    };
    // Extended documents get no diagnostics of their own (`SPEC.md` 4.1),
    // but for one the user is editing.
    let mut extended: HashSet<FileId> = layers
        .iter()
        .flat_map(|l| l.documents.iter().map(|(_, file)| *file))
        .collect();
    for (index, read_only) in &listed {
        for file in layer_files(*index) {
            extended.remove(&file);
            loaded.listed.insert(file);
            if *read_only {
                snapshot.read_only.insert(file);
            }
        }
    }
    // A file of another stack's layers, which it should not extend, gets
    // its diagnostics there.
    extended.retain(|file| {
        let Some(path) = snapshot.path(*file) else {
            return true;
        };
        let elsewhere = project.layer_stack(&dir, path).ok().flatten();
        !open.contains_key(path) && elsewhere.is_none_or(|other| other == loaded.name)
    });
    snapshot.read_only.extend(extended);
    // A stack extends only its own files (`SPEC.md` 12.4).
    for file in loaded.listed.iter().copied().collect::<Vec<_>>() {
        let Some(path) = snapshot.path(file).map(Path::to_path_buf) else {
            continue;
        };
        let text = snapshot.sources.file(file).text();
        for (range, message) in project.cross_stack_extends(&dir, &loaded.name, &path, text) {
            let span = crol_eval::Span::new(file, range);
            snapshot.extra.push(Diagnostic::error(span, message));
        }
    }
    let mut found = snapshot.followed(loads, layers);
    for extends in &mut found {
        // Only what is no layer of the stack is brought in by `extends`.
        extends.files.retain(|file| {
            !listed
                .iter()
                .any(|(index, _)| layers[*index].documents.iter().any(|d| d.1 == *file))
        });
    }
    loading.followed.append(&mut found);
    let schema_path = stack.schema_path(&dir, false);
    let schema_path = match stack.schema.clone() {
        Some(source) if source.starts_with("https://") || source.starts_with("crate:") => {
            let key = (source, dir.clone());
            let status = remote.get(&key).cloned();
            snapshot.remote.push(key);
            schema_path.map_err(|e| status.unwrap_or_else(|| e.to_string()))
        }
        _ => schema_path.map_err(|e| e.to_string()),
    };
    let schema_path = schema_path.map_err(Problem::from)?;
    // Every document in a layer should agree with the stack's schema.
    if let Some(schema_path) = &schema_path {
        let files: Vec<FileId> = (loaded.layers.iter())
            .flat_map(|l| l.documents.iter().map(|(_, file)| *file))
            .collect();
        let used = match project.stacks.len() > 1 {
            true => format!("that of the stack `{}`", loaded.name),
            false => "the project's".to_owned(),
        };
        for file in files {
            let Some(path) = snapshot.path(file).map(Path::to_path_buf) else {
                continue;
            };
            let text = snapshot.sources.file(file).text();
            let remote = crol::schema::attribute_remote(text);
            let disagrees =
                crol::schema::attribute_disagrees(text, &path, &dir, schema_path, &used, false);
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
    loaded.schema = match schema_path {
        Some(path) => Some(SchemaFile::read(&path)?),
        None => None,
    };
    Ok(())
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
        assert_eq!(snapshot.stacks[0].layers.len(), 1);
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

    /// A schema whose root is a map of structs `name` with one field.
    fn one_field_schema(name: &str, field: &str, ty: &str) -> String {
        format!(
            r#"Schema {{
    format: 1,
    root: "app::{name}s",
    types: {{
        "app::{name}s": Transparent {{
            name: "{name}s",
            source: ("src/lib.rs", 2),
            inner: Map(String, Named("app::{name}")),
            ..
        }},
        "app::{name}": Struct {{
            name: "{name}",
            source: ("src/lib.rs", 1),
            fields: {{ {field}: {{ ty: {ty}, .. }} }},
            ..
        }},
    }},
}}"#
        )
    }

    #[test]
    fn an_extends_into_another_stack_is_an_error_there() {
        let root = directory("stacks-extends");
        let project = r#"Project {
    stacks: {
        services: { layers: List([{ name: "base", dir: "services", .. }]), schema: Some("service_schema"), .. },
        themes: { layers: List([{ name: "base", dir: "themes", .. }]), schema: Some("theme_schema"), .. },
    },
    ..
}"#;
        let files = [
            ("project.crol", project.to_owned()),
            (
                "service_schema.crol",
                one_field_schema("Service", "replicas", "U32"),
            ),
            (
                "theme_schema.crol",
                one_field_schema("Theme", "color", "String"),
            ),
            (
                "services/web.crod",
                "+web: Service { replicas: -1 }\n".to_owned(),
            ),
            (
                "themes/dark.crod",
                "#![extends(::services::web)]\n\n+dark: Theme { color: \"black\" }\n".to_owned(),
            ),
        ];
        for (path, text) in &files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let snapshot = evaluate(
            Some(&root),
            &Settings::default(),
            &HashMap::new(),
            None,
            &RemoteStatus::new(),
            &Binaries::default(),
        );
        let web = snapshot.file_of(&root.join("services/web.crod")).unwrap();
        let dark = snapshot.file_of(&root.join("themes/dark.crod")).unwrap();
        let on = |file| -> Vec<String> {
            (snapshot.stacks.iter())
                .flat_map(|s| &s.evaluation.diagnostics)
                .chain(&snapshot.extra)
                .filter(|d| d.is_error() && d.span.file == file)
                .map(|d| d.message.clone())
                .collect()
        };
        let wanted = "extends: `::services::web` is in the layers of the stack `services`; the layers of `themes` extend only files of their own stack";
        assert!(on(dark).iter().any(|m| m == wanted), "{:?}", on(dark));
        // The file it reaches into is still its own stack's, with its
        // diagnostics.
        assert_eq!(snapshot.stack_of(web).unwrap().name, "services");
        assert!(!snapshot.is_read_only(web));
        assert!(on(web).iter().any(|m| m.contains("-1")), "{:?}", on(web));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn each_stack_is_evaluated_against_its_own_schema() {
        let root = directory("stacks");
        let files = [
            (
                "project.crol",
                r#"Project {
    stacks: {
        services: {
            layers: List([{ name: "base", dir: "services", .. }]),
            schema: Some("service_schema"),
            ..
        },
        themes: {
            layers: List([{ name: "base", dir: "themes", .. }]),
            schema: Some("theme_schema"),
            files: [{ glob: "config::*", type: "Theme" }],
            ..
        },
    },
    ..
}"#
                .to_owned(),
            ),
            (
                "service_schema.crol",
                one_field_schema("Service", "replicas", "U32"),
            ),
            (
                "theme_schema.crol",
                one_field_schema("Theme", "color", "String"),
            ),
            (
                "services/web.crod",
                "+web: Service { replicas: 2 }\n".to_owned(),
            ),
            ("themes/dark.crod", "+dark: Theme { color: 3 }\n".to_owned()),
            ("config/light.crol", "Theme { color: 4 }\n".to_owned()),
        ];
        for (path, text) in &files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let light = root.join("config/light.crol").canonicalize().unwrap();
        let open = HashMap::from([(light.clone(), "Theme { color: 4 }\n".to_owned())]);
        let snapshot = evaluate(
            Some(&root),
            &Settings::default(),
            &open,
            None,
            &RemoteStatus::new(),
            &Binaries::default(),
        );
        assert_eq!(snapshot.problem, None);
        let names: Vec<&str> = snapshot.stacks.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["services", "themes"]);
        let web = snapshot.file_of(&root.join("services/web.crod")).unwrap();
        let dark = snapshot.file_of(&root.join("themes/dark.crod")).unwrap();
        let light = snapshot.file_of(&light).unwrap();
        assert_eq!(snapshot.stack_of(web).unwrap().name, "services");
        assert_eq!(snapshot.stack_of(dark).unwrap().name, "themes");
        assert_eq!(
            snapshot.schema_for(Some(web)).unwrap().file.root,
            "app::Services"
        );
        assert_eq!(
            snapshot.schema_for(Some(dark)).unwrap().file.root,
            "app::Themes"
        );
        // The data of each stack is its own.
        let keys = |file| {
            let crol_eval::ValueKind::Keyed(keyed) = &snapshot.data_of(file).unwrap().kind else {
                panic!()
            };
            keyed
                .entries
                .keys()
                .map(|k| k.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(keys(web), ["web"]);
        assert_eq!(keys(dark), ["dark"]);
        // Each is checked against its own schema; a value document against
        // the schema of the stack whose `files` match it.
        let errors: Vec<_> = (snapshot.stacks.iter())
            .flat_map(|s| &s.evaluation.diagnostics)
            .chain(&snapshot.extra)
            .filter(|d| d.is_error())
            .map(|d| d.span.file)
            .collect();
        assert_eq!(errors, [dark, light], "{:?}", snapshot.extra);
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
