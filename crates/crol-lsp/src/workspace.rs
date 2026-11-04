//! What the server knows: the latest evaluation of the project, or of each
//! open document on its own (`SPEC.md` 12.5).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crol::project::Project;
use crol::schema::SchemaFile;
use crol::{CheckOptions, CrolLayer};
use crol_eval::{DataOptions, Diagnostic, Evaluation, FileId, Layer, Severity, Sources};
use lsp_types::Uri;

use crate::Settings;
use crate::convert::{self, Encoding};

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
    /// Documents evaluated on their own, as value-mode documents.
    pub documents: Vec<(FileId, Evaluation)>,
    /// Diagnostics from the schema and the application's checks.
    pub extra: Vec<Diagnostic>,
    /// The project's schema, if it names one.
    pub schema: Option<SchemaFile>,
    /// The type path of the project's root type, with the schema.
    pub root_type: Option<String>,
    /// The path of each file, by its id.
    paths: Vec<PathBuf>,
    /// Files of read-only layers, which get no diagnostics.
    read_only: HashSet<FileId>,
    /// A message for the project as a whole, such as a project file that
    /// cannot be read.
    pub problem: Option<String>,
}

impl Snapshot {
    /// The file a path is, if it was read.
    pub fn file_of(&self, path: &Path) -> Option<FileId> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.paths
            .iter()
            .position(|p| *p == canonical)
            .and_then(|i| self.sources.files().nth(i).map(|(id, _)| id))
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

/// Evaluates the workspace: the project found from `root`, or each open
/// document on its own; open buffers replace their files.
pub(crate) fn evaluate(
    root: Option<&Path>,
    settings: &Settings,
    open: &HashMap<PathBuf, String>,
    checks: Option<&crate::Checks>,
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
        paths: Vec::new(),
        read_only: HashSet::new(),
        problem: None,
    };
    let canonical_open: HashMap<PathBuf, &String> = open
        .iter()
        .map(|(p, t)| (p.canonicalize().unwrap_or_else(|_| p.clone()), t))
        .collect();
    let project_path = settings
        .project
        .clone()
        .or_else(|| root.and_then(Project::find));
    let mut check = CheckOptions::default();
    if let Some(project_path) = &project_path {
        match load_project(
            &mut snapshot,
            project_path,
            &canonical_open,
            settings.remote_schemas,
        ) {
            Ok((schema, root_type, options)) => {
                snapshot.schema = schema;
                snapshot.root_type = root_type;
                check = options;
            }
            Err(message) => snapshot.problem = Some(message),
        }
    }
    if !snapshot.layers.is_empty() {
        snapshot.evaluation =
            crol_eval::evaluate_data(&snapshot.sources, &snapshot.layers, &snapshot.options);
        if let (Some(schema), Some(base)) = (&snapshot.schema, &snapshot.evaluation.value) {
            let view = crol::DataView {
                base,
                contexts: &snapshot.evaluation.contexts,
                variants: &snapshot.evaluation.variants,
            };
            snapshot.extra = crol::check_view(view, schema, snapshot.root_type.as_deref(), &check);
        }
    }
    // Open documents that are in no layer are evaluated on their own.
    for (path, text) in &canonical_open {
        if snapshot.paths.contains(path) {
            continue;
        }
        let file = snapshot
            .sources
            .add(path.display().to_string(), text.as_str());
        snapshot.paths.push(path.clone());
        let evaluation = crol_eval::evaluate_value(&snapshot.sources, file);
        // The project file has the schema `crol` provides (`SPEC.md` 12.4).
        let is_project_file = project_path
            .as_ref()
            .is_some_and(|p| p.canonicalize().unwrap_or_else(|_| p.clone()) == *path);
        let schema = if is_project_file {
            Some(Ok(crol::schema::build::<Project>()))
        } else {
            document_schema(text, path, settings.remote_schemas)
        };
        match schema {
            Some(Ok(schema)) => {
                if let Some(value) = &evaluation.value {
                    // A top-level type name chooses the type (`SPEC.md` 4.1).
                    let named = match &value.kind {
                        crol_eval::ValueKind::Keyed(k) => k.name.as_deref(),
                        crol_eval::ValueKind::NamedTuple(n, _)
                        | crol_eval::ValueKind::UnitName(n) => Some(n.as_str()),
                        _ => None,
                    };
                    let root = named.and_then(|n| {
                        let mut found = schema.by_name(n).map(|(path, _)| path.to_owned());
                        found.next().filter(|_| found.next().is_none())
                    });
                    snapshot.extra.extend(crol::check_value(
                        value,
                        &schema,
                        root.as_deref(),
                        &CheckOptions::default(),
                    ));
                }
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
    fetch: bool,
) -> Option<Result<SchemaFile, (crol_syntax::TextRange, String)>> {
    let (range, schema_path) = crol::schema::attribute_path(text, path, fetch)?;
    Some(
        schema_path
            .and_then(SchemaFile::read)
            .map_err(|e| (range, e.to_string())),
    )
}

/// Loads the project's layers into the snapshot; returns its schema, root
/// type and check options.
fn load_project(
    snapshot: &mut Snapshot,
    project_path: &Path,
    open: &HashMap<PathBuf, &String>,
    fetch: bool,
) -> Result<(Option<SchemaFile>, Option<String>, CheckOptions), String> {
    let project = Project::read(project_path).map_err(|e| e.to_string())?;
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
    for layer in project.layer_list(&dir).map_err(|e| e.to_string())? {
        let path = crol::resolve_module_path(&layer.dir, &dir, None)
            .map_err(|e| format!("layer `{}`: {e}", layer.name))?;
        let loaded = CrolLayer::from_path(layer.name.clone(), &path)
            .map_err(|e| e.to_string())?
            .without_file(project_path);
        let mut documents = Vec::new();
        for (relative, display, text) in loaded.documents() {
            let path = PathBuf::from(display);
            let canonical = path.canonicalize().unwrap_or(path);
            // An open buffer replaces its file, saved or not.
            let text = open.get(&canonical).map_or(text, |t| t.as_str());
            let file = snapshot.sources.add(display, text);
            snapshot.paths.push(canonical);
            if layer.read_only {
                snapshot.read_only.insert(file);
            }
            documents.push((relative.to_owned(), file));
        }
        snapshot.layers.push(Layer {
            name: layer.name.clone(),
            documents,
        });
    }
    snapshot.options = options;
    let schema = match project
        .schema_path(&dir, fetch)
        .map_err(|e| e.to_string())?
    {
        Some(path) => Some(SchemaFile::read(&path).map_err(|e| e.to_string())?),
        None => None,
    };
    let mut check = CheckOptions::default();
    check.implicit_some = project.options.implicit_some;
    check.allow_unknown_fields = project.options.allow_unknown_fields;
    check.require_complete = project.options.require_complete;
    Ok((schema, project.root.clone(), check))
}
