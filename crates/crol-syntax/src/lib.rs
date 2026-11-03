//! Syntax of CROL, the Composable Rust Object Language.
//!
//! This crate holds the lexer, the error-recovering parser, the lossless
//! concrete syntax tree and a typed AST over it (`SPEC.md` sections 2, 9
//! and 12.1).
//!
//! ```
//! use crol_syntax::{ast, ast::AstNode, parse};
//!
//! let parse = parse("+web: Service { replicas: 2, .. },");
//! assert!(parse.errors().is_empty());
//!
//! // The tree is lossless: it prints back to the input.
//! assert_eq!(parse.syntax().to_string(), "+web: Service { replicas: 2, .. },");
//!
//! let entry = parse.document().entries().next().unwrap();
//! let ast::AnyEntry::Entry(entry) = entry else { panic!() };
//! assert_eq!(entry.op(), ast::EntryOp::Declare);
//! assert_eq!(entry.target().unwrap().key().unwrap().syntax().to_string(), "web");
//! ```

#![forbid(unsafe_code)]

pub mod ast;
mod lexer;
mod line_index;
mod parser;
mod syntax_kind;

use std::fmt;

pub use lexer::{Token, tokenize};
pub use line_index::{LineCol, LineIndex};
pub use rowan::{TextRange, TextSize};
pub use syntax_kind::{CrolLanguage, SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};

/// A syntax error: a message and the range of text it is about.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SyntaxError {
    message: String,
    range: TextRange,
}

impl SyntaxError {
    /// Creates an error.
    pub fn new(message: impl Into<String>, range: TextRange) -> Self {
        Self {
            message: message.into(),
            range,
        }
    }

    /// The message, such as ``expected `,` between entries``.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The range of text the error is about. It may be empty, for something
    /// that is missing.
    pub fn range(&self) -> TextRange {
        self.range
    }
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SyntaxError {}

/// The result of parsing a document: a syntax tree, which always exists,
/// and the syntax errors found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parse {
    green: rowan::GreenNode,
    errors: Vec<SyntaxError>,
}

impl Parse {
    /// The root of the syntax tree, a `DOCUMENT` node.
    pub fn syntax(&self) -> SyntaxNode {
        SyntaxNode::new_root(self.green.clone())
    }

    /// The document, as a typed AST node.
    pub fn document(&self) -> ast::Document {
        use ast::AstNode;
        ast::Document::cast(self.syntax()).expect("the root is always a document")
    }

    /// The lexing and parsing errors, in source order.
    pub fn errors(&self) -> &[SyntaxError] {
        &self.errors
    }

    /// The tree as indented text, one node or token per line, for tests and
    /// debugging.
    pub fn debug_tree(&self) -> String {
        let mut out = format!("{:#?}", self.syntax());
        // rowan ends the dump with a newline; keep the output uniform.
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out
    }
}

/// Parses a document. This never fails: the tree holds every byte of `text`,
/// and the problems are in [`Parse::errors`].
pub fn parse(text: &str) -> Parse {
    let (green, errors) = parser::parse(text);
    Parse { green, errors }
}
