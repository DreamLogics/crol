//! A value document on the command line: a `.crol` file given alone is read
//! in value mode (`SPEC.md` 4, 11). It has no layers, variants or contexts.

use std::path::{Path, PathBuf};

use crol::project::{FILE_NAME, Project, ProjectStack};
use crol::schema::SchemaFile;
use crol::{CheckOptions, Mode};
use crol_eval::{Evaluation, Sources};

use crate::diagnostics::Reporter;
use crate::layers::InputArgs;

/// A value document, evaluated, and checked if it has a schema.
pub(crate) struct Document {
    pub(crate) sources: Sources,
    pub(crate) evaluation: Evaluation,
    pub(crate) schema: Option<SchemaFile>,
    /// The type of the value, if the project's `files` give it one or the
    /// document's type name chooses one.
    pub(crate) root: Option<String>,
}

/// What to check a document against.
#[derive(Default)]
struct Expected {
    schema: Option<SchemaFile>,
    /// The type `files` gives it.
    root: Option<String>,
    options: CheckOptions,
    /// The stack whose `files` give it its type, for its constants.
    stack: Option<ProjectStack>,
}

/// The value document the arguments give: one `.crol` file. Among other
/// sources, or as `name=source`, it is a layer, which fails as one.
pub(crate) fn argument(arguments: &[String]) -> Option<&Path> {
    match arguments {
        [one] if Mode::of(one) == Some(Mode::Value) && !is_named(one) => Some(Path::new(one)),
        _ => None,
    }
}

/// The value documents the arguments give, for `crol check`: one or more
/// `.crol` files and nothing else, each checked on its own, in its own
/// stack (`SPEC.md` 11, 12.4).
pub(crate) fn arguments(arguments: &[String]) -> Option<Vec<&Path>> {
    let documents: Vec<&Path> = arguments
        .iter()
        .filter(|one| Mode::of(one) == Some(Mode::Value) && !is_named(one))
        .map(Path::new)
        .collect();
    (!documents.is_empty() && documents.len() == arguments.len()).then_some(documents)
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
/// schema `--schema` gives, else, for a project or schema file, the one
/// `crol` provides, else the one it names, else the project's, for a type
/// that the project's `files` give it. Returns `None` after reporting an
/// error.
pub(crate) fn read(path: &Path, args: &InputArgs, reporter: &mut Reporter) -> Option<Document> {
    if args.declares_variants() {
        no_contexts(path, "--discrete, --group, --open, --parent", reporter);
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
    // The project file, read once. One that does not load gives no
    // constants, so that checking it, or a document that needs nothing of
    // it, still reports what is wrong where it is.
    let project = project_path(args).map(|project_path| {
        let read = Project::read(&project_path);
        (project_path, read)
    });
    let found = project.as_ref().map(|(path, read)| (path.as_path(), read));
    let expected = match expected(path, &text, args, found) {
        Ok(expected) => expected,
        Err(message) => return fail(message),
    };
    // What `$name` reads (`SPEC.md` 6.1.4): the stand-ins of the stack
    // whose `files` give it its type, then `--const`, checked against what
    // the schema declares.
    let mut constants = match args.constants(expected.stack.as_ref()) {
        Ok(constants) => constants,
        Err(message) => return fail(message),
    };
    let declared = crol::check_constants(&mut constants, expected.schema.as_ref());
    let mut evaluation = crol_eval::evaluate_value_with(&sources, file, &constants);
    if declared.iter().any(crol_eval::Diagnostic::is_error) {
        evaluation.value = None;
    }
    evaluation.diagnostics.splice(0..0, declared);
    let root = checked(&mut evaluation, &expected);
    // Without a schema, ids that name nothing in the document (`SPEC.md`
    // 10.5); with one, the check reports them.
    if expected.schema.is_none()
        && let Some(value) = &evaluation.value
    {
        let found = crol::IdResolver::new(value)
            .ids()
            .into_iter()
            .filter_map(|id| {
                id.problem
                    .map(|problem| crol_eval::Diagnostic::warning(id.span, problem))
            });
        evaluation.diagnostics.extend(found);
    }
    if let Some(schema) = &expected.schema {
        let root = root.as_deref().unwrap_or(&schema.root);
        let root = crol::typed::TypeRoot::Value(root);
        let found = typed_binds(&sources, schema, &expected.options, root);
        evaluation.diagnostics.extend(found);
    }
    Some(Document {
        sources,
        evaluation,
        schema: expected.schema,
        root,
    })
}

/// Adds what the schema finds to the diagnostics, and returns the value's
/// type: the one the project gives it, else the one its name chooses.
fn checked(evaluation: &mut Evaluation, expected: &Expected) -> Option<String> {
    let schema = expected.schema.as_ref()?;
    let value = evaluation.value.as_ref();
    let root = (expected.root.clone()).or_else(|| crol::value_type(value?, schema));
    if let Some(value) = value {
        let found = crol::check_value(value, schema, root.as_deref(), &expected.options);
        evaluation.diagnostics.extend(found);
    }
    root
}

/// What the schema finds wrong with the `#[type(...)]` binds of every
/// document in `sources`, and with where their templates are used (`SPEC.md`
/// 6.1.2).
pub(crate) fn typed_binds(
    sources: &crol::Sources,
    schema: &crol::schema::SchemaFile,
    options: &crol::CheckOptions,
    root: crol::typed::TypeRoot<'_>,
) -> Vec<crol_eval::Diagnostic> {
    let documents: Vec<_> = sources
        .files()
        .map(|(file, source)| (file, crol_syntax::parse(source.text()).document()))
        .collect();
    let mut found = Vec::new();
    for (file, document) in &documents {
        found.extend(crol::typed::check_document(
            *file, document, schema, options,
        ));
    }
    let templates = crol::typed::typed_templates(documents.iter().map(|(f, d)| (*f, d)), schema);
    for (file, document) in &documents {
        let uses = crol::typed::check_template_uses(*file, document, schema, &templates, root);
        found.extend(uses);
    }
    found
}

/// What a document is checked against: `--schema`, for a project or schema
/// file the schema `crol` provides for it (`SPEC.md` 12.4, 10.10), the schema
/// it names, or the type the project's `files` give it.
fn expected(
    path: &Path,
    text: &str,
    args: &InputArgs,
    project: Option<(&Path, &Result<Project, crol::Error>)>,
) -> Result<Expected, String> {
    let schema_only = |schema| Expected {
        schema: Some(schema),
        ..Default::default()
    };
    if let Some(schema) = args.schema() {
        return SchemaFile::read(schema)
            .map(schema_only)
            .map_err(|e| e.to_string());
    }
    let is_project_file = path.file_name().is_some_and(|name| name == FILE_NAME)
        || args
            .project()
            .is_some_and(|project| same_file(project, path));
    if is_project_file {
        return Ok(schema_only(crol::schema::build::<Project>()));
    }
    if crol::schema::is_schema_file(text) {
        return Ok(schema_only(crol::schema::build::<SchemaFile>()));
    }
    let working = std::env::current_dir().unwrap_or_default();
    let root = project
        .map(|(project_path, _)| crol::directory_of(project_path))
        .map_or(working, Path::to_path_buf);
    match crol::schema::attribute_path(text, path, &root, !args.offline()) {
        Some((_, Ok(schema))) => SchemaFile::read(schema)
            .map(schema_only)
            .map_err(|e| format!("{}: {e}", path.display())),
        Some((_, Err(error))) => Err(format!("{}: {error}", path.display())),
        None => match project {
            Some((project_path, read)) => project_files(project_path, read, path, args),
            None => Ok(Expected::default()),
        },
    }
}

/// What the project's `files` say of a document that says nothing itself:
/// the schema and options of the stack whose `files` match it, with the
/// type of its first glob that matches; with `--stack`, of that stack only.
/// Nothing, if none does (`SPEC.md` 12.4).
fn project_files(
    project_path: &Path,
    project: &Result<Project, crol::Error>,
    path: &Path,
    args: &InputArgs,
) -> Result<Expected, String> {
    let project = project.as_ref().map_err(|e| e.to_string())?;
    let dir = crol::directory_of(project_path);
    let found = match args.stack() {
        Some(name) => {
            let stack = project.stack(name).map_err(|e| e.to_string())?;
            stack
                .file_type(dir, path)
                .map(|type_name| (name, stack, type_name))
        }
        None => project.file_stack(dir, path),
    };
    let Some((name, stack, type_name)) = found else {
        return Ok(Expected::default());
    };
    let (of_stack, owner) = match project.stacks.len() {
        1 => (String::new(), "project"),
        _ => (format!(" of the stack `{name}`"), "stack"),
    };
    let named = || {
        format!(
            "{}: `files`{of_stack} gives {} the type `{type_name}`",
            project_path.display(),
            path.display()
        )
    };
    let schema = stack
        .schema_path(dir, !args.offline())
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("{}, and the {owner} has no schema", named()))?;
    let schema = SchemaFile::read(schema).map_err(|e| e.to_string())?;
    let root = schema
        .type_path(type_name)
        .ok_or_else(|| format!("{}, which the schema does not have", named()))?
        .to_owned();
    Ok(Expected {
        schema: Some(schema),
        root: Some(root),
        options: stack.options.check_options(),
        stack: Some(stack.clone()),
    })
}

/// The project file: `--project`, else the one found from the working
/// directory (`SPEC.md` 12.4). `::` module paths start from its directory,
/// else the working directory (`SPEC.md` 4.2).
fn project_path(args: &InputArgs) -> Option<PathBuf> {
    match args.project() {
        Some(project) => Some(project.to_path_buf()),
        None => Project::find(&std::env::current_dir().unwrap_or_default()),
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    canonical(a) == canonical(b)
}
