//! What the server knows: the latest evaluation of the project, or of each
//! open document on its own (`SPEC.md` 12.5).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crol::project::Project;
use crol::schema::SchemaFile;
use crol::{
    CheckOptions, FileLoader, FileLoaders, FsLoader, Loaded, LoaderError, ModulePath, SourceId,
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
/// file, saved or not (`SPEC.md` 12.5).
struct BufferLoader {
    files: FsLoader,
    open: Buffers,
}

impl FileLoader for BufferLoader {
    fn resolve(&self, path: &ModulePath<'_>) -> Result<Option<SourceId>, LoaderError> {
        self.files.resolve(path)
    }

    fn load(&self, source: &SourceId) -> Result<Loaded, LoaderError> {
        match source.canonical_path().and_then(|p| self.open.get(p)) {
            Some(text) => Ok(Loaded::Document(text.clone().into_bytes())),
            None => self.files.load(source),
        }
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
    /// Documents evaluated on their own, as value-mode documents.
    pub documents: Vec<(FileId, Evaluation)>,
    /// Diagnostics from the schema and the application's checks.
    pub extra: Vec<Diagnostic>,
    /// The project's schema, if it names one.
    pub schema: Option<SchemaFile>,
    /// The type path of the project's root type, with the schema.
    pub root_type: Option<String>,
    /// The schemas of documents evaluated on their own, with the type of
    /// each one's value: the one it names, or the project file's.
    own_schemas: HashMap<FileId, (SchemaFile, Option<String>)>,
    /// The path of each file, by its id.
    paths: Vec<PathBuf>,
    /// Files of read-only layers, which get no diagnostics.
    read_only: HashSet<FileId>,
    /// A message for the project as a whole, such as a project file that
    /// cannot be read.
    pub problem: Option<String>,
    /// The schemas named by URL or crate, by source and directory, for
    /// fetching them.
    pub(crate) remote: Vec<(String, PathBuf)>,
}

/// The schema a document is written against.
pub(crate) struct DocumentSchema<'a> {
    pub(crate) file: &'a SchemaFile,
    /// The type path of the root type, if not the schema's.
    pub(crate) root: Option<&'a str>,
    /// Whether the document is a value of the root type (a document on its
    /// own) rather than data whose root values are (`SPEC.md` 4.1).
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
        if let Some((schema, root)) = file.and_then(|f| self.own_schemas.get(&f)) {
            return Some(DocumentSchema {
                file: schema,
                root: root.as_deref(),
                value_mode: true,
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
        paths: Vec::new(),
        read_only: HashSet::new(),
        problem: None,
        remote: Vec::new(),
    };
    let canonical_open: Buffers = Arc::new(
        open.iter()
            .map(|(p, t)| (p.canonicalize().unwrap_or_else(|_| p.clone()), t.clone()))
            .collect(),
    );
    let project_path = settings
        .project
        .clone()
        .or_else(|| root.and_then(Project::find));
    let mut check = CheckOptions::default();
    if let Some(project_path) = &project_path {
        match load_project(&mut snapshot, project_path, &canonical_open, remote) {
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
            let found = crol::check_view(view, schema, snapshot.root_type.as_deref(), &check);
            snapshot.extra.extend(found);
        }
    }
    // Open documents that are in no layer are evaluated on their own.
    for (path, text) in canonical_open.iter() {
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
            // `::` starts from the project's directory, else the
            // workspace folder, else the document's own (`SPEC.md` 4.2).
            let root_dir = project_path
                .as_ref()
                .and_then(|p| p.parent())
                .or(root)
                .or(path.parent())
                .unwrap_or(Path::new("."));
            document_schema(text, path, root_dir, remote, &mut snapshot.remote)
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
                    snapshot.own_schemas.insert(file, (schema, root));
                } else {
                    snapshot.own_schemas.insert(file, (schema, None));
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

/// Loads the project's layers into the snapshot; returns its schema, root
/// type and check options.
fn load_project(
    snapshot: &mut Snapshot,
    project_path: &Path,
    open: &Buffers,
    remote: &RemoteStatus,
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
    let loaders = FileLoaders::new().with(BufferLoader {
        files: FsLoader::new(&dir),
        open: Arc::clone(open),
    });
    for layer in project.layer_list(&dir).map_err(|e| e.to_string())? {
        let source = loaders
            .resolve(&layer.dir, &dir)
            .map_err(|e| format!("layer `{}`: {e}", layer.name))?;
        let loaded = source
            .load(layer.name.clone())
            .map_err(|e| e.to_string())?
            .without_file(project_path);
        let mut documents = Vec::new();
        for (relative, display, text) in loaded.documents() {
            let path = PathBuf::from(display);
            let canonical = path.canonicalize().unwrap_or(path);
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
    let schema_path = schema_path?;
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
        Some(path) => Some(SchemaFile::read(&path).map_err(|e| e.to_string())?),
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

    /// A project of one layer directory, with `web.crol` on disk.
    fn project(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("crol-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::write(
            root.join("project.crol"),
            "Project { layers: List([{ name: \"base\", dir: \"data\", .. }]), .. }",
        )
        .unwrap();
        std::fs::write(root.join("data/web.crol"), "+port: 80,").unwrap();
        root
    }

    #[test]
    fn an_open_buffer_replaces_its_file_in_a_layer() {
        let root = project("buffers");
        let settings = Settings::default();
        let remote = RemoteStatus::new();
        let open = HashMap::from([(root.join("data/web.crol"), "+port: 81,".to_owned())]);
        let snapshot = evaluate(Some(&root), &settings, &open, None, &remote);
        assert_eq!(snapshot.problem, None);
        assert_eq!(snapshot.layers.len(), 1);
        let file = snapshot.file_of(&root.join("data/web.crol")).unwrap();
        assert_eq!(snapshot.sources.file(file).text(), "+port: 81,");
        assert!(snapshot.in_layers(file));

        // Closed, the file is read again.
        let snapshot = evaluate(Some(&root), &settings, &HashMap::new(), None, &remote);
        let file = snapshot.file_of(&root.join("data/web.crol")).unwrap();
        assert_eq!(snapshot.sources.file(file).text(), "+port: 80,");
        let _ = std::fs::remove_dir_all(&root);
    }
}
