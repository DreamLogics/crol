//! `crol explain` (`SPEC.md` 11, 12.3): print the operations that produced a
//! value.

use std::fmt::Write as _;
use std::io::Write;

use crol::{DataPath, DataStep};
use crol_eval::{Key, PathStep, StepKind, Value, ValueKind, print};

use crate::diagnostics::Reporter;

#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// The layers, lowest first, then the key and path to explain:
    /// `web.limits.memory_mb`. Without layers, the project file's.
    #[arg(value_name = "[LAYER...] KEY[.PATH]", required = true)]
    arguments: Vec<String>,

    /// The context, by its variants: `--context prod_eu,cache`. Default: the
    /// base.
    #[arg(long, value_delimiter = ',', value_name = "VARIANTS")]
    context: Option<Vec<String>>,

    #[command(flatten)]
    inputs: crate::layers::InputArgs,
}

/// Returns whether it succeeded.
pub(crate) fn run(args: &Args, reporter: &mut Reporter) -> bool {
    let Some((path, layers)) = args
        .arguments
        .split_last()
        .filter(|(path, _)| crate::layers::is_key(path))
    else {
        let last = args.arguments.last().map_or("", String::as_str);
        reporter.error(&format!(
            "`{last}` is not a key or a path, so there is no key to explain; give the layers, if any, then the key"
        ));
        return false;
    };
    if layers.is_empty()
        && let Some(message) = crate::layers::no_layers_for(path, &args.inputs)
    {
        reporter.error(&message);
        return false;
    }
    if let Some(document) = crate::value::argument(layers) {
        // Provenance is the operations of layers; a value has none.
        reporter.error(&format!(
            "{}: a value document has no layers to explain; `crol eval {} {path}` prints the value",
            document.display(),
            document.display()
        ));
        return false;
    }
    let context = args.context.clone().unwrap_or_default();
    let Some(inputs) =
        crate::layers::inputs(layers, &args.inputs, Some(vec![context.clone()]), reporter)
    else {
        return false;
    };
    let (sources, layers, options) = (&inputs.sources, &inputs.layers, &inputs.options);
    let steps = match crol_eval::explain(sources, layers, options, path, &context) {
        Ok(steps) => steps,
        Err(diagnostics) => {
            reporter.diagnostics(sources, &diagnostics);
            return false;
        }
    };
    let mut out = String::new();
    let in_context = if context.is_empty() {
        String::new()
    } else {
        format!(" in {{{}}}", context.join(", "))
    };
    if steps.is_empty() {
        let _ = writeln!(out, "{path}{in_context}: no operation sets it");
    } else {
        let _ = writeln!(out, "{path}{in_context}:");
    }
    let locations: Vec<String> = steps
        .iter()
        .map(|s| sources.location(s.span).to_string())
        .collect();
    let width = locations.iter().map(String::len).max().unwrap_or(0);
    let entry_width = steps.iter().map(|s| s.entry.len()).max().unwrap_or(0);
    for (step, location) in steps.iter().zip(&locations) {
        let kind = match step.kind {
            StepKind::Definition => "definition",
            StepKind::Patch => "patch",
            StepKind::Remove => "remove",
        };
        let value = step
            .value
            .as_ref()
            .map_or_else(|| "(missing)".to_owned(), print::to_text);
        let _ = writeln!(
            out,
            "  {location:<width$}  {:<entry_width$}  {kind:<10}  {value}",
            step.entry
        );
    }
    // What the ids there name, and in which scope (`SPEC.md` 3.2).
    if !steps.is_empty() {
        out.push_str(&ids(&inputs, path, &context));
    }
    if let Err(error) = std::io::stdout().lock().write_all(out.as_bytes()) {
        reporter.error(&format!("cannot write the output: {error}"));
        return false;
    }
    true
}

/// A line for each id at `path` or in the value there, in the data of the
/// context: where it is, what it names, and the scopes around it. Nothing
/// where there is no id.
fn ids(inputs: &crate::layers::Inputs, path: &str, context: &[String]) -> String {
    let evaluation = crol_eval::evaluate_data(&inputs.sources, &inputs.layers, &inputs.options);
    let Some(base) = evaluation.value else {
        return String::new();
    };
    // With a context, the one evaluated besides the base.
    let root = match evaluation.contexts.first() {
        Some(data) if !context.is_empty() => data.root(&base),
        _ => base,
    };
    let Some(place) = data_path(&root, path) else {
        return String::new();
    };
    let mut resolver = crol::IdResolver::new(&root).within(&place);
    // The schema is for its `#[crol(scope)]` types only; nothing is checked.
    if let Some(schema) = &inputs.schema {
        resolver = resolver.schema(schema);
        if let Some(root) = &inputs.root {
            resolver = resolver.root_type(root);
        }
    }
    let chains = resolver.chains();
    if chains.is_empty() {
        return String::new();
    }
    let locations: Vec<String> = chains.iter().map(|c| c.location.to_string()).collect();
    let width = locations.iter().map(String::len).max().unwrap_or(0);
    let mut out = String::from("ids:\n");
    for (chain, location) in chains.iter().zip(&locations) {
        let _ = writeln!(out, "  {location:<width$}  {chain}");
    }
    out
}

/// The place `path` (`web.tags[0]`) reaches in the root: `[0]` is an index
/// of a list and a key elsewhere, and a field of what `Some(...)` holds is
/// in its payload.
fn data_path(root: &Value, path: &str) -> Option<DataPath> {
    let (key, steps) = crol_eval::parse_path(path).ok()?;
    let mut out = vec![DataStep::Key(Key::Str(key.as_str().into()))];
    let mut value = DataPath::new(out.clone()).value_in(root)?;
    for step in &steps {
        if matches!(step, PathStep::Field(_) | PathStep::Accessor(_))
            && let ValueKind::NamedTuple(_, items) = &value.kind
            && let [payload] = items.as_slice()
        {
            out.push(DataStep::Element(0));
            value = payload;
        }
        let next = match (step, &value.kind) {
            (PathStep::Field(name), _) => DataStep::Key(Key::Str(name.as_str().into())),
            (PathStep::Element(i), _) => DataStep::Element(*i),
            (PathStep::Accessor(i), ValueKind::List(_)) => {
                DataStep::Index(usize::try_from(*i).ok()?)
            }
            (PathStep::Accessor(i), _) => DataStep::Key(Key::Int(*i)),
        };
        value = DataPath::new([next.clone()]).value_in(value)?;
        out.push(next);
    }
    Some(DataPath::new(out))
}
