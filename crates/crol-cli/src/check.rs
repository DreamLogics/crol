//! `crol check` (`SPEC.md` 11): read and evaluate the layers, and print the
//! diagnostics.

use crate::diagnostics::Reporter;

#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// The layers, lowest first: each `name=source` or a source, where a
    /// source is a module path or a file system path. Default: the project
    /// file's. One `.crol` file is read as a value document instead.
    layers: Vec<String>,

    #[command(flatten)]
    inputs: crate::layers::InputArgs,
}

/// Returns whether there were no errors.
pub(crate) fn run(args: &Args, reporter: &mut Reporter) -> bool {
    if let Some(path) = crate::value::argument(&args.layers) {
        let Some(document) = crate::value::read(path, &args.inputs, reporter) else {
            return false;
        };
        reporter.diagnostics(&document.sources, &document.evaluation.diagnostics);
        return !document.evaluation.has_errors();
    }
    let Some(inputs) = crate::layers::inputs(&args.layers, &args.inputs, None, reporter) else {
        return false;
    };
    let evaluation = crate::layers::evaluate(&inputs);
    reporter.diagnostics(&inputs.sources, &evaluation.diagnostics);
    !evaluation.has_errors()
}
