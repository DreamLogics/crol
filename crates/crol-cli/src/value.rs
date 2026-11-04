//! A value document on the command line: a `.crov` file given alone is read
//! in value mode (`SPEC.md` 4, 11). It has no layers, variants or contexts.

use std::path::{Path, PathBuf};

use crol::Mode;
use crol::project::{FILE_NAME, Project};
use crol::schema::SchemaFile;
use crol_eval::{Evaluation, Sources};

use crate::diagnostics::Reporter;
use crate::layers::InputArgs;

/// A value document, evaluated, and checked if it has a schema.
pub(crate) struct Document {
    pub(crate) sources: Sources,
    pub(crate) evaluation: Evaluation,
    pub(crate) schema: Option<SchemaFile>,
    /// The type of the value, if the document's type name chooses one.
    pub(crate) root: Option<String>,
}

/// The value document the arguments give: one `.crov` file. Among other
/// sources, or as `name=source`, it is a layer, which fails as one.
pub(crate) fn argument(arguments: &[String]) -> Option<&Path> {
    match arguments {
        [one] if Mode::of(one) == Some(Mode::Value) && !is_named(one) => Some(Path::new(one)),
        _ => None,
    }
}

/// Whether a layer argument is written `name=source`.
fn is_named(argument: &str) -> bool {
    argument
        .split_once('=')
        .is_some_and(|(name, _)| crol_eval::is_identifier(name))
}

/// The error for asking `flag` of a value document, which has no variants.
pub(crate) fn no_contexts(path: &Path, flag: &str, reporter: &mut Reporter) -> bool {
    reporter.error(&format!(
        "{}: a value document has no variants, so `{flag}` does not apply",
        path.display()
    ));
    false
}

/// Reads and evaluates the document at `path`, and checks it against the
/// schema `--schema` gives, else the one it names, else, for a project file,
/// the one `crol` provides. Returns `None` after reporting an error.
pub(crate) fn read(path: &Path, args: &InputArgs, reporter: &mut Reporter) -> Option<Document> {
    if args.declares_variants() {
        no_contexts(path, "--discrete, --group, --parent", reporter);
        return None;
    }
    let mut fail = |message: String| {
        reporter.error(&message);
        None
    };
    let text = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => return fail(format!("cannot read {}: {error}", path.display())),
    };
    let Ok(text) = String::from_utf8(text) else {
        return fail(format!("{} is not valid UTF-8", path.display()));
    };
    let mut sources = Sources::new();
    let file = sources.add(path.display().to_string(), text.as_str());
    let mut evaluation = crol_eval::evaluate_value(&sources, file);
    let schema = match document_schema(path, &text, args) {
        Ok(schema) => schema,
        Err(message) => return fail(message),
    };
    let root = checked(&mut evaluation, schema.as_ref());
    Some(Document {
        sources,
        evaluation,
        schema,
        root,
    })
}

/// Adds what the schema finds to the diagnostics, and returns the value's
/// type if its name chooses one.
fn checked(evaluation: &mut Evaluation, schema: Option<&SchemaFile>) -> Option<String> {
    let (schema, value) = (schema?, evaluation.value.as_ref()?);
    let root = crol::value_type(value, schema);
    let found = crol::check_value(value, schema, root.as_deref(), &Default::default());
    evaluation.diagnostics.extend(found);
    root
}

/// The schema of a document: `--schema`, the one it names, or, for a project
/// file, the one `crol` provides for it (`SPEC.md` 12.4).
fn document_schema(
    path: &Path,
    text: &str,
    args: &InputArgs,
) -> Result<Option<SchemaFile>, String> {
    if let Some(schema) = args.schema() {
        return SchemaFile::read(schema)
            .map(Some)
            .map_err(|e| e.to_string());
    }
    let is_project_file = path.file_name().is_some_and(|name| name == FILE_NAME)
        || args
            .project()
            .is_some_and(|project| same_file(project, path));
    if is_project_file {
        return Ok(Some(crol::schema::build::<Project>()));
    }
    let root = root_directory(args)?;
    match crol::schema::attribute_path(text, path, &root, !args.offline()) {
        Some((_, Ok(schema))) => SchemaFile::read(schema)
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display())),
        Some((_, Err(error))) => Err(format!("{}: {error}", path.display())),
        None => Ok(None),
    }
}

/// Where `::` module paths start: the project file's directory, else the
/// working directory (`SPEC.md` 4.2).
fn root_directory(args: &InputArgs) -> Result<PathBuf, String> {
    let working = std::env::current_dir().unwrap_or_default();
    let project = match args.project() {
        Some(project) => Some(project.to_path_buf()),
        None => Project::find(&working).map_err(|e| e.to_string())?,
    };
    Ok(project
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or(working))
}

fn same_file(a: &Path, b: &Path) -> bool {
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    canonical(a) == canonical(b)
}
