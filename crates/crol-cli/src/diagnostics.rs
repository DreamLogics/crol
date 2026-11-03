//! Printing diagnostics: rendered with source snippets on standard error,
//! or as JSON lines on standard output (`--message-format json`).

use std::io::{IsTerminal, Write};

use codespan_reporting::diagnostic::{Diagnostic, Label, Severity};
use codespan_reporting::files::SimpleFiles;
use codespan_reporting::term::termcolor::{ColorChoice, StandardStream};
use codespan_reporting::term::{self, Config};
use crol_eval::Sources;
use crol_syntax::SyntaxError;

use crate::{ColorWhen, MessageFormat};

/// Writes diagnostics.
pub(crate) struct Reporter {
    stderr: StandardStream,
    config: Config,
    format: MessageFormat,
}

impl Reporter {
    pub(crate) fn new(color: ColorWhen, format: MessageFormat) -> Self {
        let choice = match color {
            ColorWhen::Always => ColorChoice::Always,
            ColorWhen::Never => ColorChoice::Never,
            ColorWhen::Auto if std::io::stderr().is_terminal() => ColorChoice::Auto,
            ColorWhen::Auto => ColorChoice::Never,
        };
        Self {
            stderr: StandardStream::stderr(choice),
            config: Config::default(),
            format,
        }
    }

    /// Reports the syntax errors of the file `name`, whose text is `text`.
    pub(crate) fn syntax_errors(&mut self, name: &str, text: &str, errors: &[SyntaxError]) {
        let mut sources = Sources::new();
        let file = sources.add(name, text);
        let diagnostics: Vec<_> = errors
            .iter()
            .map(|e| {
                crol_eval::Diagnostic::error(crol_eval::Span::new(file, e.range()), e.message())
            })
            .collect();
        self.diagnostics(&sources, &diagnostics);
    }

    /// Reports evaluation diagnostics.
    pub(crate) fn diagnostics(&mut self, sources: &Sources, diagnostics: &[crol_eval::Diagnostic]) {
        if self.format == MessageFormat::Json {
            let mut stdout = std::io::stdout().lock();
            for diagnostic in diagnostics {
                let _ = writeln!(stdout, "{}", json(sources, diagnostic));
            }
            return;
        }
        let mut files = SimpleFiles::new();
        for (_, file) in sources.files() {
            files.add(file.name().to_owned(), file.text().to_owned());
        }
        for diagnostic in diagnostics {
            let range = |span: crol_eval::Span| {
                usize::from(span.range.start())..usize::from(span.range.end())
            };
            let mut labels = vec![Label::primary(
                diagnostic.span.file.index(),
                range(diagnostic.span),
            )];
            for related in &diagnostic.related {
                labels.push(
                    Label::secondary(related.span.file.index(), range(related.span))
                        .with_message(&related.message),
                );
            }
            let severity = match diagnostic.severity {
                crol_eval::Severity::Error => Severity::Error,
                crol_eval::Severity::Warning => Severity::Warning,
            };
            let mut rendered = Diagnostic::new(severity)
                .with_message(&diagnostic.message)
                .with_labels(labels);
            if let Some(context) = &diagnostic.context {
                rendered = rendered.with_notes(vec![format!("while evaluating {context}")]);
            }
            // Failing to write to standard error leaves nowhere to report it.
            let _ =
                term::emit_to_write_style(&mut self.stderr.lock(), &self.config, &files, &rendered);
        }
    }

    /// Reports an error that is not about a place in a file.
    pub(crate) fn error(&mut self, message: &str) {
        if self.format == MessageFormat::Json {
            let _ = writeln!(
                std::io::stdout().lock(),
                "{{\"severity\":\"error\",\"message\":{}}}",
                json_string(message)
            );
            return;
        }
        let diagnostic = Diagnostic::<()>::error().with_message(message);
        let files = codespan_reporting::files::SimpleFile::new("", "");
        let _ =
            term::emit_to_write_style(&mut self.stderr.lock(), &self.config, &files, &diagnostic);
    }

    /// Writes a line of plain text to standard error.
    pub(crate) fn note(&mut self, message: &str) {
        let _ = writeln!(self.stderr.lock(), "{message}");
    }
}

/// A diagnostic as one line of JSON.
fn json(sources: &Sources, diagnostic: &crol_eval::Diagnostic) -> String {
    let place = |span: crol_eval::Span| {
        let file = sources.file(span.file);
        let start = file.line_col(span.range.start());
        let end = file.line_col(span.range.end());
        format!(
            "\"file\":{},\"line\":{},\"column\":{},\"end_line\":{},\"end_column\":{}",
            json_string(file.name()),
            start.line,
            start.col,
            end.line,
            end.col
        )
    };
    let severity = match diagnostic.severity {
        crol_eval::Severity::Error => "error",
        crol_eval::Severity::Warning => "warning",
    };
    let related: Vec<String> = diagnostic
        .related
        .iter()
        .map(|r| {
            format!(
                "{{{},\"message\":{}}}",
                place(r.span),
                json_string(&r.message)
            )
        })
        .collect();
    let context = diagnostic
        .context
        .as_deref()
        .map_or_else(|| "null".to_owned(), json_string);
    format!(
        "{{\"severity\":\"{severity}\",\"message\":{},{},\"context\":{context},\"related\":[{}]}}",
        json_string(&diagnostic.message),
        place(diagnostic.span),
        related.join(",")
    )
}

fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
