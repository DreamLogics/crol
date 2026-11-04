//! The `crol` command-line tool (`SPEC.md` section 11).

#![forbid(unsafe_code)]

mod check;
mod diagnostics;
mod eval;
mod explain;
mod fingerprint;
mod fmt;
mod from_ron;
mod init;
mod layers;
mod ron;
mod value;

use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};

/// Check, evaluate, format and explain CROL data.
#[derive(Debug, Parser)]
#[command(name = "crol", version, arg_required_else_help = true)]
struct Cli {
    /// When to use colours in diagnostics.
    #[arg(long, value_enum, default_value_t = ColorWhen::Auto, global = true)]
    color: ColorWhen,

    /// How to print diagnostics: rendered on standard error, or as JSON
    /// lines on standard output.
    #[arg(long, value_enum, default_value_t = MessageFormat::Human, global = true)]
    message_format: MessageFormat,

    /// Set up the Cargo project around the current directory: write
    /// `project.crov` and a first layer. The same as `crol init`.
    #[arg(long)]
    init: bool,

    #[command(subcommand)]
    command: Option<Command>,
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
    /// Read and evaluate layers, or a `.crov` value document, and print the
    /// diagnostics.
    Check(check::Args),
    /// Print evaluated layers as a data-mode document, a `.crov` value
    /// document as a value, or one value of either.
    Eval(eval::Args),
    /// Print the operations that produced a value, with the value after each.
    Explain(explain::Args),
    /// Print the fingerprint of the evaluated data, or of a `.crov` value
    /// document, in hex.
    Fingerprint(fingerprint::Args),
    /// Format `.crod` and `.crov` files in place, or check that they are
    /// formatted.
    Fmt(fmt::Args),
    /// Convert a RON value file to a CROL value document (a `.crov` file), on
    /// standard output.
    FromRon(from_ron::Args),
    /// Set up the Cargo project around the current directory: write
    /// `project.crov` and a first layer.
    Init,
    /// Run the language server on standard input and output.
    Lsp,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut reporter = diagnostics::Reporter::new(cli.color, cli.message_format);
    let command = match (cli.init, cli.command) {
        (false, Some(command)) => command,
        (true, None) => Command::Init,
        (true, Some(_)) => Cli::command()
            .error(ErrorKind::ArgumentConflict, "`--init` takes no command")
            .exit(),
        (false, None) => Cli::command()
            .error(ErrorKind::MissingSubcommand, "a command is required")
            .exit(),
    };
    let ok = match command {
        Command::Check(args) => check::run(&args, &mut reporter),
        Command::Eval(args) => eval::run(&args, &mut reporter),
        Command::Explain(args) => explain::run(&args, &mut reporter),
        Command::Fingerprint(args) => fingerprint::run(&args, &mut reporter),
        Command::Fmt(args) => fmt::run(&args, &mut reporter),
        Command::FromRon(args) => from_ron::run(&args, &mut reporter),
        Command::Init => init::run(&mut reporter),
        Command::Lsp => match crol_lsp::serve_stdio() {
            Ok(()) => true,
            Err(error) => {
                reporter.error(&format!("language server: {error}"));
                false
            }
        },
    };
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
