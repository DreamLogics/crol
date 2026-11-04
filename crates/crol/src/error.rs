//! The error type of `crol`.

use std::fmt;

use crol_eval::{Diagnostic, Sources, Span};

use crate::loader::SourceId;

/// An error from reading, evaluating, deserializing or serializing CROL.
///
/// An evaluation error carries every diagnostic found, rendered as
/// `file:line:col: message`; a deserialization error says where the value
/// was written and which part of the Rust value it was for:
/// `expected u16, found -1 (config.crod:3:12, while deserializing web.port)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(Box<ErrorImpl>);

#[derive(Debug, Clone, PartialEq, Eq)]
struct ErrorImpl {
    message: String,
    span: Option<Span>,
    /// `file:line:col` of the span.
    location: Option<String>,
    /// The path of the value being deserialized: `web.port`.
    path: Option<String>,
    /// Every diagnostic, for errors from reading and evaluation.
    diagnostics: Vec<String>,
    /// The source that could not be loaded.
    source_id: Option<SourceId>,
}

impl Error {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(Box::new(ErrorImpl {
            message: message.into(),
            span: None,
            location: None,
            path: None,
            diagnostics: Vec::new(),
            source_id: None,
        }))
    }

    /// Adds which source could not be loaded, unless it already says.
    pub(crate) fn with_source_id(mut self, id: SourceId) -> Self {
        self.0.source_id.get_or_insert(id);
        self
    }

    /// Says in which file the error is, unless its location already does.
    pub(crate) fn in_file(mut self, file: &str) -> Self {
        if self.0.location.is_none() && self.0.diagnostics.is_empty() {
            self.0.message = format!("{file}: {}", self.0.message);
        }
        self
    }

    pub(crate) fn from_diagnostics(sources: &Sources, diagnostics: &[Diagnostic]) -> Self {
        let rendered: Vec<String> = diagnostics
            .iter()
            .filter(|d| d.is_error())
            .map(|d| d.display(sources).to_string())
            .collect();
        let mut error = Self::new(rendered.first().cloned().unwrap_or_default());
        error.0.diagnostics = rendered;
        error
    }

    /// Adds where the error happened, unless it already says.
    pub(crate) fn at(
        mut self,
        sources: &Sources,
        span: Span,
        path: impl FnOnce() -> String,
    ) -> Self {
        if self.0.span.is_none() && self.0.diagnostics.is_empty() {
            self.0.span = Some(span);
            if !span.is_detached() {
                self.0.location = Some(sources.location(span).to_string());
            }
            let path = path();
            if !path.is_empty() {
                self.0.path = Some(path);
            }
        }
        self
    }

    /// The message, without the location.
    pub fn message(&self) -> &str {
        &self.0.message
    }

    /// The span of the value the error is about, if it is about one.
    pub fn span(&self) -> Option<Span> {
        self.0.span
    }

    /// The path of the value that was being deserialized: `web.port`.
    pub fn path(&self) -> Option<&str> {
        self.0.path.as_deref()
    }

    /// The source a file loader could not load, for errors from loading.
    pub fn source_id(&self) -> Option<&SourceId> {
        self.0.source_id.as_ref()
    }

    /// Every diagnostic, for errors from reading and evaluation, as
    /// `file:line:col: message`.
    pub fn diagnostics(&self) -> &[String] {
        &self.0.diagnostics
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.0.diagnostics.is_empty() {
            return f.write_str(&self.0.diagnostics.join("\n"));
        }
        f.write_str(&self.0.message)?;
        match (&self.0.location, &self.0.path) {
            (Some(location), Some(path)) => write!(f, " ({location}, while deserializing {path})"),
            (Some(location), None) => write!(f, " ({location})"),
            (None, Some(path)) => write!(f, " (while deserializing {path})"),
            (None, None) => Ok(()),
        }
    }
}

impl std::error::Error for Error {}

impl serde::de::Error for Error {
    fn custom<T: fmt::Display>(message: T) -> Self {
        Self::new(message.to_string())
    }
}

impl serde::ser::Error for Error {
    fn custom<T: fmt::Display>(message: T) -> Self {
        Self::new(message.to_string())
    }
}
