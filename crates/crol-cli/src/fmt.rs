//! `crol fmt` (`SPEC.md` 11, 12.2). Formatting does not depend on the mode.

use std::path::PathBuf;

use crol_syntax::format::{FormatConfig, format};

use crate::diagnostics::Reporter;

#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// Check that the files are formatted, without changing them; exit with
    /// status 1 if any is not.
    #[arg(long)]
    check: bool,

    /// The maximum line width.
    #[arg(long, default_value_t = FormatConfig::default().max_width)]
    max_width: usize,

    /// The files to format: `.crod` and `.crov`.
    #[arg(required = true)]
    files: Vec<PathBuf>,
}

/// Formats the files. Returns whether every file was formatted (or, with
/// `--check`, already was).
pub(crate) fn run(args: &Args, reporter: &mut Reporter) -> bool {
    let config = FormatConfig::default().with_max_width(args.max_width);
    let mut ok = true;
    for path in &args.files {
        let name = path.display().to_string();
        // It states no mode, so it is refused with the choice of both.
        if crol::is_retired(&name) {
            reporter.error(&format!(
                "{name}: the `.crol` extension is retired; \
                 rename it to `.crod` if it is data, or to `.crov` if it is a value document"
            ));
            ok = false;
            continue;
        }
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) => {
                reporter.error(&format!("cannot read {name}: {error}"));
                ok = false;
                continue;
            }
        };
        let Ok(text) = String::from_utf8(bytes) else {
            reporter.error(&format!("{name} is not valid UTF-8"));
            ok = false;
            continue;
        };
        let formatted = match format(&text, &config) {
            Ok(formatted) => formatted,
            Err(errors) => {
                reporter.syntax_errors(&name, &text, &errors);
                ok = false;
                continue;
            }
        };
        if formatted == text {
            continue;
        }
        if args.check {
            reporter.note(&format!("{name} is not formatted"));
            ok = false;
        } else if let Err(error) = std::fs::write(path, formatted) {
            reporter.error(&format!("cannot write {name}: {error}"));
            ok = false;
        }
    }
    ok
}
