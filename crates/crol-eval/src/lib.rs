//! The CROL evaluator: lowering documents to operations, evaluation,
//! [`Value`] and diagnostics.
//!
//! This crate is the engine behind the `crol` crate, which most users want
//! instead. It evaluates value-mode documents (`SPEC.md` 4) so far; layers,
//! variants and fingerprints come in later milestones.
//!
//! ```
//! use crol_eval::{Sources, ValueKind, evaluate_value};
//!
//! let mut sources = Sources::new();
//! let file = sources.add("config.crol", "port := 8080, listen: \"0.0.0.0:\" + &port");
//! let evaluation = evaluate_value(&sources, file);
//! let value = evaluation.value.unwrap();
//! let ValueKind::Keyed(keyed) = &value.kind else { panic!() };
//! let listen = &keyed.entries[&crol_eval::Key::Str("listen".into())];
//! assert!(matches!(&listen.kind, ValueKind::String(s) if s == "0.0.0.0:8080"));
//! ```

#![forbid(unsafe_code)]

mod diagnostic;
mod eval;
mod ir;
mod literal;
mod lower;
pub mod print;
mod source;
mod value;

pub use diagnostic::{Diagnostic, Related, Severity};
pub use source::{FileId, Location, SourceFile, Sources, Span};
pub use value::{Key, Keyed, Value, ValueKind, format_float, is_identifier, quote, quote_char};

/// The result of evaluating a document: its value, unless an error made
/// that impossible, and every diagnostic.
#[derive(Debug, Clone)]
pub struct Evaluation {
    /// The value; `None` if there were errors.
    pub value: Option<Value>,
    /// Errors and warnings, in the order found.
    pub diagnostics: Vec<Diagnostic>,
}

impl Evaluation {
    /// Whether any diagnostic is an error.
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(Diagnostic::is_error)
    }
}

/// Evaluates a file as a value-mode document (`SPEC.md` 4): one value, with
/// no layers. Syntax errors stop evaluation; other errors are all reported.
pub fn evaluate_value(sources: &Sources, file: FileId) -> Evaluation {
    let parse = crol_syntax::parse(sources.file(file).text());
    if !parse.errors().is_empty() {
        let diagnostics = parse
            .errors()
            .iter()
            .map(|error| Diagnostic::error(Span::new(file, error.range()), error.message()))
            .collect();
        return Evaluation {
            value: None,
            diagnostics,
        };
    }
    let (lowered, mut diagnostics) = lower::lower_value_document(&parse.document(), file);
    if diagnostics.iter().any(Diagnostic::is_error) {
        return Evaluation {
            value: None,
            diagnostics,
        };
    }
    let (value, mut errors) = eval::evaluate(&lowered.ir, lowered.root);
    diagnostics.append(&mut errors);
    let value = value.filter(|_| !diagnostics.iter().any(Diagnostic::is_error));
    Evaluation { value, diagnostics }
}
