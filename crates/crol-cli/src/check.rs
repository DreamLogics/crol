//! `crol check` (`SPEC.md` 11): read and evaluate the layers, and print the
//! diagnostics.

use crate::diagnostics::Reporter;

#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// The layers, lowest first: each `name=source` or a source, where a
    /// source is a module path or a file system path. Default: the project
    /// file's.
    layers: Vec<String>,

    #[command(flatten)]
    inputs: crate::layers::InputArgs,
}

/// Returns whether there were no errors.
pub(crate) fn run(args: &Args, reporter: &mut Reporter) -> bool {
    let Some(inputs) = crate::layers::inputs(&args.layers, &args.inputs, None, reporter) else {
        return false;
    };
    let evaluation = crate::layers::evaluate(&inputs);
    reporter.diagnostics(&inputs.sources, &evaluation.diagnostics);
    !evaluation.has_errors()
}
