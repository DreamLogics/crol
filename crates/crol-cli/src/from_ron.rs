//! `crol from-ron` (`SPEC.md` 11).

use std::io::Write;
use std::path::PathBuf;

use crol_syntax::format::FormatConfig;

use crate::diagnostics::Reporter;

#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// The maximum line width of the output.
    #[arg(long, default_value_t = FormatConfig::default().max_width)]
    max_width: usize,

    /// The RON file to convert.
    file: PathBuf,
}

/// Converts the file and writes the result to standard output. Returns
/// whether it succeeded.
pub(crate) fn run(args: &Args, reporter: &mut Reporter) -> bool {
    let name = args.file.display().to_string();
    let text = match std::fs::read_to_string(&args.file) {
        Ok(text) => text,
        Err(error) => {
            reporter.error(&format!("cannot read {name}: {error}"));
            return false;
        }
    };
    let value = match crate::ron::parse(&text) {
        Ok(value) => value,
        Err(error) => {
            reporter.error(&format!(
                "{name}:{}:{}: {}",
                error.line, error.col, error.message
            ));
            return false;
        }
    };
    let config = FormatConfig::default().with_max_width(args.max_width);
    let document = crol_eval::print::to_document(&value, &config);
    if let Err(error) = std::io::stdout().lock().write_all(document.as_bytes()) {
        reporter.error(&format!("cannot write the output: {error}"));
        return false;
    }
    true
}
