//! `crol check` (`SPEC.md` 11): read and evaluate the layers, and print the
//! diagnostics.

use crate::diagnostics::Reporter;

#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// The layers, lowest first: each `name=source` or a source, where a
    /// source is a module path or a file system path.
    layers: Vec<String>,

    #[command(flatten)]
    variants: crate::layers::VariantArgs,
}

/// Returns whether there were no errors.
pub(crate) fn run(args: &Args, reporter: &mut Reporter) -> bool {
    let Some((sources, evaluation)) =
        crate::layers::evaluate(&args.layers, &args.variants, None, reporter)
    else {
        return false;
    };
    reporter.diagnostics(&sources, &evaluation.diagnostics);
    !evaluation.has_errors()
}
