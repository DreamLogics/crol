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
//! assert!(matches!(&listen.kind, ValueKind::String(s) if &**s == "0.0.0.0:8080"));
//! ```

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;

pub mod binary;
mod cond;
mod diagnostic;
mod eval;
pub mod fingerprint;
mod ir;
mod literal;
mod lower;
mod order;
pub mod print;
mod source;
mod value;
mod variants;

pub use diagnostic::{Diagnostic, Related, Severity};
pub use order::{LayerTarget, layer_target};
pub use source::{FileId, Location, SourceFile, Sources, Span};
pub use value::{
    IdPath, Key, Keyed, Value, ValueKind, format_float, is_identifier, quote, quote_char,
};
pub use variants::{Context, VariantId, Variants};

pub use eval::{
    ContextData, DefaultsProvider, ExplainStep, PathStep, ScopeStep, StepKind, VariantKeys,
    single_payload_value, value_at_path,
};

/// What a built-in macro's reader needs to know (`SPEC.md` 6.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacroHelp {
    /// How a call is written, with its parameters: `clamp!(x, min, max)`.
    pub signature: &'static str,
    /// What the macro does, in Markdown.
    pub doc: &'static str,
}

/// The help of the built-in macro called `name`, without its `!`.
pub fn macro_help(name: &str) -> Option<MacroHelp> {
    let kind = ir::MacroKind::from_name(name)?;
    Some(MacroHelp {
        signature: kind.signature(),
        doc: kind.doc(),
    })
}

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
    /// separators). They are evaluated folder by folder (`SPEC.md` 7.1),
    /// whatever order they are given in.
    pub documents: Vec<(String, FileId)>,
}

/// Options of data evaluation (`SPEC.md` 10.7's `CrolConfig` fields).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DataOptions {
    /// The variants (`SPEC.md` 8.1). If none are declared, every name the
    /// data uses is a `Discrete` variant.
    pub variants: Variants,
    /// The contexts to evaluate besides the base, each by its chosen
    /// variants; `None` for every valid context.
    pub contexts: Option<Vec<Vec<String>>>,
    /// When `contexts` is `None`, evaluate the base and one context for each
    /// variant, rather than every combination (`SPEC.md` 8.1).
    pub each_variant: bool,
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
    /// Let a path go through a field left out, from its default
    /// (`SPEC.md` 5.1). Needs `defaults`.
    pub init_defaults: bool,
    /// Merge a block at a root key with the value below, instead of
    /// replacing it, as a `schema.crod` does (`SPEC.md` 10.12).
    pub merge_root: bool,
    /// How many threads evaluate the contexts after the base, each its
    /// share of them. `None` uses as many as the machine has, up to 3, when
    /// there is enough work (at least 20 000 expressions); `Some(1)` uses
    /// none. The result is the same either way; more threads cost memory,
    /// as each holds its own cells, and computes again what its contexts
    /// need of the base.
    pub threads: Option<usize>,
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
            variants: Variants::new(),
            contexts: None,
            each_variant: false,
            max_contexts: 4096,
            variant_keys: VariantKeys::Any,
            evaluate_every_key: false,
            defaults: None,
            init_defaults: false,
            merge_root: false,
            threads: None,
        }
    }
}

/// The most threads [`DataOptions::threads`] uses on its own: where
/// PERFORMANCE.md measured the most speed for the least memory.
const MAX_EVAL_THREADS: usize = 3;

/// How many expressions there must be before [`DataOptions::threads`] uses
/// threads on its own: below this, starting them costs more than they save.
const THREADED_EXPRS: usize = 20_000;

impl DataOptions {
    /// Sets [`DataOptions::threads`].
    pub fn with_threads(mut self, threads: Option<usize>) -> Self {
        self.threads = threads;
        self
    }

    /// Sets [`DataOptions::merge_root`].
    pub fn with_merge_root(mut self, merge: bool) -> Self {
        self.merge_root = merge;
        self
    }

    /// Sets [`DataOptions::each_variant`].
    pub fn with_each_variant(mut self, each: bool) -> Self {
        self.each_variant = each;
        self
    }

    /// Sets [`DataOptions::init_defaults`].
    pub fn with_init_defaults(mut self, init: bool) -> Self {
        self.init_defaults = init;
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

/// The document at the root of a layer that patches the schema, not the data
/// (`SPEC.md` 10.12).
pub const SCHEMA_DOCUMENT: &str = "schema.crod";

/// Layers read and lowered, ready to be evaluated in any context, without
/// reading their text again. Share one between threads with an `Arc`.
#[derive(Debug)]
pub struct Compiled {
    lowered: lower::LoweredData,
    layer_names: Vec<String>,
    /// For each layer, which of its documents are in order other than by
    /// name (`SPEC.md` 7.1).
    orders: Vec<order::LayerOrder>,
    /// For each layer, its documents in order.
    documents: Vec<Vec<FileId>>,
    /// What the layers decoded from bytes gave, which the IR holds frozen.
    decoded: Option<Arc<Decoded>>,
    /// What each document of the layers from text lowered to on its own, so
    /// that compiling again lowers only the documents that changed.
    texts: HashMap<FileId, Arc<lower::LoweredDocument>>,
}

/// What layers decoded from bytes gave, so that compiling again with the
/// same bytes for the same documents need not decode them.
#[derive(Debug)]
struct Decoded {
    /// Each such layer: its index, its documents, and where its bytes are
    /// and how many.
    from: Vec<(usize, Vec<FileId>, usize, usize)>,
    parts: Vec<binary::Part>,
}

/// Evaluates layers of data-mode documents (`SPEC.md` 7): later layers
/// apply over earlier ones. The value is the root, a keyed value from root
/// keys to their final values.
pub fn evaluate_data(sources: &Sources, layers: &[Layer], options: &DataOptions) -> Evaluation {
    let (compiled, mut diagnostics) = compile(sources, layers);
    let Some(compiled) = compiled else {
        return Evaluation {
            diagnostics,
            ..Evaluation::default()
        };
    };
    let mut evaluation = evaluate_compiled(&compiled, options);
    diagnostics.append(&mut evaluation.diagnostics);
    evaluation.diagnostics = diagnostics;
    evaluation
}

/// Reads and lowers layers of data-mode documents, with every diagnostic
/// found; `None` if there are errors.
pub fn compile(sources: &Sources, layers: &[Layer]) -> (Option<Compiled>, Vec<Diagnostic>) {
    compile_documents(sources, layers, false, &[], None)
}

/// [`compile`], where `lowered` holds, for each layer that has one, what its
/// `.crob` file says, so that its documents are not parsed or lowered. The
/// layer's documents must be those of the file.
pub fn compile_binary(
    sources: &Sources,
    layers: &[Layer],
    lowered: &[Option<&[u8]>],
) -> (Option<Compiled>, Vec<Diagnostic>) {
    compile_documents(sources, layers, false, lowered, None)
}

/// [`compile_binary`], after `before` was compiled: what it decoded from
/// the same bytes, for the same documents, is not decoded again, and the
/// two share it.
pub fn compile_binary_after(
    sources: &Sources,
    layers: &[Layer],
    lowered: &[Option<&[u8]>],
    before: &Compiled,
) -> (Option<Compiled>, Vec<Diagnostic>) {
    compile_documents(sources, layers, false, lowered, Some(before))
}

/// The `.crob` file of a layer (`SPEC.md` 7.8). It must evaluate on its own
/// as far as its templates go: one of another layer is not known to it.
pub fn encode_layer(sources: &Sources, layer: &Layer) -> Result<Vec<u8>, Vec<Diagnostic>> {
    let layers = std::slice::from_ref(layer);
    let (compiled, diagnostics) = compile(sources, layers);
    let Some(compiled) = compiled else {
        return Err(diagnostics);
    };
    let all = documents_of(sources, layer, true);
    let files: Vec<FileId> = all.iter().map(|(_, file)| *file).collect();
    let documents: Vec<binary::BinaryDocument> = all
        .iter()
        .map(|(relative, file)| binary::BinaryDocument {
            relative: relative.clone(),
            display: sources.file(*file).name().to_owned(),
            text: sources.file(*file).text().to_owned(),
        })
        .collect();
    Ok(binary::write_file(
        &documents,
        &binary::write_lowered(&compiled.lowered, &files),
    ))
}

/// What a layer lowers to on its own, as a `.crob` file holds it, for
/// [`compile_binary`]; `None` if it does not stand alone: it uses a template of
/// another layer, or there is something to say about it (`SPEC.md` 7.8).
pub fn lower_layer(sources: &Sources, layer: &Layer) -> Option<Vec<u8>> {
    let (compiled, diagnostics) = compile(sources, std::slice::from_ref(layer));
    let compiled = compiled.filter(|_| diagnostics.is_empty())?;
    let all = documents_of(sources, layer, true);
    let files: Vec<FileId> = all.iter().map(|(_, file)| *file).collect();
    Some(binary::write_lowered(&compiled.lowered, &files))
}

/// [`compile`], for a layer's `schema.crod` too, which is otherwise left out.
pub fn compile_schema_documents(
    sources: &Sources,
    layers: &[Layer],
) -> (Option<Compiled>, Vec<Diagnostic>) {
    compile_documents(sources, layers, true, &[], None)
}

/// A layer's documents in the order they apply (`SPEC.md` 7.1), without
/// its `schema.crod` unless `schema` says.
fn documents_of<'l>(
    sources: &Sources,
    layer: &'l Layer,
    schema: bool,
) -> Vec<&'l (String, FileId)> {
    order::order_layer(sources, layer, schema).0
}

/// How much text is worth a thread.
const PARSE_BYTES_PER_THREAD: usize = 1 << 20;

/// The most threads that parsing uses.
const MAX_PARSE_THREADS: usize = 8;

/// The stack of a thread that parses, as much as a main thread usually has:
/// the parser recurses, up to `crol_syntax::MAX_NESTING`.
const PARSE_STACK: usize = 8 << 20;

/// Parses documents, which do not depend on each other, on several threads
/// if there is enough text. The results are in the order of `texts`.
#[cfg(test)]
fn parse_all(texts: &[&str]) -> Vec<crol_syntax::Parse> {
    on_threads(texts, |_, text| crol_syntax::parse(text))
}

/// `work` for each of `texts`, which do not depend on each other, on several
/// threads if there is enough text. The results are in the order of `texts`.
fn on_threads<R: Send>(texts: &[&str], work: impl Fn(usize, &str) -> R + Sync) -> Vec<R> {
    let bytes: usize = texts.iter().map(|text| text.len()).sum();
    let threads = std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(MAX_PARSE_THREADS)
        .min(bytes / PARSE_BYTES_PER_THREAD);
    if threads < 2 {
        return texts
            .iter()
            .enumerate()
            .map(|(index, text)| work(index, text))
            .collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let each = || {
        let mut parsed = Vec::new();
        loop {
            let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let Some(text) = texts.get(index) else {
                return parsed;
            };
            parsed.push((index, work(index, text)));
        }
    };
    let mut parsed = std::thread::scope(|scope| {
        // A thread that cannot start leaves its share to the others, and
        // this one always works.
        let workers: Vec<_> = (1..threads)
            .filter_map(|_| {
                std::thread::Builder::new()
                    .stack_size(PARSE_STACK)
                    .spawn_scoped(scope, each)
                    .ok()
            })
            .collect();
        let mut parsed = each();
        for worker in workers {
            match worker.join() {
                Ok(more) => parsed.extend(more),
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
        parsed
    });
    parsed.sort_unstable_by_key(|(index, _)| *index);
    parsed.into_iter().map(|(_, parse)| parse).collect()
}

fn compile_documents(
    sources: &Sources,
    layers: &[Layer],
    schema: bool,
    lowered: &[Option<&[u8]>],
    before: Option<&Compiled>,
) -> (Option<Compiled>, Vec<Diagnostic>) {
    let mut diagnostics = Vec::new();
    // The documents of the layers from text: file, layer and position.
    let mut texts = Vec::new();
    let mut ir = ir::Ir::default();
    let mut parts = Vec::new();
    // The layers from bytes, and whether `before` decoded them already.
    let from: Vec<(usize, Vec<FileId>, usize, usize)> = layers
        .iter()
        .enumerate()
        .filter_map(|(index, layer)| {
            let bytes = lowered.get(index).copied().flatten()?;
            let files = documents_of(sources, layer, true);
            let files = files.iter().map(|(_, file)| *file).collect();
            Some((index, files, bytes.as_ptr() as usize, bytes.len()))
        })
        .collect();
    let reused = before
        .and_then(|before| Some((before, before.decoded.as_ref()?)))
        .filter(|(_, decoded)| !from.is_empty() && decoded.from == from);
    if let Some((before, decoded)) = reused {
        ir = before.lowered.ir.frozen_part();
        parts = decoded.parts.clone();
    }
    let mut last_file = None;
    let mut orders = Vec::with_capacity(layers.len());
    let mut in_order = Vec::with_capacity(layers.len());
    for (layer_index, layer) in layers.iter().enumerate() {
        let (documents, order, mut errors) = order::order_layer(sources, layer, schema);
        diagnostics.append(&mut errors);
        orders.push(order);
        in_order.push(documents.iter().map(|(_, file)| *file).collect());
        last_file = documents.last().map(|(_, file)| *file).or(last_file);
        if let Some(Some(bytes)) = lowered.get(layer_index) {
            if reused.is_some() {
                continue;
            }
            let all = documents_of(sources, layer, true);
            let files: Vec<FileId> = all.iter().map(|(_, file)| *file).collect();
            match binary::read_lowered(bytes, &files, sources, layer_index, &mut ir) {
                Ok(part) => parts.push(part),
                Err(message) => {
                    let span = files.first().map_or(Span::detached(), |file| {
                        Span::new(*file, Default::default())
                    });
                    diagnostics.push(Diagnostic::error(span, message));
                    return (None, diagnostics);
                }
            }
            continue;
        }
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
            texts.push((*file, layer_index, index));
        }
    }
    // What was decoded is fixed from here on, for a compilation after this
    // one to start from.
    let decoded = match reused {
        Some((_, decoded)) => Some(Arc::clone(decoded)),
        None if !from.is_empty() => {
            ir.freeze();
            Some(Arc::new(Decoded {
                from,
                parts: parts.clone(),
            }))
        }
        None => None,
    };
    // A document lowered before is lowered the same again; the others are
    // parsed and lowered, on threads if there are many.
    let kept = |file: &FileId| before.and_then(|before| before.texts.get(file));
    let fresh: Vec<FileId> = texts
        .iter()
        .map(|(file, _, _)| *file)
        .filter(|file| kept(file).is_none())
        .collect();
    let fresh_texts: Vec<&str> = fresh
        .iter()
        .map(|file| sources.file(*file).text())
        .collect();
    let fresh_lowered = on_threads(&fresh_texts, |index, text| {
        let file = fresh[index];
        let parse = crol_syntax::parse(text);
        let errors: Vec<Diagnostic> = parse
            .errors()
            .iter()
            .map(|error| Diagnostic::error(Span::new(file, error.range()), error.message()))
            .collect();
        (errors, lower::lower_document(file, &parse.document()))
    });
    let mut lowered_texts = HashMap::with_capacity(texts.len());
    for (file, (mut errors, lowered)) in fresh.iter().zip(fresh_lowered) {
        diagnostics.append(&mut errors);
        lowered_texts.insert(*file, Arc::new(lowered));
    }
    for (file, _, _) in &texts {
        if let Some(kept) = kept(file) {
            lowered_texts.insert(*file, Arc::clone(kept));
        }
    }
    let placed: Vec<lower::Placed<'_>> = texts
        .iter()
        .map(|(file, layer, index)| lower::Placed {
            lowered: &lowered_texts[file],
            layer: *layer,
            index: *index,
        })
        .collect();
    let (mut lowered, mut errors) = lower::link(&placed, ir, parts);
    drop(placed);
    if let Some(file) = last_file {
        lowered.root_span = Span::new(file, Default::default());
    }
    diagnostics.append(&mut errors);
    if diagnostics.iter().any(Diagnostic::is_error) {
        return (None, diagnostics);
    }
    lowered.ir.shrink();
    lowered.ops.shrink_to_fit();
    let layer_names = layers.iter().map(|l| l.name.clone()).collect();
    (
        Some(Compiled {
            lowered,
            layer_names,
            orders,
            documents: in_order,
            decoded,
            texts: lowered_texts,
        }),
        diagnostics,
    )
}

/// Evaluates compiled layers. With [`DataOptions::contexts`], this is how
/// a context is evaluated later, without reading the layers again.
pub fn evaluate_compiled(compiled: &Compiled, options: &DataOptions) -> Evaluation {
    evaluate_lowered(compiled, options, None)
}

/// What changes in evaluated data when its layers change, worked out from
/// the evaluation before: see [`evaluate_changed`].
pub struct Update {
    again: eval::Again,
    /// Where what is wrong with the root as a whole is reported.
    root_span: Span,
}

impl Update {
    /// The root keys evaluated again; no other key's value changed. Some of
    /// them may no longer exist, or not have existed before.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.again.affected.iter().map(|key| &**key)
    }

    /// Applies the update to the data it was worked out from: the base root
    /// and the contexts after it, as [`evaluate_changed`] was given them.
    pub fn apply(self, base: &mut Value, contexts: &mut [ContextData]) {
        let eval::Again {
            root,
            mut values,
            defaults,
            defaulted,
            contexts: fresh,
            affected,
        } = self.again;
        base.span = self.root_span;
        let is_affected = |key: &Key| match key {
            Key::Str(name) => affected.contains(name),
            Key::Int(_) | Key::Id(_) => false,
        };
        if let ValueKind::Keyed(keyed) = &mut base.kind {
            let mut old = std::mem::take(&mut keyed.entries);
            keyed.entries = root
                .into_iter()
                .filter_map(|key| {
                    let value = values.remove(&key).or_else(|| old.swap_remove(&key))?;
                    Some((key, value))
                })
                .collect();
            keyed.defaults = defaults;
            keyed.defaulted.clone_from(&defaulted);
        }
        for (data, fresh) in contexts.iter_mut().zip(fresh) {
            match fresh {
                Some(fresh) => *data = fresh,
                None => {
                    // The keys evaluated again do not differ here from the base.
                    data.changes.retain(|key, _| !is_affected(key));
                    data.defaulted.retain(|key| !is_affected(key));
                    data.defaulted
                        .extend(defaulted.iter().filter(|key| is_affected(key)).cloned());
                }
            }
        }
    }
}

/// Works out what evaluating `after` gives from the evaluation of `before`,
/// whose base root is `base` and whose contexts after it are `contexts`,
/// with `variants`, in the same contexts: only the keys whose operations
/// changed, and those that read them, are evaluated again, and only in the
/// contexts in which they can differ from the base. `options` must be those
/// of the evaluation before, with [`DataOptions::contexts`] naming those
/// contexts.
///
/// `None` where that cannot be done and everything is to be evaluated again
/// (`SPEC.md` 7.9): a template, a `..` at a root or a variant declaration
/// changed, documents changed their order, a reference reads another
/// context, the schema has constraints or sealed fields, or there is
/// anything to report.
pub fn evaluate_changed(
    before: &Compiled,
    after: &Compiled,
    options: &DataOptions,
    variants: &Variants,
    contexts: &[ContextData],
) -> Option<Update> {
    if let Some(defaults) = &options.defaults
        && (defaults.0.has_constraints() || defaults.0.has_sealed())
    {
        return None;
    }
    if options.variant_keys != VariantKeys::Any || before.layer_names != after.layer_names {
        return None;
    }
    // The documents in both are in the same order, and those in one of them
    // only are the ones that changed.
    let mut changed = std::collections::HashSet::new();
    for (layer, (old, new)) in before.documents.iter().zip(&after.documents).enumerate() {
        let in_old: std::collections::HashSet<FileId> = old.iter().copied().collect();
        let in_new: std::collections::HashSet<FileId> = new.iter().copied().collect();
        let kept_old = old.iter().filter(|file| in_new.contains(file));
        if !kept_old.eq(new.iter().filter(|file| in_old.contains(file)))
            || !before.orders[layer].same_edges(&after.orders[layer])
        {
            return None;
        }
        changed.extend(in_old.symmetric_difference(&in_new).copied());
    }
    let in_changed = |span: Span| changed.contains(&span.file);
    for compiled in [before, after] {
        let lowered = &compiled.lowered;
        let ir = &lowered.ir;
        let templates = lowered.templates.iter();
        if templates
            .map(|&id| ir.exprs[ir.value_binds[id].expr].span)
            .chain(lowered.root_defaults.iter().map(|d| d.span))
            .chain(lowered.declared.iter().map(|d| d.span))
            .any(in_changed)
        {
            return None;
        }
    }
    let dirty: std::collections::HashSet<Arc<str>> = before
        .lowered
        .ops
        .iter()
        .chain(&after.lowered.ops)
        .filter(|op| in_changed(op.span))
        .map(|op| op.key.clone())
        .collect();
    let lowered = &after.lowered;
    let input = eval::DataInput {
        ops: &lowered.ops,
        templates: &lowered.templates,
        root_defaults: &lowered.root_defaults,
        declared: &lowered.declared,
        root_span: lowered.root_span,
        layer_names: &after.layer_names,
        orders: &after.orders,
        variants: &options.variants,
        contexts: options.contexts.as_deref(),
        each_variant: options.each_variant,
        max_contexts: options.max_contexts,
        variant_keys: options.variant_keys,
        evaluate_every_key: options.evaluate_every_key,
        defaults: options.defaults.as_ref().map(|d| &*d.0),
        init_defaults: options.init_defaults,
        merge_root: options.merge_root,
        known_base: None,
        only: None,
    };
    let again = eval::evaluate_again(&lowered.ir, &input, &dirty, variants, contexts)?;
    Some(Update {
        again,
        root_span: lowered.root_span,
    })
}

/// Evaluates [`DataOptions::contexts`] given the base root as it was evaluated
/// before, which is not evaluated again: only what the contexts need of it
/// is. The evaluation's `value` is `None`, and its `contexts` are as usual.
pub fn evaluate_contexts(compiled: &Compiled, options: &DataOptions, base: &Value) -> Evaluation {
    evaluate_lowered(compiled, options, Some(base))
}

fn evaluate_lowered(
    compiled: &Compiled,
    options: &DataOptions,
    known_base: Option<&Value>,
) -> Evaluation {
    let lowered = &compiled.lowered;
    let mut diagnostics = Vec::new();
    let input = eval::DataInput {
        ops: &lowered.ops,
        templates: &lowered.templates,
        root_defaults: &lowered.root_defaults,
        declared: &lowered.declared,
        root_span: lowered.root_span,
        layer_names: &compiled.layer_names,
        orders: &compiled.orders,
        variants: &options.variants,
        contexts: options.contexts.as_deref(),
        each_variant: options.each_variant,
        max_contexts: options.max_contexts,
        variant_keys: options.variant_keys,
        evaluate_every_key: options.evaluate_every_key,
        defaults: options.defaults.as_ref().map(|d| &*d.0),
        init_defaults: options.init_defaults,
        merge_root: options.merge_root,
        known_base,
        only: None,
    };
    let threads = match options.threads {
        Some(threads) => threads.max(1),
        None if lowered.ir.exprs.len() >= THREADED_EXPRS => std::thread::available_parallelism()
            .map_or(1, std::num::NonZero::get)
            .min(MAX_EVAL_THREADS),
        None => 1,
    };
    let (result, mut errors) = if threads > 1 {
        eval::evaluate_data_threaded(&lowered.ir, &input, threads)
    } else {
        eval::evaluate_data(&lowered.ir, &input)
    };
    diagnostics.append(&mut errors);
    let Some(result) = result else {
        return Evaluation {
            diagnostics,
            ..Evaluation::default()
        };
    };
    let evaluated = !diagnostics.iter().any(Diagnostic::is_error);
    let base = result.base.as_ref().or(known_base);
    if let (Some(defaults), Some(base)) = (&options.defaults, base)
        && defaults.0.has_constraints()
    {
        check_constraints(
            &*defaults.0,
            base,
            &result.contexts,
            &result.variants,
            &mut diagnostics,
        );
    }
    if evaluated
        && let (Some(defaults), Some(base)) = (input.defaults, base)
        && defaults.has_sealed()
    {
        check_sealed(lowered, &input, defaults, base, &result, &mut diagnostics);
    }
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
    // The whole text is the path: not `+a`, or `a: 1, b`, which hold one.
    let range = crol_syntax::ast::AstNode::syntax(&target).text_range();
    if usize::from(range.start()) != 0 || usize::from(range.end()) != text.len() {
        return Err(invalid());
    }
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

/// Whether `expression`, written as the value of an entry, uses the value
/// below the entry (`SPEC.md` 7.2): `& + 1` does, `&root.x` does not.
pub fn uses_value_below(expression: &str) -> bool {
    let mut sources = Sources::new();
    let file = sources.add("expression".to_owned(), format!("key: {expression}\n"));
    let parse = crol_syntax::parse(sources.file(file).text());
    let document = parse.document();
    let (lowered, _) = lower::lower_data(&[lower::DataDocument {
        file,
        document: &document,
        layer: 0,
        index: 0,
    }]);
    lowered.ops.iter().any(|op| op.uses_below)
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
        let documents = documents_of(sources, layer, false);
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
    explain_lowered(
        &lowered,
        &layer_names,
        &evaluation.variants,
        options,
        requests,
        context,
    )
}

/// What [`explain_compiled`] works out once of layers compiled and evaluated
/// before, without errors, for as many lookups as there are.
#[derive(Clone)]
pub struct ExplainPlan(Arc<eval::PlanData>);

impl std::fmt::Debug for ExplainPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExplainPlan").finish_non_exhaustive()
    }
}

/// Works out an [`ExplainPlan`]. `variants` are the evaluation's.
pub fn explain_plan(
    compiled: &Compiled,
    variants: &Variants,
    options: &DataOptions,
) -> Result<ExplainPlan, Vec<Diagnostic>> {
    let input = explain_input(&compiled.lowered, &compiled.layer_names, variants, options);
    eval::plan_data(&compiled.lowered.ir, &input).map(ExplainPlan)
}

/// [`explain`] of layers compiled before: only the key is looked at, so an
/// editor can ask as it goes.
pub fn explain_compiled(
    compiled: &Compiled,
    plan: &ExplainPlan,
    options: &DataOptions,
    path: &str,
    context: &[String],
) -> Result<Vec<ExplainStep>, Vec<Diagnostic>> {
    let (key, steps) =
        parse_path(path).map_err(|message| vec![Diagnostic::error(Span::detached(), message)])?;
    let lowered = &compiled.lowered;
    let variants = &plan.0.variants;
    let input = explain_input(lowered, &compiled.layer_names, variants, options);
    let requests = [(key.as_str(), steps.as_slice())];
    let mut found = eval::explain_planned(&lowered.ir, &input, &plan.0, &requests, context)?;
    Ok(found.pop().unwrap_or_default())
}

fn explain_lowered(
    lowered: &lower::LoweredData,
    layer_names: &[String],
    variants: &Variants,
    options: &DataOptions,
    requests: &[(&str, &[PathStep])],
    context: &[String],
) -> Result<Vec<Vec<ExplainStep>>, Vec<Diagnostic>> {
    let input = explain_input(lowered, layer_names, variants, options);
    eval::explain(&lowered.ir, &input, requests, context)
}

fn explain_input<'a>(
    lowered: &'a lower::LoweredData,
    layer_names: &'a [String],
    variants: &'a Variants,
    options: &'a DataOptions,
) -> eval::DataInput<'a> {
    eval::DataInput {
        ops: &lowered.ops,
        templates: &lowered.templates,
        root_defaults: &lowered.root_defaults,
        declared: &lowered.declared,
        root_span: lowered.root_span,
        layer_names,
        orders: &[],
        variants,
        contexts: options.contexts.as_deref(),
        each_variant: options.each_variant,
        max_contexts: options.max_contexts,
        variant_keys: options.variant_keys,
        evaluate_every_key: false,
        defaults: options.defaults.as_ref().map(|d| &*d.0),
        init_defaults: options.init_defaults,
        merge_root: options.merge_root,
        known_base: None,
        only: None,
    }
}

/// Whether an operation's path leads to a field path, or past it. An
/// operation with no path may set anything.
fn touches(op: &ir::RootOp, path: &[String]) -> bool {
    op.path
        .iter()
        .zip(path)
        .all(|(selector, field)| eval::selector_field(selector).is_none_or(|name| name == *field))
}

/// Reports the sealed fields that differ from what they are after the layer
/// that defines their root key (`SPEC.md` 10.13), however they changed. The
/// data after a layer is the data of the layers up to it, evaluated alone: a
/// layer that does not evaluate alone has nothing to compare with.
fn check_sealed(
    lowered: &lower::LoweredData,
    input: &eval::DataInput<'_>,
    defaults: &dyn DefaultsProvider,
    base: &Value,
    result: &eval::DataResult,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let ops = input.ops;
    let mut first_layer: IndexMap<&str, usize> = IndexMap::new();
    for op in ops {
        first_layer.entry(&*op.key).or_insert(op.layer);
    }
    let contexts: Vec<Vec<String>> = result
        .contexts
        .iter()
        .map(|data| {
            let names = result.variants.chosen_names(&data.context);
            names.into_iter().map(str::to_owned).collect()
        })
        .collect();
    let mut reported = std::collections::HashSet::new();
    for layer in 0..input.layer_names.len() {
        // Operations are in layer order.
        let end = ops.partition_point(|op| op.layer <= layer);
        if end == ops.len() {
            break;
        }
        if !first_layer.values().any(|&first| first == layer) {
            continue;
        }
        let below = eval::DataInput {
            ops: &ops[..end],
            variants: &result.variants,
            contexts: Some(&contexts),
            orders: &[],
            known_base: None,
            ..*input
        };
        let (below, _) = eval::evaluate_data(&lowered.ir, &below);
        let Some((below, Some(below_base))) = below.map(|b| {
            let value = b.base.clone();
            (b, value)
        }) else {
            continue;
        };
        let keyed = |value: &Value, key: &Key| match &value.kind {
            ValueKind::Keyed(keyed) => keyed.entries.get(key).cloned(),
            _ => None,
        };
        for (key, _) in first_layer.iter().filter(|(_, first)| **first == layer) {
            let root_key = Key::Str(Arc::from(*key));
            let mut pairs = vec![(keyed(&below_base, &root_key), keyed(base, &root_key))];
            for data in &result.contexts {
                let then = below.contexts.iter().find(|b| b.context == data.context);
                pairs.push((
                    then.and_then(|then| then.get(&below_base, &root_key).cloned()),
                    data.get(base, &root_key).cloned(),
                ));
            }
            for (then, now) in pairs {
                let (Some(then), Some(now)) = (then, now) else {
                    continue;
                };
                for path in defaults.changed_sealed(key, &then, &now) {
                    let above = || ops[end..].iter().filter(|op| &*op.key == *key);
                    let op = above()
                        .find(|op| touches(op, &path))
                        .or_else(|| above().next())
                        .or_else(|| ops.iter().find(|op| &*op.key == *key));
                    let Some(op) = op else { continue };
                    let name = format!("{key}.{}", path.join("."));
                    let layer_name = &input.layer_names[layer];
                    let message = format!(
                        "`{name}` is sealed: only the layer that defines `{key}` (`{layer_name}`) can set it"
                    );
                    if reported.insert((op.span, message.clone())) {
                        diagnostics.push(Diagnostic::error(op.span, message));
                    }
                }
            }
        }
    }
}

/// Checks the constraints of every root key, in the base, and in each context
/// where a key differs from it (`SPEC.md` 10.12).
fn check_constraints(
    defaults: &dyn DefaultsProvider,
    base: &Value,
    contexts: &[ContextData],
    variants: &Variants,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let ValueKind::Keyed(keyed) = &base.kind else {
        return;
    };
    let name_of = |key: &Key| match key {
        Key::Str(name) => name.to_string(),
        Key::Int(i) => i.to_string(),
        Key::Id(id) => id.to_string(),
    };
    let in_base = |name: &str| keyed.entries.get(&value::KeyRef::Str(name)).cloned();
    for (key, value) in &keyed.entries {
        for (span, message) in defaults.broken_constraints(&name_of(key), value, &in_base) {
            diagnostics.push(Diagnostic::error(span, message));
        }
    }
    for data in contexts {
        let in_context = |name: &str| data.get(base, &Key::Str(Arc::from(name))).cloned();
        for (key, change) in &data.changes {
            let Some(value) = change else { continue };
            for (span, message) in defaults.broken_constraints(&name_of(key), value, &in_context) {
                let context = variants.display(&data.context);
                diagnostics.push(Diagnostic::error(span, format!("in {context}: {message}")));
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing_on_threads_gives_what_parsing_in_order_gives() {
        // Enough text for several threads, some of it with syntax errors.
        let texts: Vec<String> = (0..600)
            .map(|i| {
                let broken = if i % 7 == 0 { "+: ,\n" } else { "" };
                let entry = format!("k{i}: {{ a: {i}, b: \"text\" }},\n");
                format!("{broken}{}", entry.repeat(200))
            })
            .collect();
        let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
        let bytes: usize = texts.iter().map(|text| text.len()).sum();
        assert!(bytes >= 2 * PARSE_BYTES_PER_THREAD);

        let parsed = parse_all(&texts);
        assert_eq!(parsed.len(), texts.len());
        for (text, parse) in texts.iter().zip(&parsed) {
            let alone = crol_syntax::parse(text);
            assert_eq!(parse.syntax().to_string(), *text);
            assert_eq!(parse.errors(), alone.errors());
        }
        assert!(parsed.iter().any(|parse| !parse.errors().is_empty()));
    }

    #[test]
    fn lowering_and_linking_on_threads_keep_the_documents_apart() {
        // Enough for several threads, with templates used across documents.
        const DOCUMENTS: usize = 300;
        let text = |i: usize, extra: usize| {
            let next = (i + 1) % DOCUMENTS;
            let mut text = format!("t{i} := {},\n", i + extra);
            for j in 0..50 {
                text += &format!(
                    "+k{i}_{j}: {{ a: {j}, b: &t{next} + {j}, c: [&root.k{i}_0.a, {{ d: &t{i} }}], s: \"{}\" }},\n",
                    "text ".repeat(20)
                );
            }
            text
        };
        let mut sources = Sources::new();
        let mut layer = Layer {
            name: "base".to_owned(),
            documents: (0..DOCUMENTS)
                .map(|i| {
                    (
                        format!("d{i:03}.crod"),
                        sources.add(format!("d{i:03}.crod"), text(i, 0)),
                    )
                })
                .collect(),
        };
        let bytes: usize = (0..DOCUMENTS).map(|i| text(i, 0).len()).sum();
        assert!(bytes >= 2 * PARSE_BYTES_PER_THREAD);
        let (compiled, diagnostics) = compile(&sources, std::slice::from_ref(&layer));
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let compiled = compiled.unwrap();
        assert!(compiled.lowered.ir.exprs.len() >= 2 * lower::LINK_EXPRS_PER_THREAD);
        let options = DataOptions::default();
        let evaluation = evaluate_compiled(&compiled, &options);
        let root = evaluation.value.unwrap();
        let before = fingerprint::fingerprint(&root, &evaluation.contexts, &evaluation.variants);
        let field = |value: &Value, name: &str| match &value.kind {
            ValueKind::Keyed(keyed) => keyed.entries[&Key::Str(name.into())].clone(),
            _ => panic!("{value:?} is not keyed"),
        };
        let int = |value: Value| match value.kind {
            ValueKind::Int(i) => i,
            kind => panic!("{kind:?} is not an integer"),
        };
        for i in [0, 1, DOCUMENTS / 2, DOCUMENTS - 1] {
            for j in [0, 49] {
                let key = field(&root, &format!("k{i}_{j}"));
                let next = ((i + 1) % DOCUMENTS) as i128;
                assert_eq!(int(field(&key, "b")), next + j as i128);
                let ValueKind::List(items) = field(&key, "c").kind else {
                    panic!("not a list");
                };
                assert_eq!(int(field(&items[1], "d")), i as i128);
            }
        }

        // Compiling again after one document changes keeps the others.
        layer.documents[DOCUMENTS / 2].1 = sources.add("changed", text(DOCUMENTS / 2, 1000));
        let (again, _) =
            compile_binary_after(&sources, std::slice::from_ref(&layer), &[], &compiled);
        let (fresh, _) = compile(&sources, std::slice::from_ref(&layer));
        let fingerprint = |compiled: &Compiled| {
            let evaluation = evaluate_compiled(compiled, &options);
            fingerprint::fingerprint(
                &evaluation.value.unwrap(),
                &evaluation.contexts,
                &evaluation.variants,
            )
        };
        let again = again.unwrap();
        assert_eq!(fingerprint(&again), fingerprint(&fresh.unwrap()));
        assert_ne!(fingerprint(&again), before);
        let kept = layer
            .documents
            .iter()
            .filter(|(_, file)| {
                let before = compiled.texts.get(file);
                before.is_some_and(|before| Arc::ptr_eq(before, &again.texts[file]))
            })
            .count();
        assert_eq!(kept, DOCUMENTS - 1);
    }
}
