//! `crol explain` (`SPEC.md` 11, 12.3): print the operations that produced a
//! value.

use std::fmt::Write as _;
use std::io::Write;

use crol_eval::{StepKind, print};

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
    if let Err(error) = std::io::stdout().lock().write_all(out.as_bytes()) {
        reporter.error(&format!("cannot write the output: {error}"));
        return false;
    }
    true
}
