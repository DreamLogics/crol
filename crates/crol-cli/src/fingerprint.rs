//! `crol fingerprint` (`SPEC.md` 7.6, 11): print the fingerprint in hex.

use std::io::Write;

use crate::diagnostics::Reporter;

#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// The layers, lowest first.
    layers: Vec<String>,

    #[command(flatten)]
    inputs: crate::layers::InputArgs,
}

/// Returns whether it succeeded.
pub(crate) fn run(args: &Args, reporter: &mut Reporter) -> bool {
    let Some(inputs) = crate::layers::inputs(&args.layers, &args.inputs, None, reporter) else {
        return false;
    };
    let evaluation = crate::layers::evaluate(&inputs);
    reporter.diagnostics(&inputs.sources, &evaluation.diagnostics);
    let Some(base) = &evaluation.value else {
        return false;
    };
    let hash =
        crol_eval::fingerprint::fingerprint(base, &evaluation.contexts, &evaluation.variants);
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    if let Err(error) = writeln!(std::io::stdout().lock(), "{hex}") {
        reporter.error(&format!("cannot write the output: {error}"));
        return false;
    }
    true
}
