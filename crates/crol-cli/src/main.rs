//! The `crol` command-line tool (`SPEC.md` section 11).

#![forbid(unsafe_code)]

use std::process::ExitCode;

fn main() -> ExitCode {
    #[expect(clippy::print_stderr, reason = "placeholder until the CLI exists")]
    {
        eprintln!(
            "crol {}: no commands are implemented yet",
            env!("CARGO_PKG_VERSION")
        );
    }
    ExitCode::FAILURE
}
