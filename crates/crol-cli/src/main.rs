//! The `crol` command-line tool (`SPEC.md` section 11).

#![forbid(unsafe_code)]

mod check;
mod diagnostics;
mod eval;
mod fingerprint;
mod fmt;
mod from_ron;
mod layers;
mod ron;

use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};

/// Check, evaluate, format and explain CROL data.
#[derive(Debug, Parser)]
#[command(name = "crol", version)]
struct Cli {
    /// When to use colours in diagnostics.
    #[arg(long, value_enum, default_value_t = ColorWhen::Auto, global = true)]
    color: ColorWhen,

    /// How to print diagnostics: rendered on standard error, or as JSON
    /// lines on standard output.
    #[arg(long, value_enum, default_value_t = MessageFormat::Human, global = true)]
    message_format: MessageFormat,

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum MessageFormat {
    Human,
    Json,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Read and evaluate layers, and print the diagnostics.
    Check(check::Args),
    /// Print evaluated data as a data-mode document, or one value.
    Eval(eval::Args),
    /// Print the fingerprint of the evaluated data in hex.
    Fingerprint(fingerprint::Args),
    /// Format files in place, or check that they are formatted.
    Fmt(fmt::Args),
    /// Convert a RON value file to a CROL value document, on standard output.
    FromRon(from_ron::Args),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut reporter = diagnostics::Reporter::new(cli.color, cli.message_format);
    let ok = match cli.command {
        Command::Check(args) => check::run(&args, &mut reporter),
        Command::Eval(args) => eval::run(&args, &mut reporter),
        Command::Fingerprint(args) => fingerprint::run(&args, &mut reporter),
        Command::Fmt(args) => fmt::run(&args, &mut reporter),
        Command::FromRon(args) => from_ron::run(&args, &mut reporter),
    };
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
