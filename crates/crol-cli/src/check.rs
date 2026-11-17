//! `crol check` (`SPEC.md` 11): read and evaluate the layers, and print the
//! diagnostics.

use crate::diagnostics::Reporter;

#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// The layers, lowest first: each `name=source` or a source, where a
    /// source is a module path or a file system path. Default: the project
    /// file's, of every layer stack. `.crol` files alone are read as value
    /// documents instead, each checked on its own.
    layers: Vec<String>,

    #[command(flatten)]
    inputs: crate::layers::InputArgs,
}

/// Returns whether there were no errors.
pub(crate) fn run(args: &Args, reporter: &mut Reporter) -> bool {
    if let Some(paths) = crate::value::arguments(&args.layers) {
        let mut ok = true;
        for path in paths {
            let Some(document) = crate::value::read(path, &args.inputs, reporter) else {
                ok = false;
                continue;
            };
            reporter.diagnostics(&document.sources, &document.evaluation.diagnostics);
            ok &= !document.evaluation.has_errors();
        }
        return ok;
    }
    // A project with several stacks: each is checked on its own (`SPEC.md`
    // 12.4).
    if args.layers.is_empty()
        && let Some(stacks) = crate::layers::every_stack(&args.inputs)
    {
        let mut ok = true;
        for stack in stacks {
            ok &= check_layers(&args.layers, &args.inputs.with_stack(&stack), reporter);
        }
        return ok;
    }
    check_layers(&args.layers, &args.inputs, reporter)
}

fn check_layers(
    layers: &[String],
    inputs: &crate::layers::InputArgs,
    reporter: &mut Reporter,
) -> bool {
    let Some(inputs) = crate::layers::inputs(layers, inputs, None, reporter) else {
        return false;
    };
    let evaluation = crate::layers::evaluate(&inputs);
    reporter.diagnostics(&inputs.sources, &evaluation.diagnostics);
    !evaluation.has_errors()
}
