//! `crol fingerprint` (`SPEC.md` 7.6, 11): print the fingerprint in hex.

use std::io::Write;

use crate::diagnostics::Reporter;

#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// The layers, lowest first; or one `.crov` file, a value document.
    layers: Vec<String>,

    #[command(flatten)]
    inputs: crate::layers::InputArgs,
}

/// Returns whether it succeeded.
pub(crate) fn run(args: &Args, reporter: &mut Reporter) -> bool {
    if let Some(path) = crate::value::argument(&args.layers) {
        let Some(document) = crate::value::read(path, &args.inputs, reporter) else {
            return false;
        };
        reporter.diagnostics(&document.sources, &document.evaluation.diagnostics);
        return match &document.evaluation.value {
            Some(value) if !document.evaluation.has_errors() => {
                write(&crol_eval::fingerprint::fingerprint_value(value), reporter)
            }
            _ => false,
        };
    }
    let Some(inputs) = crate::layers::inputs(&args.layers, &args.inputs, None, reporter) else {
        return false;
    };
    let evaluation = crate::layers::evaluate(&inputs);
    reporter.diagnostics(&inputs.sources, &evaluation.diagnostics);
    if evaluation.has_errors() {
        return false;
    }
    let Some(base) = &evaluation.value else {
        return false;
    };
    let hash =
        crol_eval::fingerprint::fingerprint(base, &evaluation.contexts, &evaluation.variants);
    write(&hash, reporter)
}

fn write(hash: &[u8; 32], reporter: &mut Reporter) -> bool {
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    if let Err(error) = writeln!(std::io::stdout().lock(), "{hex}") {
        reporter.error(&format!("cannot write the output: {error}"));
        return false;
    }
    true
}
