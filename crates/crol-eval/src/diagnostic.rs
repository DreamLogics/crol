//! Diagnostics (`SPEC.md` 7.7).

use std::fmt;

use crate::source::{Sources, Span};

/// How serious a diagnostic is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// The data cannot be used.
    Error,
    /// Worth a look, but the data can be used.
    Warning,
}

/// A secondary place a diagnostic refers to, such as the entry a `+`
/// collides with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Related {
    /// Where.
    pub span: Span,
    /// What is there.
    pub message: String,
}

/// An error or warning, with where it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Error or warning.
    pub severity: Severity,
    /// The message, starting with the operator or entry that failed:
    /// `+: cannot add a string to a list`.
    pub message: String,
    /// The primary place.
    pub span: Span,
    /// Secondary places.
    pub related: Vec<Related>,
    /// What was being evaluated: `web.limits.memory_mb`.
    pub context: Option<String>,
}

impl Diagnostic {
    /// An error at `span`.
    pub fn error(span: Span, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
            span,
            related: Vec::new(),
            context: None,
        }
    }

    /// A warning at `span`.
    pub fn warning(span: Span, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            ..Self::error(span, message)
        }
    }

    /// Adds a secondary place.
    pub fn with_related(mut self, span: Span, message: impl Into<String>) -> Self {
        self.related.push(Related {
            span,
            message: message.into(),
        });
        self
    }

    /// Whether this is an error.
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    /// The diagnostic on one line, as `file:line:col: message`, or the
    /// message alone if it is about no place in a file.
    pub fn display<'a>(&'a self, sources: &'a Sources) -> impl fmt::Display + 'a {
        DisplayDiagnostic {
            diagnostic: self,
            sources,
        }
    }
}

struct DisplayDiagnostic<'a> {
    diagnostic: &'a Diagnostic,
    sources: &'a Sources,
}

impl fmt::Display for DisplayDiagnostic<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let d = self.diagnostic;
        if d.severity == Severity::Warning {
            write!(f, "warning: ")?;
        }
        if self.sources.get(d.span.file).is_some() {
            write!(f, "{}: ", self.sources.location(d.span))?;
        }
        write!(f, "{}", d.message)?;
        if let Some(context) = &d.context {
            write!(f, " (while evaluating {context})")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_detached_span_is_displayed_without_a_place() {
        let mut sources = Sources::new();
        let file = sources.add("a.crod", "+a: 1");
        let placed = Diagnostic::error(Span::new(file, Default::default()), "here");
        assert_eq!(placed.display(&sources).to_string(), "a.crod:1:1: here");

        let detached = Diagnostic::error(Span::detached(), "nowhere");
        assert_eq!(detached.display(&sources).to_string(), "nowhere");
        assert_eq!(
            sources.location(Span::detached()).to_string(),
            "<unknown>:1:1"
        );
        assert!(sources.get(Span::detached().file).is_none());
    }
}
