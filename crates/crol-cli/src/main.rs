//! The `crol` command-line tool (`SPEC.md` section 11).

#![forbid(unsafe_code)]

mod diagnostics;
mod fmt;

use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};

/// Check, evaluate, format and explain CROL data.
#[derive(Debug, Parser)]
#[command(name = "crol", version)]
struct Cli {
    /// When to use colours in diagnostics.
    #[arg(long, value_enum, default_value_t = ColorWhen::Auto, global = true)]
    color: ColorWhen,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ColorWhen {
    /// When standard error is a terminal.
    Auto,
    Always,
    Never,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Format files in place, or check that they are formatted.
    Fmt(fmt::Args),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut reporter = diagnostics::Reporter::new(cli.color);
    let ok = match cli.command {
        Command::Fmt(args) => fmt::run(&args, &mut reporter),
    };
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
