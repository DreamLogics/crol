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

/// A layer: a named, ordered set of documents (`SPEC.md` 7.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layer {
    /// The layer's name, for messages: `base`, `::data::local`.
    pub name: String,
    /// The documents, each with its path relative to the layer (`/`
    /// separators). They are evaluated in the order of their paths.
    pub documents: Vec<(String, FileId)>,
}

/// Options of data evaluation (`SPEC.md` 10.7's `CrolConfig` fields).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DataOptions {
    /// Warn when patches to one key come from several documents of one
    /// layer, since their order then depends on file names (`SPEC.md` 7.2).
    pub warn_cross_file_patches: bool,
}

impl Default for DataOptions {
    fn default() -> Self {
        Self {
            warn_cross_file_patches: true,
        }
    }
}

impl DataOptions {
    /// Sets [`DataOptions::warn_cross_file_patches`].
    pub fn with_warn_cross_file_patches(mut self, warn: bool) -> Self {
        self.warn_cross_file_patches = warn;
        self
    }
}

/// Evaluates layers of data-mode documents (`SPEC.md` 7): later layers
/// apply over earlier ones. The value is the root, a keyed value from root
/// keys to their final values.
pub fn evaluate_data(sources: &Sources, layers: &[Layer], options: &DataOptions) -> Evaluation {
    let mut diagnostics = Vec::new();
    let mut parses = Vec::new();
    for (layer_index, layer) in layers.iter().enumerate() {
        let mut documents: Vec<&(String, FileId)> = layer.documents.iter().collect();
        documents.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        for pair in documents.windows(2) {
            if pair[0].0 == pair[1].0 {
                let span = Span::new(pair[1].1, Default::default());
                diagnostics.push(
                    Diagnostic::error(
                        span,
                        format!(
                            "the layer `{}` has two documents at `{}`",
                            layer.name, pair[1].0
                        ),
                    )
                    .with_related(
                        Span::new(pair[0].1, Default::default()),
                        "the other document",
                    ),
                );
            }
        }
        for (index, (_, file)) in documents.into_iter().enumerate() {
            let parse = crol_syntax::parse(sources.file(*file).text());
            diagnostics.extend(
                parse.errors().iter().map(|error| {
                    Diagnostic::error(Span::new(*file, error.range()), error.message())
                }),
            );
            parses.push((*file, parse.document(), layer_index, index));
        }
    }
    let documents: Vec<lower::DataDocument<'_>> = parses
        .iter()
        .map(|(file, document, layer, index)| lower::DataDocument {
            file: *file,
            document,
            layer: *layer,
            index: *index,
        })
        .collect();
    let (lowered, mut errors) = lower::lower_data(&documents);
    diagnostics.append(&mut errors);
    if diagnostics.iter().any(Diagnostic::is_error) {
        return Evaluation {
            value: None,
            diagnostics,
        };
    }
    let layer_names: Vec<String> = layers.iter().map(|l| l.name.clone()).collect();
    let input = eval::DataInput {
        ops: &lowered.ops,
        templates: &lowered.templates,
        layer_names: &layer_names,
        warn_cross_file_patches: options.warn_cross_file_patches,
    };
    let (value, mut errors) = eval::evaluate_data(&lowered.ir, &input);
    diagnostics.append(&mut errors);
    let value = value.filter(|_| !diagnostics.iter().any(Diagnostic::is_error));
    Evaluation { value, diagnostics }
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
