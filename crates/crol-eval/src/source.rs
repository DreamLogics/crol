//! Source files and spans.

use std::fmt;
use std::sync::Arc;

use crol_syntax::{LineCol, LineIndex, TextRange};

/// Identifies a file in [`Sources`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileId(u32);

/// A range of text in a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    /// The file.
    pub file: FileId,
    /// The byte range in the file.
    pub range: TextRange,
}

impl Span {
    /// Creates a span.
    pub fn new(file: FileId, range: TextRange) -> Self {
        Self { file, range }
    }

    /// A span for values that were not read from a file, such as values
    /// built by a serializer.
    pub fn detached() -> Self {
        Self {
            file: FileId(u32::MAX),
            range: TextRange::default(),
        }
    }

    /// Whether this span is [`Span::detached`].
    pub fn is_detached(&self) -> bool {
        self.file.0 == u32::MAX
    }
}

/// A source file: its name (a path or a name given by the application), its
/// text and an index of its lines.
#[derive(Debug, Clone)]
pub struct SourceFile {
    name: String,
    text: Arc<str>,
    line_index: LineIndex,
}

impl SourceFile {
    /// The file's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The file's text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The line and column of a byte offset.
    pub fn line_col(&self, offset: crol_syntax::TextSize) -> LineCol {
        self.line_index.line_col(&self.text, offset)
    }
}

/// The source files an evaluation reads.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    files: Vec<SourceFile>,
}

impl Sources {
    /// Creates an empty set of sources.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a file.
    pub fn add(&mut self, name: impl Into<String>, text: impl Into<Arc<str>>) -> FileId {
        let text = text.into();
        let id = FileId(u32::try_from(self.files.len()).expect("too many files"));
        self.files.push(SourceFile {
            name: name.into(),
            line_index: LineIndex::new(&text),
            text,
        });
        id
    }

    /// A file.
    ///
    /// # Panics
    ///
    /// If `id` is not from these sources, or is a detached span's.
    pub fn file(&self, id: FileId) -> &SourceFile {
        &self.files[id.0 as usize]
    }

    /// Where a span starts, for messages: `name:line:col`.
    pub fn location(&self, span: Span) -> Location<'_> {
        let file = self.file(span.file);
        Location {
            name: file.name(),
            line_col: file.line_col(span.range.start()),
        }
    }
}

/// A position in a named file, displayed as `name:line:col`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Location<'a> {
    /// The file's name.
    pub name: &'a str,
    /// The line and column.
    pub line_col: LineCol,
}

impl fmt::Display for Location<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}",
            self.name, self.line_col.line, self.line_col.col
        )
    }
}
