//! `crol eval` (`SPEC.md` 11): print the evaluated data as a data-mode
//! document, a value document as a value, or one value of either.

use std::io::Write;
use std::path::Path;

use crol_eval::{Key, Span, Value, ValueKind, print};
use crol_syntax::format::FormatConfig;

use crate::diagnostics::Reporter;

#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// The layers, lowest first, or one `.crov` file, then optionally a key
    /// and path to print only that value: `web.limits.cpu`. Without layers,
    /// the project file's. A last argument of the form of a key is a key:
    /// write a layer `./base` or `::base` there.
    #[arg(value_name = "[LAYER...] [KEY[.PATH]]")]
    arguments: Vec<String>,

    /// The maximum line width of the output.
    #[arg(long, default_value_t = FormatConfig::default().max_width)]
    max_width: usize,

    /// The context to print, by its variants: `--context prod_eu,cache`.
    /// Default: the base.
    #[arg(
        long,
        value_delimiter = ',',
        value_name = "VARIANTS",
        conflicts_with = "all_contexts"
    )]
    context: Option<Vec<String>>,

    /// Print the base, then each other context's differences as a variant
    /// block.
    #[arg(long)]
    all_contexts: bool,

    /// Print fields that take their defaults as the data leaves them out,
    /// instead of with the defaults the schema records.
    #[arg(long)]
    no_defaults: bool,

    #[command(flatten)]
    inputs: crate::layers::InputArgs,
}

/// Returns whether it succeeded.
pub(crate) fn run(args: &Args, reporter: &mut Reporter) -> bool {
    let mut layers = args.arguments.clone();
    let path = layers.pop_if(|last| crate::layers::is_key(last));
    if let Some(document) = crate::value::argument(&layers) {
        return run_value(args, document, path.as_deref(), reporter);
    }
    if let (true, Some(path)) = (layers.is_empty(), &path)
        && let Some(message) = crate::layers::no_layers_for(path, &args.inputs)
    {
        reporter.error(&message);
        return false;
    }
    let contexts = match (&args.context, args.all_contexts) {
        (_, true) => None,
        (Some(context), false) => Some(vec![context.clone()]),
        (None, false) => Some(Vec::new()),
    };
    let Some(mut inputs) = crate::layers::inputs(&layers, &args.inputs, contexts, reporter) else {
        return false;
    };
    let evaluation = crate::layers::evaluate(&inputs);
    reporter.diagnostics(&inputs.sources, &evaluation.diagnostics);
    let Some(base) = evaluation.value else {
        return false;
    };
    let config = FormatConfig::default().with_max_width(args.max_width);
    if args.all_contexts {
        if path.is_some() {
            reporter.error("`--all-contexts` prints all the data; it takes no key");
            return false;
        }
        let output =
            print::to_all_contexts(&base, &evaluation.contexts, &evaluation.variants, &config);
        return write(&output, reporter);
    }
    // With `--context`, the one context evaluated besides the base.
    let root = match (&args.context, evaluation.contexts.first()) {
        (Some(_), Some(data)) => data.root(&base),
        _ => base,
    };
    // Fields that take their defaults show them, as the application sees
    // the data, each marked with a comment.
    let filled = Span {
        file: inputs.sources.add("<default>", ""),
        range: Default::default(),
    };
    let root = match &inputs.schema {
        Some(schema) if !args.no_defaults => {
            crol::fill_defaults(&root, schema, inputs.root.as_deref(), filled)
        }
        _ => root,
    };
    print_value(&root, path.as_deref(), filled, &config, reporter)
}

/// `crol eval` of a value document (`SPEC.md` 11).
fn run_value(args: &Args, document: &Path, path: Option<&str>, reporter: &mut Reporter) -> bool {
    if args.all_contexts {
        return crate::value::no_contexts(document, "--all-contexts", reporter);
    }
    if args.context.is_some() {
        return crate::value::no_contexts(document, "--context", reporter);
    }
    let Some(mut document) = crate::value::read(document, &args.inputs, reporter) else {
        return false;
    };
    reporter.diagnostics(&document.sources, &document.evaluation.diagnostics);
    let value = match document.evaluation.value.take() {
        Some(value) if !document.evaluation.has_errors() => value,
        _ => return false,
    };
    let filled = Span {
        file: document.sources.add("<default>", ""),
        range: Default::default(),
    };
    let value = match &document.schema {
        Some(schema) if !args.no_defaults => {
            crol::fill_defaults_value(&value, schema, document.root.as_deref(), filled)
        }
        _ => value,
    };
    let config = FormatConfig::default().with_max_width(args.max_width);
    print_value(&value, path, filled, &config, reporter)
}

/// Prints `root`, or the value at `path` in it, with what has the span
/// `filled` marked as a default.
fn print_value(
    root: &Value,
    path: Option<&str>,
    filled: Span,
    config: &FormatConfig,
    reporter: &mut Reporter,
) -> bool {
    let note = |value: &Value| (value.span == filled).then(|| "default".to_owned());
    let value = match path {
        None => root,
        Some(path) => match select(root, path) {
            Ok(value) => value,
            Err(message) => {
                reporter.error(&message);
                return false;
            }
        },
    };
    let output = print::to_document_noted(value, config, &note);
    write(&output, reporter)
}

fn write(output: &str, reporter: &mut Reporter) -> bool {
    if let Err(error) = std::io::stdout().lock().write_all(output.as_bytes()) {
        reporter.error(&format!("cannot write the output: {error}"));
        return false;
    }
    true
}

/// The value at `path` (`web.limits.cpu`, `web.tags[0]`) in the root.
fn select<'v>(root: &'v Value, path: &str) -> Result<&'v Value, String> {
    let (key, steps) = crol_eval::parse_path(path)?;
    let missing = || format!("there is no value at `{path}`");
    let ValueKind::Keyed(keyed) = &root.kind else {
        return Err(missing());
    };
    let value = keyed.entries.get(&Key::Str(key)).ok_or_else(missing)?;
    crol_eval::value_at_path(value, &steps).ok_or_else(missing)
}
