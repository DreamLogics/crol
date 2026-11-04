//! Layers, variants, options and the schema: from the command line or the
//! project file (`SPEC.md` 11, 12.4).

use std::path::{Path, PathBuf};

use crol::project::{FILE_NAME, Project};
use crol::schema::SchemaFile;
use crol::{CheckOptions, CrolComposer, CrolConfig, FileLoaders, LayerSource, Mode};
use crol_eval::{DataOptions, Diagnostic, Evaluation, Layer, Sources, Span, Variants};

use crate::diagnostics::Reporter;

/// The options common to every command that evaluates (`SPEC.md` 11).
#[derive(Debug, Clone, Default, clap::Args)]
pub(crate) struct InputArgs {
    /// Declare discrete variants: `--discrete cache,debug`.
    #[arg(long, value_delimiter = ',', value_name = "NAMES")]
    discrete: Vec<String>,

    /// Declare a group of alternatives: `--group dev,prod,prod_eu`.
    /// Repeatable.
    #[arg(long, value_name = "NAMES")]
    group: Vec<String>,

    /// Give a variant a parent: `--parent prod_eu=prod`. Repeatable.
    #[arg(long, value_name = "CHILD=PARENT")]
    parent: Vec<String>,

    /// The project file; default: `project.crov` in the current directory or
    /// above, used when no layers are given.
    #[arg(long, value_name = "FILE")]
    project: Option<PathBuf>,

    /// A schema file to check the data against.
    #[arg(long, value_name = "FILE")]
    schema: Option<PathBuf>,

    /// Fetch no schemas named by URL or crate; use cached copies and local
    /// crates only.
    #[arg(long)]
    offline: bool,
}

impl InputArgs {
    /// `--schema`.
    pub(crate) fn schema(&self) -> Option<&Path> {
        self.schema.as_deref()
    }

    /// `--project`.
    pub(crate) fn project(&self) -> Option<&Path> {
        self.project.as_deref()
    }

    /// `--offline`.
    pub(crate) fn offline(&self) -> bool {
        self.offline
    }

    pub(crate) fn declares_variants(&self) -> bool {
        !self.discrete.is_empty() || !self.group.is_empty() || !self.parent.is_empty()
    }

    fn variants(&self) -> Result<Variants, String> {
        let mut variants = Variants::new();
        for group in &self.group {
            variants.add_grouped(group.split(','))?;
        }
        for name in &self.discrete {
            variants.add_discrete(name)?;
        }
        for pair in &self.parent {
            let (child, parent) = pair
                .split_once('=')
                .ok_or_else(|| format!("`--parent {pair}` is not `child=parent`"))?;
            variants.add_parent(child, parent)?;
        }
        Ok(variants)
    }
}

/// Everything needed to evaluate.
pub(crate) struct Inputs {
    pub(crate) sources: Sources,
    pub(crate) layers: Vec<Layer>,
    pub(crate) options: DataOptions,
    pub(crate) schema: Option<SchemaFile>,
    /// The type of root values, if the project names one.
    pub(crate) root: Option<String>,
    pub(crate) check: CheckOptions,
    /// Warnings about the inputs, such as documents that name another
    /// schema.
    pub(crate) warnings: Vec<Diagnostic>,
}

/// Whether `text` has the form of a module path (`SPEC.md` 4.2).
fn is_module_path(text: &str) -> bool {
    let rest = text.strip_prefix("::").unwrap_or(text);
    !rest.is_empty() && rest.split("::").all(crol_eval::is_identifier)
}

/// Whether the last argument of `eval` or `explain` is a key: it has the form
/// of a key or path, and does not end like a document (`SPEC.md` 11). Only the
/// form counts, so that a command means the same whatever files exist.
pub(crate) fn is_key(argument: &str) -> bool {
    crol_eval::parse_path(argument).is_ok()
        && Mode::of(argument).is_none()
        && !crol::is_retired(argument)
}

/// The error for `key` when there is nothing to read it from: no layers were
/// given, and no project file is found. `None` if there is one.
pub(crate) fn no_layers_for(key: &str, args: &InputArgs) -> Option<String> {
    let dir = std::env::current_dir().ok()?;
    let none = args.project.is_none() && matches!(Project::find(&dir), Ok(None));
    none.then(|| {
        format!(
            "no layers given, and no {FILE_NAME} in this directory or above; `{key}` is read as a key, so write `./{key}` to name a layer"
        )
    })
}

/// The layers, variants, options and schema: from the arguments, or with no
/// layer arguments from the project file. Returns `None` after reporting an
/// error.
pub(crate) fn inputs(
    arguments: &[String],
    args: &InputArgs,
    contexts: Option<Vec<Vec<String>>>,
    reporter: &mut Reporter,
) -> Option<Inputs> {
    let fail = |reporter: &mut Reporter, message: String| {
        reporter.error(&message);
        None
    };
    let project_path = match &args.project {
        Some(path) => Some(path.clone()),
        None => match std::env::current_dir().map(|dir| Project::find(&dir)) {
            Ok(Ok(path)) => path,
            Ok(Err(error)) => return fail(reporter, error.to_string()),
            Err(_) => None,
        },
    };
    let project = match &project_path {
        Some(path) if arguments.is_empty() || args.project.is_some() => match Project::read(path) {
            Ok(project) => Some(project),
            Err(error) => return fail(reporter, error.to_string()),
        },
        _ => None,
    };
    let project_dir = project_path
        .as_ref()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)
        .unwrap_or_default();

    // The root directory module paths start from (`SPEC.md` 4.2).
    let root_dir = if project.is_some() {
        project_dir.clone()
    } else {
        std::env::current_dir().unwrap_or_default()
    };
    // The project file is never read as data (`SPEC.md` 12.4).
    let file_loaders = match &project_path {
        Some(file) => FileLoaders::new().with_project_file(file),
        None => FileLoaders::new(),
    };
    let mut composer = CrolComposer::with_cfg(CrolConfig {
        // Tools always follow `extends` (`SPEC.md` 4.1).
        load_extends: true,
        root_path: root_dir.clone(),
        file_loaders: file_loaders.clone(),
        ..CrolConfig::<String>::default()
    });
    let config = CrolConfig::<String> {
        file_loaders: file_loaders.clone(),
        ..CrolConfig::default()
    };
    if arguments.is_empty() {
        let Some(project) = &project else {
            return fail(
                reporter,
                format!("no layers given, and no {FILE_NAME} in this directory or above"),
            );
        };
        let layers = match project.layer_list(&project_dir) {
            Ok(layers) => layers,
            Err(error) => return fail(reporter, error.to_string()),
        };
        for layer in layers {
            let source = match file_loaders.resolve(&layer.dir, &project_dir) {
                Ok(source) => source,
                Err(error) => {
                    return fail(
                        reporter,
                        format!("project: layer `{}`: {error}", layer.name),
                    );
                }
            };
            match source.load(layer.name.clone()) {
                Ok(loaded) => composer = composer.load_layer(loaded),
                Err(error) => return fail(reporter, error.to_string()),
            }
        }
    }
    for argument in arguments {
        let (name, source) = match argument.split_once('=') {
            Some((name, source)) if crol_eval::is_identifier(name) => (Some(name), source),
            _ => (None, argument.as_str()),
        };
        // The module path, if both name something (`SPEC.md` 11).
        let module = is_module_path(source).then(|| source.load(&config));
        let layer = match module {
            Some(Ok(layer)) => Ok(layer),
            Some(Err(error)) if !Path::new(source).exists() => Err(error),
            _ => Path::new(source).load(&config),
        };
        match layer {
            Ok(layer) => {
                let layer = match name {
                    Some(name) => layer.with_name(name),
                    None => layer,
                };
                composer = composer.load_layer(layer);
            }
            Err(error) => return fail(reporter, error.to_string()),
        }
    }

    let variants = if args.declares_variants() || project.is_none() {
        args.variants()
    } else {
        project.as_ref().map_or(Ok(Variants::new()), |p| {
            p.variants().map_err(|e| e.to_string())
        })
    };
    let variants = match variants {
        Ok(variants) => variants,
        Err(message) => return fail(reporter, message),
    };
    let mut options = DataOptions::default().with_variants(variants);
    let mut check = CheckOptions::default();
    let mut root = None;
    let mut schema_path = args.schema.clone();
    let mut used = "the one given with `--schema`";
    if let Some(project) = &project {
        let project_options = &project.options;
        options = options
            .with_warn_cross_file_patches(project_options.warn_cross_file_patches)
            .with_max_contexts(project_options.max_contexts)
            .with_contexts(project_options.contexts.clone());
        match project_options.variant_keys() {
            Ok(rule) => options = options.with_variant_keys(rule),
            Err(error) => return fail(reporter, error.to_string()),
        }
        check.implicit_some = project_options.implicit_some;
        check.allow_unknown_fields = project_options.allow_unknown_fields;
        check.require_complete = project_options.require_complete;
        root.clone_from(&project.root);
        if schema_path.is_none() {
            match project.schema_path(&project_dir, !args.offline) {
                Ok(path) => {
                    schema_path = path;
                    used = "the project's";
                }
                Err(error) => return fail(reporter, error.to_string()),
            }
        }
    }
    if contexts.is_some() {
        options = options.with_contexts(contexts);
    }
    // Else the schema the documents name with `#![schema(...)]`, if they
    // agree (`SPEC.md` 4.1).
    let given = schema_path.clone();
    if schema_path.is_none() {
        let mut named: Vec<PathBuf> = Vec::new();
        for layer in composer.layers() {
            for (_, display, text) in layer.documents() {
                match crol::schema::attribute_path(
                    text,
                    Path::new(display),
                    &root_dir,
                    !args.offline,
                ) {
                    Some((_, Ok(path))) => {
                        let path = path.canonicalize().unwrap_or(path);
                        if !named.contains(&path) {
                            named.push(path);
                        }
                    }
                    Some((_, Err(error))) => {
                        return fail(reporter, format!("{display}: {error}"));
                    }
                    None => {}
                }
            }
        }
        match named.len() {
            0 => {}
            1 => schema_path = named.pop(),
            _ => {
                return fail(
                    reporter,
                    "the documents name different schemas; choose one with --schema".to_owned(),
                );
            }
        }
    }
    let schema = match &schema_path {
        Some(path) => match SchemaFile::read(path) {
            Ok(schema) => Some(schema),
            Err(error) => return fail(reporter, error.to_string()),
        },
        None => None,
    };

    let mut sources = Sources::new();
    let mut warnings = Vec::new();
    let layers: Vec<Layer> = composer
        .layers()
        .iter()
        .map(|layer| Layer {
            name: layer.name().to_owned(),
            documents: layer
                .documents()
                .map(|(relative, display, text)| {
                    let file = sources.add(display, text);
                    // A document should agree with the schema it is
                    // checked against.
                    if let Some(given) = &given
                        && let Some((range, message)) = crol::schema::attribute_disagrees(
                            text,
                            Path::new(display),
                            &root_dir,
                            given,
                            used,
                            !args.offline,
                        )
                    {
                        warnings.push(Diagnostic::warning(Span::new(file, range), message));
                    }
                    (relative.to_owned(), file)
                })
                .collect(),
        })
        .collect();
    Some(Inputs {
        sources,
        layers,
        options,
        schema,
        root,
        check,
        warnings,
    })
}

/// Evaluates the inputs, and checks them against the schema if there is
/// one. The evaluation's diagnostics include the schema's.
pub(crate) fn evaluate(inputs: &Inputs) -> Evaluation {
    let mut evaluation = crol_eval::evaluate_data(&inputs.sources, &inputs.layers, &inputs.options);
    evaluation
        .diagnostics
        .extend(inputs.warnings.iter().cloned());
    if let (Some(schema), Some(base)) = (&inputs.schema, &evaluation.value) {
        let view = crol::DataView {
            base,
            contexts: &evaluation.contexts,
            variants: &evaluation.variants,
        };
        let found = crol::check_view(view, schema, inputs.root.as_deref(), &inputs.check);
        evaluation.diagnostics.extend(found);
    }
    evaluation
}
