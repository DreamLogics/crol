//! Printing diagnostics to standard error.

use std::io::{IsTerminal, Write};

use codespan_reporting::diagnostic::{Diagnostic, Label};
use codespan_reporting::files::SimpleFile;
use codespan_reporting::term::termcolor::{ColorChoice, StandardStream};
use codespan_reporting::term::{self, Config};
use crol_syntax::SyntaxError;

use crate::ColorWhen;

/// Writes diagnostics to standard error.
pub(crate) struct Reporter {
    stderr: StandardStream,
    config: Config,
}

impl Reporter {
    pub(crate) fn new(color: ColorWhen) -> Self {
        let choice = match color {
            ColorWhen::Always => ColorChoice::Always,
            ColorWhen::Never => ColorChoice::Never,
            ColorWhen::Auto if std::io::stderr().is_terminal() => ColorChoice::Auto,
            ColorWhen::Auto => ColorChoice::Never,
        };
        Self {
            stderr: StandardStream::stderr(choice),
            config: Config::default(),
        }
    }

    /// Reports the syntax errors of the file `name`, whose text is `text`.
    pub(crate) fn syntax_errors(&mut self, name: &str, text: &str, errors: &[SyntaxError]) {
        let file = SimpleFile::new(name, text);
        for error in errors {
            let range = usize::from(error.range().start())..usize::from(error.range().end());
            let diagnostic = Diagnostic::error()
                .with_message(error.message())
                .with_labels(vec![Label::primary((), range)]);
            // Failing to write to standard error leaves nowhere to report it.
            let _ = term::emit_to_write_style(
                &mut self.stderr.lock(),
                &self.config,
                &file,
                &diagnostic,
            );
        }
    }

    /// Reports an error that is not about a place in a file.
    pub(crate) fn error(&mut self, message: &str) {
        let diagnostic = Diagnostic::<()>::error().with_message(message);
        let file = SimpleFile::new("", "");
        let _ =
            term::emit_to_write_style(&mut self.stderr.lock(), &self.config, &file, &diagnostic);
    }

    /// Writes a line of plain text to standard error.
    pub(crate) fn note(&mut self, message: &str) {
        let _ = writeln!(self.stderr.lock(), "{message}");
    }
}
