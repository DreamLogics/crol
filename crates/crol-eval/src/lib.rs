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
//! let file = sources.add("config.crov", "port := 8080, listen: \"0.0.0.0:\" + &port");
//! let evaluation = evaluate_value(&sources, file);
//! let value = evaluation.value.unwrap();
//! let ValueKind::Keyed(keyed) = &value.kind else { panic!() };
//! let listen = &keyed.entries[&crol_eval::Key::Str("listen".into())];
//! assert!(matches!(&listen.kind, ValueKind::String(s) if s == "0.0.0.0:8080"));
//! ```

#![forbid(unsafe_code)]

mod cond;
mod diagnostic;
mod eval;
pub mod fingerprint;
mod ir;
mod literal;
mod lower;
pub mod print;
mod source;
mod value;
mod variants;

pub use diagnostic::{Diagnostic, Related, Severity};
pub use source::{FileId, Location, SourceFile, Sources, Span};
pub use value::{Key, Keyed, Value, ValueKind, format_float, is_identifier, quote, quote_char};
pub use variants::{Context, VariantId, Variants};

pub use eval::{
    ContextData, DefaultsProvider, ExplainStep, PathStep, StepKind, VariantKeys, value_at_path,
};

/// The result of evaluating: the value, unless an error made that
/// impossible, and every diagnostic.
#[derive(Debug, Clone, Default)]
pub struct Evaluation {
    /// The value, in the base context; `None` if there were errors.
    pub value: Option<Value>,
    /// How every other evaluated context differs from the base, for data
    /// with variants.
    pub contexts: Vec<ContextData>,
    /// The variants: those declared, or else those the data uses.
    pub variants: Variants,
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
    /// The variants (`SPEC.md` 8.1). If none are declared, every name the
    /// data uses is a `Discrete` variant.
    pub variants: Variants,
    /// The contexts to evaluate besides the base, each by its chosen
    /// variants; `None` for every valid context.
    pub contexts: Option<Vec<Vec<String>>>,
    /// The most contexts evaluated when `contexts` is `None`.
    pub max_contexts: usize,
    /// Which keys a context may have, compared with the base (`SPEC.md` 8.5).
    pub variant_keys: VariantKeys,
    /// Evaluate every key in every context, rather than only those that can
    /// differ from the base. For testing that the two agree.
    #[doc(hidden)]
    pub evaluate_every_key: bool,
    /// Where `&default` finds defaults (`SPEC.md` 6.1.3); without one,
    /// `&default` is an error.
    pub defaults: Option<Defaults>,
}

/// A shared [`DefaultsProvider`], for [`DataOptions::defaults`].
#[derive(Debug, Clone)]
pub struct Defaults(pub std::sync::Arc<dyn DefaultsProvider>);

impl PartialEq for Defaults {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for Defaults {}

impl Default for DataOptions {
    fn default() -> Self {
        Self {
            warn_cross_file_patches: true,
            variants: Variants::new(),
            contexts: None,
            max_contexts: 4096,
            variant_keys: VariantKeys::Any,
            evaluate_every_key: false,
            defaults: None,
        }
    }
}

impl DataOptions {
    /// Sets [`DataOptions::warn_cross_file_patches`].
    pub fn with_warn_cross_file_patches(mut self, warn: bool) -> Self {
        self.warn_cross_file_patches = warn;
        self
    }

    /// Sets [`DataOptions::variants`].
    pub fn with_variants(mut self, variants: Variants) -> Self {
        self.variants = variants;
        self
    }

    /// Sets [`DataOptions::contexts`].
    pub fn with_contexts(mut self, contexts: Option<Vec<Vec<String>>>) -> Self {
        self.contexts = contexts;
        self
    }

    /// Sets [`DataOptions::max_contexts`].
    pub fn with_max_contexts(mut self, max_contexts: usize) -> Self {
        self.max_contexts = max_contexts;
        self
    }

    /// Sets [`DataOptions::defaults`].
    pub fn with_defaults(mut self, defaults: Option<Defaults>) -> Self {
        self.defaults = defaults;
        self
    }

    /// Sets [`DataOptions::variant_keys`].
    pub fn with_variant_keys(mut self, variant_keys: VariantKeys) -> Self {
        self.variant_keys = variant_keys;
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
            diagnostics,
            ..Evaluation::default()
        };
    }
    let layer_names: Vec<String> = layers.iter().map(|l| l.name.clone()).collect();
    let input = eval::DataInput {
        ops: &lowered.ops,
        templates: &lowered.templates,
        layer_names: &layer_names,
        warn_cross_file_patches: options.warn_cross_file_patches,
        variants: &options.variants,
        contexts: options.contexts.as_deref(),
        max_contexts: options.max_contexts,
        variant_keys: options.variant_keys,
        evaluate_every_key: options.evaluate_every_key,
        defaults: options.defaults.as_ref().map(|d| &*d.0),
    };
    let (result, mut errors) = eval::evaluate_data(&lowered.ir, &input);
    diagnostics.append(&mut errors);
    let Some(result) = result else {
        return Evaluation {
            diagnostics,
            ..Evaluation::default()
        };
    };
    if diagnostics.iter().any(Diagnostic::is_error) {
        return Evaluation {
            variants: result.variants,
            diagnostics,
            ..Evaluation::default()
        };
    }
    Evaluation {
        value: result.base,
        contexts: result.contexts,
        variants: result.variants,
        diagnostics,
    }
}

/// The root operations of one data-mode document, each with its target's
/// span and whether it is a definition, a patch or a remove (`SPEC.md`
/// 7.2), for editors. The document is read on its own: names it uses from
/// other documents do not change what its operations are.
pub fn operations(sources: &Sources, file: FileId) -> Vec<(Span, StepKind)> {
    let parse = crol_syntax::parse(sources.file(file).text());
    let document = parse.document();
    let (lowered, _) = lower::lower_data(&[lower::DataDocument {
        file,
        document: &document,
        layer: 0,
        index: 0,
    }]);
    lowered.ops.iter().map(|op| (op.span, op.kind())).collect()
}

/// Parses a path to a value: a root key, then selectors, as `web.limits.cpu`
/// or `web.tags[0]` (`SPEC.md` 5.1).
pub fn parse_path(text: &str) -> Result<(String, Vec<PathStep>), String> {
    let invalid = || format!("`{text}` is not a key or a path");
    let parse = crol_syntax::parse(&format!("{text}: ()"));
    let entry = match parse.document().entries().next() {
        Some(crol_syntax::ast::AnyEntry::Entry(entry)) if parse.errors().is_empty() => entry,
        _ => return Err(invalid()),
    };
    let target = entry.target().ok_or_else(invalid)?;
    let key = target.key().and_then(|k| k.token()).ok_or_else(invalid)?;
    let key = match key.kind() {
        crol_syntax::SyntaxKind::IDENT => key.text().to_owned(),
        crol_syntax::SyntaxKind::STRING | crol_syntax::SyntaxKind::RAW_STRING => {
            literal::string(key.text())
        }
        _ => return Err(invalid()),
    };
    let mut path = Vec::new();
    for selector in target.selectors() {
        path.push(match &selector {
            crol_syntax::ast::Selector::Field(field) => {
                PathStep::Field(field.name().ok_or_else(invalid)?.text().to_owned())
            }
            crol_syntax::ast::Selector::Index(index) => PathStep::Element(
                index
                    .index()
                    .ok_or_else(invalid)?
                    .text()
                    .parse()
                    .map_err(|_| invalid())?,
            ),
            crol_syntax::ast::Selector::Accessor(accessor) => {
                let token = accessor.key().ok_or_else(invalid)?;
                match token.kind() {
                    crol_syntax::SyntaxKind::INT => PathStep::Accessor(
                        literal::int(token.text(), accessor.is_negative())
                            .map_err(|_| invalid())?,
                    ),
                    _ => PathStep::Field(literal::string(token.text())),
                }
            }
            _ => return Err(invalid()),
        });
    }
    Ok((key, path))
}

/// The operations that produced the value at `path` (`web.limits.cpu`) in
/// the context of the `context` variants, in order, with the value after
/// each (`SPEC.md` 12.3). Fails with the diagnostics if the data has errors.
pub fn explain(
    sources: &Sources,
    layers: &[Layer],
    options: &DataOptions,
    path: &str,
    context: &[String],
) -> Result<Vec<ExplainStep>, Vec<Diagnostic>> {
    let (key, steps) =
        parse_path(path).map_err(|message| vec![Diagnostic::error(Span::detached(), message)])?;
    let mut found = explain_paths(sources, layers, options, &[(&key, &steps)], context)?;
    Ok(found.pop().unwrap_or_default())
}

/// The value of the root key after each root operation in `file`, in the
/// context of the `context` variants, as [`explain`] gives it: for editors,
/// which show what an entry computes. Operations whose chain fails have no
/// value. Fails with the diagnostics if the data has errors.
pub fn operation_values(
    sources: &Sources,
    layers: &[Layer],
    options: &DataOptions,
    file: FileId,
    context: &[String],
) -> Result<Vec<(Span, Option<Value>)>, Vec<Diagnostic>> {
    let parse = crol_syntax::parse(sources.file(file).text());
    let document = parse.document();
    let (lowered, _) = lower::lower_data(&[lower::DataDocument {
        file,
        document: &document,
        layer: 0,
        index: 0,
    }]);
    let mut keys: Vec<&str> = lowered.ops.iter().map(|op| &*op.key).collect();
    keys.sort_unstable();
    keys.dedup();
    let requests: Vec<(&str, &[PathStep])> = keys.iter().map(|k| (*k, &[][..])).collect();
    let found = explain_paths(sources, layers, options, &requests, context)?;
    Ok(found
        .into_iter()
        .flatten()
        .filter(|step| step.span.file == file)
        .map(|step| (step.span, step.value))
        .collect())
}

fn explain_paths(
    sources: &Sources,
    layers: &[Layer],
    options: &DataOptions,
    requests: &[(&str, &[PathStep])],
    context: &[String],
) -> Result<Vec<Vec<ExplainStep>>, Vec<Diagnostic>> {
    let evaluation = evaluate_data(sources, layers, options);
    if evaluation.has_errors() {
        return Err(evaluation.diagnostics);
    }
    let mut parses = Vec::new();
    for (layer_index, layer) in layers.iter().enumerate() {
        let mut documents: Vec<&(String, FileId)> = layer.documents.iter().collect();
        documents.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        for (index, (_, file)) in documents.into_iter().enumerate() {
            parses.push((
                *file,
                crol_syntax::parse(sources.file(*file).text()).document(),
                layer_index,
                index,
            ));
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
    let (lowered, _) = lower::lower_data(&documents);
    let layer_names: Vec<String> = layers.iter().map(|l| l.name.clone()).collect();
    let input = eval::DataInput {
        ops: &lowered.ops,
        templates: &lowered.templates,
        layer_names: &layer_names,
        warn_cross_file_patches: false,
        variants: &evaluation.variants,
        contexts: options.contexts.as_deref(),
        max_contexts: options.max_contexts,
        variant_keys: options.variant_keys,
        evaluate_every_key: false,
        defaults: options.defaults.as_ref().map(|d| &*d.0),
    };
    eval::explain(&lowered.ir, &input, requests, context)
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
            diagnostics,
            ..Evaluation::default()
        };
    }
    let (lowered, mut diagnostics) = lower::lower_value_document(&parse.document(), file);
    if diagnostics.iter().any(Diagnostic::is_error) {
        return Evaluation {
            diagnostics,
            ..Evaluation::default()
        };
    }
    let (value, mut errors) = eval::evaluate(&lowered.ir, lowered.root);
    diagnostics.append(&mut errors);
    let value = value.filter(|_| !diagnostics.iter().any(Diagnostic::is_error));
    Evaluation {
        value,
        diagnostics,
        ..Evaluation::default()
    }
}
