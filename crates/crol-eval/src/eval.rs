//! The evaluator (`SPEC.md` 5, 6 and 7.5).
//!
//! Evaluation is lazy and memoized. Every value lives in a [`Cell`], which
//! computes its *shape* on demand: its kind, and for keyed values, lists and
//! tuples, the cells of their entries and elements, which are lazy in turn.
//! Forcing a cell that is already being computed is a cycle.
//!
//! Each entry of a keyed value keeps the *recipe* that produced it, so that a
//! spread can evaluate copied entries again at their new location (`SPEC.md`
//! 6.1, "rebinding through spreads").

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::rc::{Rc, Weak};

use indexmap::IndexMap;

use crate::cond::Resolved;
use crate::diagnostic::Diagnostic;
use crate::ir::*;
use crate::source::Span;
use crate::value::{Key, KeyRef, Keyed, Value, ValueKind, format_float};
use crate::variants::{Context, Variants};

/// The most cells being computed at once, so that deeply nested references
/// fail with an error instead of overflowing the stack. A debug build uses
/// about 8 KiB of stack per level, so this fits a 2 MiB thread.
const MAX_DEPTH: usize = 128;

/// The most cells one evaluation computes. A value that spreads a value
/// containing it twice doubles at every level; this bounds the work so that
/// evaluation always ends (`SPEC.md` 14.2).
const MAX_STEPS: u64 = 2_000_000;

/// The deepest an evaluated value may nest. Documents cannot nest deeper
/// than the parser's limit, so only a value that contains a copy of itself
/// gets here.
const MAX_VALUE_DEPTH: usize = crol_syntax::MAX_NESTING;

type Lazy = Rc<Cell>;

/// A lazily computed value.
struct Cell {
    state: RefCell<State>,
    /// The expression the cell evaluates, if any: a value whose contents
    /// hold a cell of the same expression contains a copy of itself.
    expr: Option<ExprId>,
    /// Where the value ends up, for "while evaluating ..." in diagnostics.
    location: Option<Location>,
}

enum State {
    /// Boxed, so that computed cells, the most common, stay small.
    Pending(Box<Job>),
    Running,
    Done(Result<Rc<Shaped>, Poisoned>),
}

/// A value that failed; the failure has been reported.
#[derive(Debug, Clone, Copy)]
struct Poisoned;

/// Why evaluating an expression failed.
enum Fail {
    /// A value is missing (`SPEC.md` 6.5); `|` catches this.
    Missing(Span, String),
    /// An error, already reported, or a dependency on one.
    Poison,
}

impl From<Poisoned> for Fail {
    fn from(_: Poisoned) -> Self {
        Fail::Poison
    }
}

type Eval<T> = Result<T, Fail>;

enum Job {
    Expr {
        expr: ExprId,
        env: Env,
    },
    /// A path entry (`web.limits.cpu: 2.0`) applied to a value.
    Path {
        base: Lazy,
        recipe: Rc<PathRecipe>,
        chain: Chain,
        here: Option<Location>,
        context: usize,
    },
}

impl Cell {
    fn new(job: Job, location: Option<Location>) -> Lazy {
        let expr = match &job {
            Job::Expr { expr, .. } => Some(*expr),
            Job::Path { .. } => None,
        };
        Rc::new(Cell {
            state: RefCell::new(State::Pending(Box::new(job))),
            expr,
            location,
        })
    }

    fn done(shaped: Rc<Shaped>) -> Lazy {
        Rc::new(Cell {
            state: RefCell::new(State::Done(Ok(shaped))),
            expr: None,
            location: None,
        })
    }
}

/// A value whose top level is computed.
struct Shaped {
    shape: Shape,
    span: Span,
}

enum Shape {
    Unit,
    Bool(bool),
    Int(i128),
    Float(f64),
    Char(char),
    Str(Rc<str>),
    Id(Rc<str>),
    List(Vec<Lazy>),
    Tuple(Vec<Lazy>),
    UnitName(Rc<str>),
    Named(Rc<str>, Vec<Lazy>),
    Keyed(KeyedShape),
}

impl Shape {
    fn kind_name(&self) -> &'static str {
        match self {
            Shape::Unit => "the unit value",
            Shape::Bool(_) => "a boolean",
            Shape::Int(_) => "an integer",
            Shape::Float(_) => "a float",
            Shape::Char(_) => "a char",
            Shape::Str(_) => "a string",
            Shape::Id(_) => "an id",
            Shape::List(_) => "a list",
            Shape::Tuple(_) => "a tuple",
            Shape::UnitName(_) => "a unit name",
            Shape::Named(..) => "a named tuple",
            Shape::Keyed(_) => "a keyed value",
        }
    }
}

#[derive(Clone, Default)]
struct KeyedShape {
    name: Option<Rc<str>>,
    slots: IndexMap<Key, Slot>,
    defaults: bool,
    defaulted: BTreeSet<Key>,
}

#[derive(Clone)]
struct Slot {
    value: Lazy,
    recipe: Recipe,
}

/// How an entry's value was produced, so it can be produced again elsewhere.
#[derive(Clone)]
enum Recipe {
    /// An entry's expression.
    Entry(Rc<EntryRecipe>),
    /// A path entry applied to an earlier value.
    Path(Rc<PathRecipe>),
    /// A value that stays what it is wherever it is copied.
    Fixed,
}

struct EntryRecipe {
    expr: ExprId,
    /// `&`, which does not move when the entry is copied.
    below: Below,
    /// Value binds, which do not move either (`SPEC.md` 6.1.1).
    vbinds: VBinds,
}

struct PathRecipe {
    base: Recipe,
    base_value: Lazy,
    path: Vec<Selector>,
    op: Option<SetOp>,
    value: Option<Rc<EntryRecipe>>,
    span: Span,
}

/// What an expression is evaluated in.
#[derive(Clone)]
struct Env {
    /// The locations of the blocks around the expression, innermost first.
    chain: Chain,
    /// Where the expression's value ends up, if anywhere.
    here: Option<Location>,
    /// The value below the innermost entry: `&`.
    below: Below,
    /// The value below that a whole-value block here sees through to.
    outer_below: Below,
    vbinds: VBinds,
    /// The context the expression is evaluated in, as an index into the
    /// evaluator's contexts.
    context: usize,
}

type Chain = Option<Rc<ChainLink>>;

struct ChainLink {
    location: Option<Location>,
    /// The block of a value bind: `&parent` cannot go above it.
    bind_root: bool,
    next: Chain,
}

#[derive(Clone)]
enum Below {
    Missing,
    Value(Lazy),
    Select(Rc<Below>, Selector),
}

type VBinds = Option<Rc<VBindLink>>;

struct VBindLink {
    id: BindId,
    cell: Lazy,
    next: VBinds,
}

/// What a location starts from, so that it can be found again in another
/// context (`SPEC.md` 8.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Anchor {
    Root,
    Template(BindId),
    /// A value bind inside a block, which has no other contexts.
    Local,
}

/// A path from the document's value, or from a value bind's.
#[derive(Clone)]
struct Location {
    base: Weak<Cell>,
    anchor: Anchor,
    /// The bind's name, or empty for the root.
    name: Rc<str>,
    /// The steps from the base, as a shared list from the last step back,
    /// so that a child location costs one allocation.
    path: Option<Rc<PathLink>>,
}

struct PathLink {
    step: Step,
    parent: Option<Rc<PathLink>>,
}

#[derive(Clone)]
enum Step {
    Key(Key),
    Index(usize),
    /// `[i]`: an integer key, or a list index, depending on the value.
    Accessor(i128),
}

/// The step a selector takes; `None` for a context selector.
fn selector_step(selector: &Selector) -> Option<Step> {
    match &selector.kind {
        SelectorKind::Field(name) => Some(Step::Key(Key::Str(name.to_string()))),
        SelectorKind::Element(i) => Some(Step::Index(*i)),
        SelectorKind::Accessor(i) => Some(Step::Accessor(*i)),
        SelectorKind::Context(_) => None,
    }
}

impl Location {
    fn child(&self, step: Step) -> Location {
        Location {
            base: self.base.clone(),
            anchor: self.anchor,
            name: self.name.clone(),
            path: Some(Rc::new(PathLink {
                step,
                parent: self.path.clone(),
            })),
        }
    }

    /// The steps from the base, first to last, borrowed.
    fn step_refs(&self) -> Vec<&Step> {
        let mut steps = Vec::new();
        let mut link = self.path.as_deref();
        while let Some(l) = link {
            steps.push(&l.step);
            link = l.parent.as_deref();
        }
        steps.reverse();
        steps
    }

    /// The steps from the base, first to last.
    fn steps(&self) -> Vec<Step> {
        let mut steps = Vec::new();
        let mut link = self.path.as_ref();
        while let Some(l) = link {
            steps.push(l.step.clone());
            link = l.parent.as_ref();
        }
        steps.reverse();
        steps
    }

    fn describe(&self) -> String {
        let mut out = self.name.to_string();
        for step in &self.steps() {
            match step {
                Step::Key(key @ Key::Str(s)) if key.is_identifier() => {
                    if !out.is_empty() {
                        out.push('.');
                    }
                    out.push_str(s);
                }
                Step::Key(Key::Str(s)) => out.push_str(&format!("[{}]", crate::value::quote(s))),
                Step::Key(Key::Int(i)) => {
                    let _ = write!(out, "[{i}]");
                }
                Step::Index(i) => {
                    let _ = write!(out, "[{i}]");
                }
                Step::Accessor(i) => {
                    let _ = write!(out, "[{i}]");
                }
            }
        }
        out
    }
}

fn child_location(location: &Option<Location>, step: Step) -> Option<Location> {
    location.as_ref().map(|l| l.child(step))
}

fn key_selector(key: &Key, span: Span) -> Selector {
    let kind = match key {
        Key::Str(s) => SelectorKind::Field(Rc::from(s.as_str())),
        Key::Int(i) => SelectorKind::Accessor(*i),
    };
    Selector { kind, span }
}

/// An entry's target as written: `web.limits.cpu`, with its operator.
fn describe_target(entry: &Entry) -> String {
    let mut out = match entry.op {
        None => "-".to_owned(),
        Some(SetOp::Declare) => "+".to_owned(),
        Some(SetOp::Ensure) => "?".to_owned(),
        Some(SetOp::Set) => String::new(),
    };
    out.push_str(&entry.key.to_string());
    for selector in &entry.path {
        out.push_str(&describe_selector(selector));
    }
    out
}

fn describe_selector(selector: &Selector) -> String {
    match &selector.kind {
        SelectorKind::Field(name) if crate::value::is_identifier(name) => format!(".{name}"),
        SelectorKind::Field(name) => format!("[{}]", crate::value::quote(name)),
        SelectorKind::Element(i) => format!(".{i}"),
        SelectorKind::Accessor(i) => format!("[{i}]"),
        SelectorKind::Context(_) => "<...>".to_owned(),
    }
}

pub(crate) struct Evaluator<'ir> {
    ir: &'ir Ir,
    diagnostics: Vec<Diagnostic>,
    /// The cells being computed, with the span of what forced each.
    stack: Vec<(Lazy, Span)>,
    /// Cells computed so far, against [`MAX_STEPS`].
    steps: u64,
    /// The contexts evaluated so far, each with its own root.
    contexts: Vec<ContextState>,
    /// For data mode: the operations, by key, and the variants.
    plan: Option<&'ir DataPlan<'ir>>,
    /// Whether contexts share the base's cells for keys that cannot differ
    /// from it; off when the base failed, so that every context reports its
    /// own errors.
    share: bool,
    /// For each base root key, in root order, the operation that created it.
    base_created: Vec<usize>,
    /// Where `&default` finds defaults.
    defaults: Option<&'ir dyn DefaultsProvider>,
}

/// What the entries of a block are evaluated in.
struct BlockScope {
    chain: Chain,
    location: Option<Location>,
    outer_below: Below,
    vbinds: VBinds,
    context: usize,
}

/// The root and templates of one context.
struct ContextState {
    context: Context,
    /// Kept alive here; locations refer to it weakly.
    root_cell: Lazy,
    root: Location,
    templates: VBinds,
}

fn root_location(cell: &Lazy) -> Location {
    Location {
        base: Rc::downgrade(cell),
        anchor: Anchor::Root,
        name: Rc::from(""),
        path: None,
    }
}

/// Where `&default` finds defaults (`SPEC.md` 6.1.3): usually a schema.
pub trait DefaultsProvider: std::fmt::Debug + Send + Sync {
    /// The default of the field at `path` from the type named `type_name`
    /// (`None`: the root type), or with an empty path the type's own
    /// default; `None` if no default is recorded.
    fn default_of(&self, type_name: Option<&str>, path: &[String]) -> Option<Value>;
}

/// A shape for a value that is already computed, such as a default.
fn shaped_from_value(value: &Value) -> Rc<Shaped> {
    let cells = |items: &[Value]| {
        items
            .iter()
            .map(|v| Cell::done(shaped_from_value(v)))
            .collect()
    };
    let shape = match &value.kind {
        ValueKind::Unit => Shape::Unit,
        ValueKind::Bool(b) => Shape::Bool(*b),
        ValueKind::Int(i) => Shape::Int(*i),
        ValueKind::Float(f) => Shape::Float(*f),
        ValueKind::Char(c) => Shape::Char(*c),
        ValueKind::String(s) => Shape::Str(Rc::from(s.as_str())),
        ValueKind::Id(s) => Shape::Id(Rc::from(s.as_str())),
        ValueKind::UnitName(n) => Shape::UnitName(Rc::from(n.as_str())),
        ValueKind::List(items) => Shape::List(cells(items)),
        ValueKind::Tuple(items) => Shape::Tuple(cells(items)),
        ValueKind::NamedTuple(n, items) => Shape::Named(Rc::from(n.as_str()), cells(items)),
        ValueKind::Keyed(keyed) => Shape::Keyed(KeyedShape {
            name: keyed.name.as_deref().map(Rc::from),
            slots: keyed
                .entries
                .iter()
                .map(|(k, v)| {
                    let cell = Cell::done(shaped_from_value(v));
                    (
                        k.clone(),
                        Slot {
                            value: cell,
                            recipe: Recipe::Fixed,
                        },
                    )
                })
                .collect(),
            defaults: keyed.defaults,
            defaulted: keyed.defaulted.clone(),
        }),
    };
    Rc::new(Shaped {
        shape,
        span: value.span,
    })
}

/// Evaluates a lowered value-mode document.
pub(crate) fn evaluate(ir: &Ir, root: ExprId) -> (Option<Value>, Vec<Diagnostic>) {
    let mut evaluator = Evaluator {
        ir,
        diagnostics: Vec::new(),
        stack: Vec::new(),
        steps: 0,
        contexts: Vec::new(),
        plan: None,
        share: false,
        base_created: Vec::new(),
        defaults: None,
    };
    let root_cell = Rc::new(Cell {
        state: RefCell::new(State::Running),
        expr: Some(root),
        location: None,
    });
    let location = root_location(&root_cell);
    *root_cell.state.borrow_mut() = State::Pending(Box::new(Job::Expr {
        expr: root,
        env: Env {
            chain: None,
            here: Some(location.clone()),
            below: Below::Missing,
            outer_below: Below::Missing,
            vbinds: None,
            context: 0,
        },
    }));
    evaluator.contexts.push(ContextState {
        context: Context::base(),
        root_cell: root_cell.clone(),
        root: location,
        templates: None,
    });
    let span = ir.exprs[root].span;
    let value = evaluator.full_value(&root_cell, span, &mut Vec::new()).ok();
    (value, evaluator.diagnostics)
}

/// Which keys a context may have, compared with the base (`SPEC.md` 8.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VariantKeys {
    /// Any: contexts may add and remove keys.
    #[default]
    Any,
    /// Every key in a context must exist in the base.
    NoNewKeys,
    /// Every context has exactly the base's keys.
    SameKeys,
}

/// What data evaluation needs besides the IR.
pub(crate) struct DataInput<'a> {
    pub(crate) ops: &'a [RootOp],
    pub(crate) templates: &'a [BindId],
    pub(crate) layer_names: &'a [String],
    pub(crate) warn_cross_file_patches: bool,
    pub(crate) variants: &'a Variants,
    /// The contexts to evaluate, by chosen variant names; `None` for every
    /// valid context.
    pub(crate) contexts: Option<&'a [Vec<String>]>,
    pub(crate) max_contexts: usize,
    pub(crate) variant_keys: VariantKeys,
    pub(crate) evaluate_every_key: bool,
    pub(crate) defaults: Option<&'a dyn DefaultsProvider>,
}

/// The evaluated data: the variants (declared, or found in the data), the
/// base root, and how every other evaluated context differs from it.
pub(crate) struct DataResult {
    pub(crate) variants: Variants,
    pub(crate) base: Option<Value>,
    pub(crate) contexts: Vec<ContextData>,
}

/// How one context's data differs from the base: the keys whose value is
/// not identical to the base's, those that exist only in the context, and
/// with `None` those the context removes (`SPEC.md` 7.6). Keys are in root
/// order, with context-only keys after.
#[derive(Debug, Clone)]
pub struct ContextData {
    /// The context.
    pub context: Context,
    /// The differences from the base.
    pub changes: IndexMap<Key, Option<Value>>,
}

impl ContextData {
    /// The context's whole root: the base with the changes applied.
    pub fn root(&self, base: &Value) -> Value {
        let ValueKind::Keyed(keyed) = &base.kind else {
            return base.clone();
        };
        let mut keyed = keyed.clone();
        for (key, change) in &self.changes {
            match change {
                Some(value) => {
                    keyed.entries.insert(key.clone(), value.clone());
                }
                None => {
                    keyed.entries.shift_remove(key);
                }
            }
        }
        Value::new(ValueKind::Keyed(keyed), base.span)
    }

    /// A root key's value in the context, given the base root.
    pub fn get<'v>(&'v self, base: &'v Value, key: &Key) -> Option<&'v Value> {
        match self.changes.get(key) {
            Some(change) => change.as_ref(),
            None => match &base.kind {
                ValueKind::Keyed(keyed) => keyed.entries.get(key),
                _ => None,
            },
        }
    }
}

/// The operations of every key, ready to be applied in any context.
pub(crate) struct DataPlan<'a> {
    ops: &'a [RootOp],
    templates: &'a [BindId],
    variants: Variants,
    /// Each operation's condition, resolved; `None` for a base operation.
    conditions: Vec<Option<Resolved>>,
    keys: IndexMap<Rc<str>, KeyPlan>,
    /// For each root key, the keys whose value it reads late.
    dependents: std::collections::HashMap<Rc<str>, Vec<Rc<str>>>,
}

/// What a key's operations read, found from their expressions.
#[derive(Debug, Default, Clone)]
struct Reads {
    /// Root keys read through `&root.key`, or `&self` and the like at the
    /// top level.
    keys: Vec<Rc<str>>,
    /// The whole root, or a root key that cannot be told statically.
    whole: bool,
    /// A reference into another context.
    switch: bool,
}

/// Collects what an expression reads (`depth`: the number of blocks around
/// it below the root), for deciding which keys can differ between contexts.
fn collect_reads(
    ir: &Ir,
    expr: ExprId,
    depth: usize,
    templates: &mut TemplateReads,
    out: &mut Reads,
) {
    // Whether a reference `up` blocks out reaches the root.
    let at_root = |up: usize| up == depth;
    match &ir.exprs[expr].kind {
        ExprKind::Ref(kind) => match kind {
            RefKind::SelfValue if at_root(0) => out.whole = true,
            RefKind::Parent(n) if at_root(*n) => out.whole = true,
            RefKind::Root => out.whole = true,
            RefKind::Bind { up, target } if at_root(*up) => match target.first().map(|s| &s.kind) {
                Some(SelectorKind::Field(name)) => out.keys.push(name.clone()),
                _ => out.whole = true,
            },
            RefKind::ValueBind(id) => templates.add(ir, *id, out),
            _ => {}
        },
        ExprKind::Path(base, selectors) => {
            if selectors
                .iter()
                .any(|s| matches!(s.kind, SelectorKind::Context(_)))
            {
                out.switch = true;
            }
            let root_reference = match &ir.exprs[*base].kind {
                ExprKind::Ref(RefKind::Root) => Some(None),
                ExprKind::Ref(RefKind::SelfValue) if at_root(0) => Some(None),
                ExprKind::Ref(RefKind::Parent(n)) if at_root(*n) => Some(None),
                ExprKind::Ref(RefKind::Bind { up, target }) if at_root(*up) => Some(target.first()),
                _ => None,
            };
            match root_reference {
                Some(target) => match target.or_else(|| selectors.first()).map(|s| &s.kind) {
                    Some(SelectorKind::Field(name)) => out.keys.push(name.clone()),
                    _ => out.whole = true,
                },
                None => collect_reads(ir, *base, depth, templates, out),
            }
        }
        ExprKind::Block(block) => {
            let block = &ir.blocks[*block];
            for &spread in &block.spreads {
                collect_reads(ir, spread, depth + 1, templates, out);
            }
            for entry in &block.entries {
                if let Some(value) = entry.value {
                    collect_reads(ir, value, depth + 1, templates, out);
                }
            }
            for &bind in &block.value_binds {
                collect_reads(ir, ir.value_binds[bind].expr, depth + 1, templates, out);
            }
        }
        ExprKind::List(items) | ExprKind::Tuple(items) | ExprKind::Named(_, items) => {
            for &item in items {
                collect_reads(ir, item, depth, templates, out);
            }
        }
        ExprKind::Neg(operand) => collect_reads(ir, *operand, depth, templates, out),
        ExprKind::Binary(_, lhs, rhs) => {
            collect_reads(ir, *lhs, depth, templates, out);
            collect_reads(ir, *rhs, depth, templates, out);
        }
        _ => {}
    }
}

/// What each template reads, computed once.
#[derive(Default)]
struct TemplateReads {
    done: std::collections::HashMap<BindId, Reads>,
    in_progress: std::collections::HashSet<BindId>,
    /// The templates; other value binds are inside blocks, whose
    /// expressions are walked with the block.
    templates: std::collections::HashSet<BindId>,
}

impl TemplateReads {
    fn add(&mut self, ir: &Ir, id: BindId, out: &mut Reads) {
        if !self.templates.contains(&id) || !self.in_progress.insert(id) {
            return;
        }
        if !self.done.contains_key(&id) {
            let mut reads = Reads::default();
            // A template's expression is evaluated in the root's chain.
            collect_reads(ir, ir.value_binds[id].expr, 0, self, &mut reads);
            self.done.insert(id, reads);
        }
        self.in_progress.remove(&id);
        let reads = &self.done[&id];
        out.keys.extend(reads.keys.iter().cloned());
        out.whole |= reads.whole;
        out.switch |= reads.switch;
    }
}

struct KeyPlan {
    /// Every operation on the key, in document order.
    all: Vec<usize>,
    /// What the key's operations read late.
    reads: Reads,
    /// The base operations, in order (`SPEC.md` 7.2).
    base: Vec<usize>,
    /// The conditional operations, in the order of `SPEC.md` 7.2: layer by
    /// layer, definitions first, then document and source order.
    conditional: Vec<usize>,
}

/// Evaluates lowered data-mode documents: one chain of operations per root
/// key, in every evaluated context (`SPEC.md` 7, 8).
pub(crate) fn evaluate_data(
    ir: &Ir,
    input: &DataInput<'_>,
) -> (Option<DataResult>, Vec<Diagnostic>) {
    let mut diagnostics = Vec::new();
    let Some(plan) = DataPlan::new(ir, input, &mut diagnostics) else {
        return (None, diagnostics);
    };
    let contexts = match plan.evaluated_contexts(input) {
        Ok(contexts) => contexts,
        Err(message) => {
            let span = input.ops.first().map_or(Span::detached(), |op| op.span);
            diagnostics.push(Diagnostic::error(span, message));
            return (None, diagnostics);
        }
    };
    let mut evaluator = Evaluator {
        ir,
        diagnostics: Vec::new(),
        stack: Vec::new(),
        steps: 0,
        contexts: Vec::new(),
        plan: Some(&plan),
        share: false,
        base_created: Vec::new(),
        defaults: input.defaults,
    };
    // Errors in the static structure of the chains are reported once.
    evaluator.check_chains(input);
    diagnostics.append(&mut evaluator.diagnostics);
    if diagnostics.iter().any(Diagnostic::is_error) {
        diagnostics.sort_by_key(|d| (d.span.file, d.span.range.start()));
        return (None, diagnostics);
    }

    let mut found: Vec<(Diagnostic, Vec<usize>)> = Vec::new();
    let mut record = |evaluator: &mut Evaluator<'_>, index: usize| {
        for diagnostic in evaluator.diagnostics.drain(..) {
            match found.iter_mut().find(|(d, _)| *d == diagnostic) {
                Some((_, in_contexts)) => in_contexts.push(index),
                None => found.push((diagnostic, vec![index])),
            }
        }
    };

    // The base, in full.
    let base_id = evaluator.context_state(&contexts[0]);
    let base_cell = evaluator.contexts[base_id].root_cell.clone();
    let base = evaluator
        .full_value(&base_cell, Span::detached(), &mut Vec::new())
        .ok();
    let base_failed = base.is_none() || !evaluator.diagnostics.is_empty();
    evaluator.share = !base_failed && !input.evaluate_every_key;
    record(&mut evaluator, 0);
    let base_keyed = match base.as_ref().map(|v| &v.kind) {
        Some(ValueKind::Keyed(keyed)) => keyed.clone(),
        _ => Keyed::default(),
    };

    let mut per_context = Vec::with_capacity(contexts.len() - 1);
    for (index, context) in contexts.iter().enumerate().skip(1) {
        evaluator.steps = 0;
        let id = evaluator.context_state(context);
        let root_cell = evaluator.contexts[id].root_cell.clone();
        // Only keys that can differ are evaluated again, unless the base
        // failed: then every context's own errors are wanted.
        let varying = evaluator.share.then(|| plan.varying(context));
        let slots = match &*root_cell.state.borrow() {
            State::Done(Ok(shaped)) => match &shaped.shape {
                Shape::Keyed(keyed) => keyed.slots.clone(),
                _ => IndexMap::new(),
            },
            _ => IndexMap::new(),
        };
        let mut changes = IndexMap::new();
        let mut failed = false;
        for (key, slot) in &slots {
            let base_value = base_keyed.entries.get(key);
            let name = match key {
                Key::Str(name) => name.as_str(),
                Key::Int(_) => "",
            };
            if let (Some(varying), Some(_)) = (&varying, base_value)
                && !varying.contains(name)
            {
                continue;
            }
            match evaluator.full_value(
                &slot.value,
                Span::detached(),
                &mut vec![(Rc::as_ptr(&root_cell), None)],
            ) {
                Ok(value) => {
                    if !base_value.is_some_and(|b| b.identical(&value)) {
                        changes.insert(key.clone(), Some(value));
                    }
                }
                Err(_) => failed = true,
            }
        }
        // Base keys the context removes, in root order before its own keys.
        let removed: Vec<Key> = base_keyed
            .entries
            .keys()
            .filter(|k| !slots.contains_key(*k))
            .cloned()
            .collect();
        let mut ordered = IndexMap::with_capacity(changes.len() + removed.len());
        for key in base_keyed.entries.keys() {
            if let Some(change) = changes.shift_remove(key) {
                ordered.insert(key.clone(), change);
            } else if removed.contains(key) {
                ordered.insert(key.clone(), None);
            }
        }
        ordered.extend(changes);
        record(&mut evaluator, index);
        if !failed {
            per_context.push(ContextData {
                context: context.clone(),
                changes: ordered,
            });
        }
    }
    evaluator.check_variant_keys(
        &base_keyed,
        &per_context,
        input.variant_keys,
        &mut diagnostics,
    );
    // An error in only some contexts says in which (`SPEC.md` 12.5).
    for (mut diagnostic, in_contexts) in found {
        if in_contexts.len() < contexts.len() {
            let mut names: Vec<String> = in_contexts
                .iter()
                .take(3)
                .map(|&i| plan.variants.display(&contexts[i]).to_string())
                .collect();
            if in_contexts.len() > 3 {
                names.push(format!("and {} more", in_contexts.len() - 3));
            }
            diagnostic.message = format!("in {}: {}", names.join(", "), diagnostic.message);
        }
        diagnostics.push(diagnostic);
    }
    diagnostics.sort_by_key(|d| (d.span.file, d.span.range.start()));
    let variants = plan.variants.clone();
    (
        Some(DataResult {
            variants,
            base,
            contexts: per_context,
        }),
        diagnostics,
    )
}

/// One operation in the history of a value (`SPEC.md` 12.3).
#[derive(Debug, Clone)]
pub struct ExplainStep {
    /// The operation's target.
    pub span: Span,
    /// The layer it is in.
    pub layer: String,
    /// The entry as written, condition included: `+web`, `web<prod>.replicas`.
    pub entry: String,
    /// What the operation is.
    pub kind: StepKind,
    /// The value at the explained path after the operation; `None` where it
    /// does not exist.
    pub value: Option<Value>,
}

/// The kind of an [`ExplainStep`] (`SPEC.md` 7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    /// A definition: a declare, ensure, or set that does not use `&`.
    Definition,
    /// A patch: a path entry, or a set that uses `&`.
    Patch,
    /// A remove.
    Remove,
}

/// A path into data: a root key, then fields, elements and accessors, as
/// `crol explain` and `crol eval` take it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathStep {
    /// `.name`
    Field(String),
    /// `.0`
    Element(usize),
    /// `[3]`: an integer key, or a list index.
    Accessor(i128),
}

impl PathStep {
    fn from_selector(selector: &Selector) -> Option<PathStep> {
        match &selector.kind {
            SelectorKind::Field(name) => Some(PathStep::Field(name.to_string())),
            SelectorKind::Element(i) => Some(PathStep::Element(*i)),
            SelectorKind::Accessor(i) => Some(PathStep::Accessor(*i)),
            SelectorKind::Context(_) => None,
        }
    }
}

/// The value at `path` in `value`.
pub fn value_at_path<'v>(value: &'v Value, path: &[PathStep]) -> Option<&'v Value> {
    let mut value = value;
    for step in path {
        value = match (&value.kind, step) {
            (ValueKind::Keyed(keyed), PathStep::Field(name)) => {
                keyed.entries.get(&KeyRef::Str(name))?
            }
            (ValueKind::Keyed(keyed), PathStep::Accessor(i)) => {
                keyed.entries.get(&KeyRef::Int(*i))?
            }
            (ValueKind::List(items), PathStep::Accessor(i)) => {
                items.get(usize::try_from(*i).ok()?)?
            }
            (ValueKind::Tuple(items) | ValueKind::NamedTuple(_, items), PathStep::Element(i)) => {
                items.get(*i)?
            }
            _ => return None,
        };
    }
    Some(value)
}

/// For each `key` + `path` in `requests`, the operations that produced the
/// value there in `context`, in order, with the value after each
/// (`SPEC.md` 12.3). Each step's value is the chain up to that operation,
/// with late references reading the final data, as they do.
pub(crate) fn explain(
    ir: &Ir,
    input: &DataInput<'_>,
    requests: &[(&str, &[PathStep])],
    context: &[String],
) -> Result<Vec<Vec<ExplainStep>>, Vec<Diagnostic>> {
    let mut diagnostics = Vec::new();
    let Some(plan) = DataPlan::new(ir, input, &mut diagnostics) else {
        return Err(diagnostics);
    };
    let mut ids = Vec::new();
    for name in context {
        match plan.variants.id(name) {
            Some(id) => ids.push(id),
            None => {
                let span = input.ops.first().map_or(Span::detached(), |op| op.span);
                return Err(vec![Diagnostic::error(
                    span,
                    format!("unknown variant `{name}`"),
                )]);
            }
        }
    }
    let context = match plan.variants.context(&ids) {
        Ok(context) => context,
        Err(message) => {
            let span = input.ops.first().map_or(Span::detached(), |op| op.span);
            return Err(vec![Diagnostic::error(span, message)]);
        }
    };
    let mut evaluator = Evaluator {
        ir,
        diagnostics: Vec::new(),
        stack: Vec::new(),
        steps: 0,
        contexts: Vec::new(),
        plan: Some(&plan),
        share: false,
        base_created: Vec::new(),
        defaults: input.defaults,
    };
    let id = evaluator.context_state(&context);
    Ok(requests
        .iter()
        .map(|(key, path)| explain_key(&mut evaluator, &plan, input, key, path, &context, id))
        .collect())
}

fn explain_key(
    evaluator: &mut Evaluator<'_>,
    plan: &DataPlan<'_>,
    input: &DataInput<'_>,
    key: &str,
    path: &[PathStep],
    context: &Context,
    id: usize,
) -> Vec<ExplainStep> {
    let Some(key_plan) = plan.keys.get(key) else {
        return Vec::new();
    };
    let order = plan.chain(key_plan, context);
    let root = evaluator.contexts[id].root.clone();
    let templates = evaluator.contexts[id].templates.clone();
    let chain = Some(Rc::new(ChainLink {
        location: Some(root.clone()),
        bind_root: false,
        next: None,
    }));
    let relevant = |op: &RootOp| {
        let op_path: Vec<PathStep> = op.path.iter().filter_map(PathStep::from_selector).collect();
        op_path.iter().zip(path).all(|(a, b)| a == b)
    };
    let mut steps = Vec::new();
    for (n, &i) in order.iter().enumerate() {
        let op = &input.ops[i];
        if !relevant(op) {
            continue;
        }
        // The chain up to this operation, evaluated afresh.
        let value = evaluator
            .apply_chain(key, &order[..=n], &chain, &root, &templates, id)
            .and_then(|(_, slot)| {
                evaluator
                    .full_value(&slot.value, op.span, &mut Vec::new())
                    .ok()
            });
        evaluator.diagnostics.clear();
        let mut entry = match op.op {
            None => "-".to_owned(),
            Some(SetOp::Declare) => "+".to_owned(),
            Some(SetOp::Ensure) => "?".to_owned(),
            Some(SetOp::Set) => String::new(),
        };
        entry.push_str(&Key::Str(key.to_owned()).to_string());
        if let Some(condition) = &op.condition {
            entry.push_str(&format!("<{condition}>"));
        }
        for selector in &op.path {
            entry.push_str(&describe_selector(selector));
        }
        steps.push(ExplainStep {
            span: op.span,
            layer: input.layer_names.get(op.layer).cloned().unwrap_or_default(),
            entry,
            kind: op.kind(),
            value: value.as_ref().and_then(|v| value_at_path(v, path)).cloned(),
        });
    }
    steps
}

impl<'a> DataPlan<'a> {
    /// Resolves conditions against the variants, declaring the names the
    /// data uses if the application declared none (`SPEC.md` 8.1), and
    /// groups operations by key.
    fn new(ir: &Ir, input: &DataInput<'a>, diagnostics: &mut Vec<Diagnostic>) -> Option<Self> {
        let mut names = Vec::new();
        for op in input.ops {
            if let Some(condition) = &op.condition {
                condition.names(&mut names);
            }
        }
        for expr in &ir.exprs {
            if let ExprKind::Path(_, selectors) = &expr.kind {
                for selector in selectors {
                    if let SelectorKind::Context(changes) = &selector.kind {
                        names.extend(changes.iter().map(|(name, _, span)| (name.clone(), *span)));
                    }
                }
            }
        }
        let mut variants = input.variants.clone();
        if variants.is_empty() {
            for (name, _) in &names {
                if variants.id(name).is_none() {
                    let _ = variants.add_discrete(name);
                }
            }
        }
        for (name, span) in &names {
            if variants.id(name).is_none() {
                diagnostics.push(Diagnostic::error(
                    *span,
                    format!("unknown variant `{name}`"),
                ));
            }
        }
        if diagnostics.iter().any(Diagnostic::is_error) {
            return None;
        }
        let conditions: Vec<Option<Resolved>> = input
            .ops
            .iter()
            .map(|op| op.condition.as_ref().and_then(|c| c.resolve(&variants)))
            .collect();
        // A condition that cannot hold is a mistake (`SPEC.md` 8.2); checked
        // where the contexts are few enough to try.
        if variants.context_count() <= 1 << 16 {
            let all = variants.all_contexts();
            let mut reported = std::collections::HashSet::new();
            for (op, condition) in input.ops.iter().zip(&conditions) {
                if let Some(condition) = condition
                    && !all.iter().any(|c| condition.holds(c))
                    && reported.insert(condition.normalized())
                {
                    diagnostics.push(Diagnostic::error(
                        op.span,
                        "this condition cannot hold in any context",
                    ));
                }
            }
        }
        let mut keys: IndexMap<Rc<str>, KeyPlan> = IndexMap::new();
        let mut indices: IndexMap<Rc<str>, Vec<usize>> = IndexMap::new();
        for (i, op) in input.ops.iter().enumerate() {
            indices.entry(op.key.clone()).or_default().push(i);
        }
        // A layer's second definition under one condition is an error, which
        // `check_chains` reports; the chain keeps the first.
        let ordered = |list: Vec<usize>| -> Vec<usize> {
            let mut list = list;
            list.sort_by_key(|&i| (input.ops[i].layer, !input.ops[i].is_definition(), i));
            let mut seen = std::collections::HashSet::new();
            list.retain(|&i| {
                let op = &input.ops[i];
                !op.is_definition()
                    || seen.insert((op.layer, conditions[i].as_ref().map(Resolved::normalized)))
            });
            list
        };
        let mut template_reads = TemplateReads {
            templates: input.templates.iter().copied().collect(),
            ..TemplateReads::default()
        };
        let mut dependents: std::collections::HashMap<Rc<str>, Vec<Rc<str>>> = Default::default();
        for (key, list) in indices {
            let mut reads = Reads::default();
            for &i in &list {
                if let Some(value) = input.ops[i].value {
                    collect_reads(
                        ir,
                        value,
                        input.ops[i].blocks.len(),
                        &mut template_reads,
                        &mut reads,
                    );
                }
            }
            reads.keys.sort();
            reads.keys.dedup();
            for read in &reads.keys {
                dependents
                    .entry(read.clone())
                    .or_default()
                    .push(key.clone());
            }
            let (base, conditional): (Vec<usize>, Vec<usize>) =
                list.iter().partition(|&&i| conditions[i].is_none());
            keys.insert(
                key,
                KeyPlan {
                    all: list,
                    reads,
                    base: ordered(base),
                    conditional: ordered(conditional),
                },
            );
        }
        Some(Self {
            ops: input.ops,
            templates: input.templates,
            variants,
            conditions,
            keys,
            dependents,
        })
    }

    /// The keys whose value can differ from the base in `context`: those
    /// with an operation whose condition holds there, and those that read
    /// them, directly or not. Every other key has its base value.
    fn varying(&self, context: &Context) -> std::collections::HashSet<Rc<str>> {
        let mut varying = std::collections::HashSet::new();
        let mut queue = Vec::new();
        for (key, plan) in &self.keys {
            let conditional = plan.conditional.iter().any(|&i| {
                self.conditions[i]
                    .as_ref()
                    .is_some_and(|c| c.holds(context))
            });
            if conditional || plan.reads.switch {
                varying.insert(key.clone());
                queue.push(key.clone());
            }
        }
        if !varying.is_empty() {
            for (key, plan) in &self.keys {
                if plan.reads.whole && varying.insert(key.clone()) {
                    queue.push(key.clone());
                }
            }
        }
        while let Some(key) = queue.pop() {
            for dependent in self.dependents.get(&key).into_iter().flatten() {
                if varying.insert(dependent.clone()) {
                    queue.push(dependent.clone());
                }
            }
        }
        varying
    }

    /// The contexts to evaluate: the base, then the requested ones or every
    /// valid one (`SPEC.md` 8.1).
    fn evaluated_contexts(&self, input: &DataInput<'_>) -> Result<Vec<Context>, String> {
        let mut contexts = vec![Context::base()];
        match input.contexts {
            Some(requested) => {
                for names in requested {
                    let mut ids = Vec::new();
                    for name in names {
                        ids.push(
                            self.variants
                                .id(name)
                                .ok_or_else(|| format!("unknown variant `{name}` in a context"))?,
                        );
                    }
                    let context = self.variants.context(&ids)?;
                    if !contexts.contains(&context) {
                        contexts.push(context);
                    }
                }
            }
            None => {
                let count = self.variants.context_count();
                if count > input.max_contexts as u128 {
                    return Err(format!(
                        "the variants make {count} contexts, more than the {} allowed; list the contexts to evaluate",
                        input.max_contexts
                    ));
                }
                contexts.extend(
                    self.variants
                        .all_contexts()
                        .into_iter()
                        .filter(|c| !c.is_base()),
                );
            }
        }
        Ok(contexts)
    }

    /// A key's operations in a context: the base ones, then those whose
    /// condition holds, by specificity (`SPEC.md` 8.3).
    fn chain(&self, key: &KeyPlan, context: &Context) -> Vec<usize> {
        let mut conditional: Vec<(usize, usize)> = key
            .conditional
            .iter()
            .filter_map(|&i| {
                let condition = self.conditions[i].as_ref()?;
                condition
                    .holds(context)
                    .then(|| (condition.specificity(&self.variants), i))
            })
            .collect();
        // Stable, so equal specificities keep the order of `SPEC.md` 7.2.
        conditional.sort_by_key(|(specificity, _)| *specificity);
        let mut chain = key.base.clone();
        chain.extend(conditional.into_iter().map(|(_, i)| i));
        chain
    }
}

impl Evaluator<'_> {
    /// Reports the errors in the chains that do not depend on the context:
    /// two definitions under one condition in one layer, and patches from
    /// several documents of one layer (`SPEC.md` 7.2, 8.3).
    fn check_chains(&mut self, input: &DataInput<'_>) {
        let plan = self.plan.expect("data mode");
        let ops = plan.ops;
        for (key, key_plan) in &plan.keys {
            let all = &key_plan.all;
            let mut layers: Vec<usize> = all.iter().map(|&i| ops[i].layer).collect();
            layers.sort_unstable();
            layers.dedup();
            for layer in layers {
                let layer_name = input.layer_names.get(layer).map_or("", String::as_str);
                let in_layer: Vec<usize> = all
                    .iter()
                    .copied()
                    .filter(|&i| ops[i].layer == layer)
                    .collect();
                let mut definitions: Vec<(String, usize)> = Vec::new();
                for &i in in_layer.iter().filter(|&&i| ops[i].is_definition()) {
                    let condition = plan.conditions[i]
                        .as_ref()
                        .map(Resolved::normalized)
                        .unwrap_or_default();
                    match definitions.iter().find(|(c, _)| *c == condition) {
                        None => definitions.push((condition, i)),
                        Some(&(_, first)) => {
                            let under = if condition.is_empty() {
                                ""
                            } else {
                                " under one condition"
                            };
                            let diagnostic = Diagnostic::error(
                                ops[i].span,
                                format!("`{key}` is defined twice{under} in the layer `{layer_name}`; a layer may define a key once"),
                            )
                            .with_related(ops[first].span, "the other definition");
                            self.report(diagnostic);
                        }
                    }
                }
                if input.warn_cross_file_patches {
                    let patches: Vec<usize> = in_layer
                        .iter()
                        .copied()
                        .filter(|&i| !ops[i].is_definition())
                        .collect();
                    let mut documents: Vec<usize> =
                        patches.iter().map(|&i| ops[i].document).collect();
                    documents.sort_unstable();
                    documents.dedup();
                    if documents.len() > 1 {
                        let mut diagnostic = Diagnostic::warning(
                            ops[patches[0]].span,
                            format!(
                                "`{key}` is patched from {} documents of the layer `{layer_name}`, so the order of the patches depends on the file names",
                                documents.len()
                            ),
                        );
                        for &i in &patches[1..] {
                            diagnostic = diagnostic.with_related(ops[i].span, "another patch");
                        }
                        self.report(diagnostic);
                    }
                }
            }
        }
    }

    /// Checks the keys of each context against the base (`SPEC.md` 8.5).
    fn check_variant_keys(
        &self,
        base: &Keyed,
        contexts: &[ContextData],
        rule: VariantKeys,
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        if rule == VariantKeys::Any {
            return;
        }
        let variants = &self.plan.expect("data mode").variants;
        for data in contexts {
            for (key, change) in &data.changes {
                match (change, base.entries.get(key)) {
                    (Some(value), None) => diagnostics.push(Diagnostic::error(
                        value.span,
                        format!(
                            "in {}: `{key}` does not exist in the base, and the variant keys rule is `{rule:?}`",
                            variants.display(&data.context)
                        ),
                    )),
                    (None, Some(value)) if rule == VariantKeys::SameKeys => {
                        diagnostics.push(Diagnostic::error(
                            value.span,
                            format!(
                                "`{key}` does not exist in {}, and the variant keys rule is `SameKeys`",
                                variants.display(&data.context)
                            ),
                        ));
                    }
                    _ => {}
                }
            }
        }
    }

    /// The index of a context's state, evaluating its root if it is new.
    fn context_state(&mut self, context: &Context) -> usize {
        if let Some(i) = self.contexts.iter().position(|c| c.context == *context) {
            return i;
        }
        let id = self.contexts.len();
        let root_cell = Rc::new(Cell {
            state: RefCell::new(State::Running),
            expr: None,
            location: None,
        });
        let root = root_location(&root_cell);
        // Root entries are in the root, so `&self` there is the root.
        let chain = Some(Rc::new(ChainLink {
            location: Some(root.clone()),
            bind_root: false,
            next: None,
        }));
        self.contexts.push(ContextState {
            context: context.clone(),
            root_cell: root_cell.clone(),
            root: root.clone(),
            templates: None,
        });
        let plan = self.plan.expect("only data mode has more than one context");
        let templates = self.bind_cells(plan.templates, &chain, None, id, true);
        self.contexts[id].templates = templates.clone();

        // A key that cannot differ from the base shares the base's cell.
        let shared = (self.share && id > 0).then(|| {
            let base = &self.contexts[0];
            let slots = match &*base.root_cell.state.borrow() {
                State::Done(Ok(shaped)) => match &shaped.shape {
                    Shape::Keyed(keyed) => keyed.slots.clone(),
                    _ => IndexMap::new(),
                },
                _ => IndexMap::new(),
            };
            (plan.varying(context), slots)
        });
        let mut slots = Vec::new();
        for (key, key_plan) in &plan.keys {
            if let Some((varying, base_slots)) = &shared
                && !varying.contains(key)
            {
                let key = Key::Str(key.to_string());
                if let Some((position, _, slot)) = base_slots.get_full(&key) {
                    slots.push((self.base_created[position], key, slot.clone()));
                }
                continue;
            }
            let order = plan.chain(key_plan, context);
            if let Some((created, slot)) =
                self.apply_chain(key, &order, &chain, &root, &templates, id)
            {
                slots.push((created, Key::Str(key.to_string()), slot));
            }
        }
        // Keys are in the order they were created, by layer and position.
        slots.sort_by_key(|(created, _, _)| *created);
        if id == 0 {
            self.base_created = slots.iter().map(|(created, _, _)| *created).collect();
        }
        let keyed = KeyedShape {
            slots: slots
                .into_iter()
                .map(|(_, key, slot)| (key, slot))
                .collect(),
            ..KeyedShape::default()
        };
        *root_cell.state.borrow_mut() = State::Done(Ok(Rc::new(Shaped {
            shape: Shape::Keyed(keyed),
            span: Span::detached(),
        })));
        id
    }

    /// The chain of blocks an operation is evaluated in: the root, and for an
    /// operation lifted out of blocks, those blocks' locations.
    fn op_chain(
        &self,
        root: &Location,
        root_chain: &Chain,
        key: &str,
        blocks: &[Vec<Selector>],
    ) -> Chain {
        let mut chain = root_chain.clone();
        for path in blocks.iter().rev() {
            let mut location = root.child(Step::Key(Key::Str(key.to_owned())));
            for step in path.iter().filter_map(selector_step) {
                location = location.child(step);
            }
            chain = Some(Rc::new(ChainLink {
                location: Some(location),
                bind_root: false,
                next: chain,
            }));
        }
        chain
    }

    /// Applies a key's operations in order (`SPEC.md` 7.4). Returns the
    /// final value, if the key exists, and the operation that created it.
    fn apply_chain(
        &mut self,
        key: &str,
        order: &[usize],
        root_chain: &Chain,
        root: &Location,
        vbinds: &VBinds,
        context: usize,
    ) -> Option<(usize, Slot)> {
        let ops = self.plan.expect("data mode").ops;
        let here = root.child(Step::Key(Key::Str(key.to_owned())));
        let mut value: Option<(usize, Lazy)> = None;
        for &i in order {
            let op = &ops[i];
            let chain = self.op_chain(root, root_chain, key, &op.blocks);
            if !op.path.is_empty() {
                let Some((created, base)) = value.clone() else {
                    let mut target = match op.op {
                        None => format!("-{key}"),
                        Some(SetOp::Set) => key.to_owned(),
                        Some(set) => format!("{}{key}", set.symbol()),
                    };
                    for selector in &op.path {
                        target.push_str(&describe_selector(selector));
                    }
                    let _ = self.fail(
                        op.span,
                        format!("{target}: `{key}` does not exist, so it cannot be patched"),
                    );
                    continue;
                };
                let entry = op.value.map(|expr| {
                    let mut below = Below::Value(base.clone());
                    for selector in &op.path {
                        below = Below::Select(Rc::new(below), selector.clone());
                    }
                    Rc::new(EntryRecipe {
                        expr,
                        below,
                        vbinds: vbinds.clone(),
                    })
                });
                let recipe = Rc::new(PathRecipe {
                    base: Recipe::Fixed,
                    base_value: base.clone(),
                    path: op.path.clone(),
                    op: op.op,
                    value: entry,
                    span: op.span,
                });
                let cell = Cell::new(
                    Job::Path {
                        base,
                        recipe,
                        chain,
                        here: Some(here.clone()),
                        context,
                    },
                    Some(here.clone()),
                );
                value = Some((created, cell));
                continue;
            }
            match op.op {
                None => {
                    if value.take().is_none() {
                        let _ = self.fail(op.span, format!("-: `{key}` does not exist"));
                    }
                }
                Some(set) => {
                    if let Some((created, _)) = &value {
                        match set {
                            SetOp::Declare => {
                                let diagnostic = Diagnostic::error(
                                    op.span,
                                    format!("+: `{key}` already exists"),
                                )
                                .with_related(ops[*created].span, "it is created here");
                                self.report(diagnostic);
                                continue;
                            }
                            SetOp::Ensure => continue,
                            SetOp::Set => {}
                        }
                    }
                    let below = match (&value, set) {
                        (Some((_, cell)), SetOp::Set) => Below::Value(cell.clone()),
                        _ => Below::Missing,
                    };
                    let cell = Cell::new(
                        Job::Expr {
                            expr: op.value.expect("a set has a value"),
                            env: Env {
                                chain,
                                here: Some(here.clone()),
                                below: below.clone(),
                                outer_below: below,
                                vbinds: vbinds.clone(),
                                context,
                            },
                        },
                        Some(here.clone()),
                    );
                    let created = value.as_ref().map_or(i, |(created, _)| *created);
                    value = Some((created, cell));
                }
            }
        }
        value.map(|(created, cell)| {
            (
                created,
                Slot {
                    value: cell,
                    recipe: Recipe::Fixed,
                },
            )
        })
    }

    /// Cells for value binds, each at a location of its own, visible to
    /// each other and to `vbinds`'s binds (`SPEC.md` 6.1.2). `templates`
    /// says whether these are the data's templates, which exist in every
    /// context.
    fn bind_cells(
        &self,
        binds: &[BindId],
        chain: &Chain,
        mut vbinds: VBinds,
        context: usize,
        templates: bool,
    ) -> VBinds {
        let ir = self.ir;
        let mut cells = Vec::with_capacity(binds.len());
        for &bind in binds {
            let cell = Rc::new(Cell {
                state: RefCell::new(State::Running),
                expr: Some(ir.value_binds[bind].expr),
                location: None,
            });
            vbinds = Some(Rc::new(VBindLink {
                id: bind,
                cell: cell.clone(),
                next: vbinds,
            }));
            cells.push((bind, cell));
        }
        for (bind, cell) in cells {
            let here = Location {
                base: Rc::downgrade(&cell),
                anchor: if templates {
                    Anchor::Template(bind)
                } else {
                    Anchor::Local
                },
                name: ir.value_binds[bind].name.clone(),
                path: None,
            };
            *cell.state.borrow_mut() = State::Pending(Box::new(Job::Expr {
                expr: ir.value_binds[bind].expr,
                env: Env {
                    chain: chain.clone(),
                    here: Some(here),
                    below: Below::Missing,
                    outer_below: Below::Missing,
                    vbinds: vbinds.clone(),
                    context,
                },
            }));
        }
        vbinds
    }

    // ---- Contexts in references (`SPEC.md` 8.4) ----

    /// The location a late reference names, without evaluating it.
    fn reference_location(&mut self, expr: ExprId, env: &Env) -> Eval<Location> {
        let ir = self.ir;
        let span = ir.exprs[expr].span;
        match &ir.exprs[expr].kind {
            ExprKind::Ref(RefKind::SelfValue) => self.chain_location(&env.chain, 0, span, "&self"),
            ExprKind::Ref(RefKind::Parent(n)) => {
                self.chain_location(&env.chain, *n, span, "&parent")
            }
            ExprKind::Ref(RefKind::Root) => Ok(self.contexts[env.context].root.clone()),
            ExprKind::Ref(RefKind::Bind { up, target }) => {
                let mut location = self.chain_location(&env.chain, *up, span, "bind")?;
                for step in target.iter().filter_map(selector_step) {
                    location = location.child(step);
                }
                Ok(location)
            }
            ExprKind::Ref(RefKind::ValueBind(id)) => {
                let mut link = env.vbinds.as_ref();
                while let Some(l) = link {
                    if l.id == *id {
                        let template = self.plan.is_some_and(|p| p.templates.contains(id));
                        return Ok(Location {
                            base: Rc::downgrade(&l.cell),
                            anchor: if template {
                                Anchor::Template(*id)
                            } else {
                                Anchor::Local
                            },
                            name: self.ir.value_binds[*id].name.clone(),
                            path: None,
                        });
                    }
                    link = l.next.as_ref();
                }
                Err(self.fail(span, "internal error: a value bind is not in scope"))
            }
            _ => Err(self.fail(span, "a variant selector must follow a late reference")),
        }
    }

    /// Reads a reference in another context: `&root.db<dev>`.
    fn in_context(
        &mut self,
        base: ExprId,
        selectors: &[Selector],
        env: &Env,
        span: Span,
    ) -> Eval<Rc<Shaped>> {
        let Some(plan) = self.plan else {
            return Err(self.fail(span, "variants need layered data"));
        };
        let mut location = self.reference_location(base, env)?;
        let mut changes = Vec::new();
        for selector in selectors {
            match (&selector.kind, selector_step(selector)) {
                (SelectorKind::Context(names), _) => {
                    for (name, active, name_span) in names {
                        match plan.variants.id(name) {
                            Some(id) => changes.push((id, *active)),
                            None => {
                                return Err(
                                    self.fail(*name_span, format!("unknown variant `{name}`"))
                                );
                            }
                        }
                    }
                }
                (_, Some(step)) => location = location.child(step),
                (_, None) => {}
            }
        }
        let current = self.contexts[env.context].context.clone();
        let target = plan.variants.switch(&current, &changes);
        let target = self.context_state(&target);
        let mapped = match location.anchor {
            Anchor::Root => self.contexts[target].root.clone(),
            Anchor::Template(id) => {
                let mut link = self.contexts[target].templates.as_ref();
                let mut found = None;
                while let Some(l) = link {
                    if l.id == id {
                        found = Some(Location {
                            base: Rc::downgrade(&l.cell),
                            anchor: Anchor::Template(id),
                            name: location.name.clone(),
                            path: None,
                        });
                        break;
                    }
                    link = l.next.as_ref();
                }
                match found {
                    Some(found) => found,
                    None => return Err(self.fail(span, "internal error: a template is missing")),
                }
            }
            Anchor::Local => {
                return Err(self.fail(
                    span,
                    "a value bind inside a block has no other contexts; use `&root` or a template",
                ));
            }
        };
        let mut mapped = mapped;
        for step in location.steps() {
            mapped = mapped.child(step);
        }
        self.value_at(&mapped, span)
    }

    // ---- Errors ----

    /// Reports an error and returns the poison that propagates it.
    fn fail(&mut self, span: Span, message: impl Into<String>) -> Fail {
        self.report(Diagnostic::error(span, message));
        Fail::Poison
    }

    fn report(&mut self, mut diagnostic: Diagnostic) {
        if diagnostic.context.is_none() {
            diagnostic.context = self
                .stack
                .iter()
                .rev()
                .find_map(|(cell, _)| cell.location.as_ref().map(Location::describe));
        }
        self.diagnostics.push(diagnostic);
    }

    // ---- Cells ----

    fn force(&mut self, cell: &Lazy, span: Span) -> Eval<Rc<Shaped>> {
        let job = {
            let mut state = cell.state.borrow_mut();
            match &*state {
                State::Done(result) => return result.clone().map_err(Fail::from),
                State::Running => {
                    drop(state);
                    return Err(self.cycle(cell, span));
                }
                State::Pending(_) => match std::mem::replace(&mut *state, State::Running) {
                    State::Pending(job) => *job,
                    _ => unreachable!(),
                },
            }
        };
        self.steps += 1;
        if self.steps == MAX_STEPS {
            let message = format!(
                "evaluation stopped after {MAX_STEPS} steps; does a value spread a value that contains it?"
            );
            self.report(Diagnostic::error(span, message));
        }
        if self.steps >= MAX_STEPS {
            *cell.state.borrow_mut() = State::Done(Err(Poisoned));
            return Err(Fail::Poison);
        }
        if self.stack.len() >= MAX_DEPTH {
            let result = Err(self.fail(
                span,
                format!("references are nested too deeply; the limit is {MAX_DEPTH} levels"),
            ));
            *cell.state.borrow_mut() = State::Done(Err(Poisoned));
            return result;
        }
        self.stack.push((cell.clone(), span));
        let result = self.run(job);
        self.stack.pop();
        let stored = match result {
            Ok(shaped) => Ok(shaped),
            Err(Fail::Missing(span, message)) => {
                let mut diagnostic = Diagnostic::error(span, message);
                diagnostic.context = cell.location.as_ref().map(Location::describe);
                self.diagnostics.push(diagnostic);
                Err(Poisoned)
            }
            Err(Fail::Poison) => Err(Poisoned),
        };
        *cell.state.borrow_mut() = State::Done(stored.clone());
        stored.map_err(Fail::from)
    }

    fn cycle(&mut self, cell: &Lazy, span: Span) -> Fail {
        let start = self
            .stack
            .iter()
            .position(|(c, _)| Rc::ptr_eq(c, cell))
            .unwrap_or(0);
        let mut diagnostic = Diagnostic::error(span, "cycle: this value depends on itself");
        for (_, through) in &self.stack[start + 1..] {
            diagnostic = diagnostic.with_related(*through, "through this reference");
        }
        if let Some((_, first)) = self.stack.get(start) {
            diagnostic = diagnostic.with_related(*first, "which is evaluated here");
        }
        diagnostic.context = cell.location.as_ref().map(Location::describe);
        self.diagnostics.push(diagnostic);
        Fail::Poison
    }

    fn run(&mut self, job: Job) -> Eval<Rc<Shaped>> {
        match job {
            Job::Expr { expr, env } => self.eval(expr, &env),
            Job::Path {
                base,
                recipe,
                chain,
                here,
                context,
            } => {
                let shaped = self.force(&base, recipe.span)?;
                self.apply_path(&shaped, &recipe.path, &recipe, &chain, here, context)
            }
        }
    }

    fn instantiate(
        &self,
        recipe: &Recipe,
        value: &Lazy,
        chain: &Chain,
        here: Option<Location>,
        context: usize,
    ) -> Lazy {
        match recipe {
            Recipe::Fixed => value.clone(),
            Recipe::Entry(entry) => Cell::new(
                Job::Expr {
                    expr: entry.expr,
                    env: Env {
                        chain: chain.clone(),
                        here: here.clone(),
                        below: entry.below.clone(),
                        outer_below: entry.below.clone(),
                        vbinds: entry.vbinds.clone(),
                        context,
                    },
                },
                here,
            ),
            Recipe::Path(path) => {
                let base =
                    self.instantiate(&path.base, &path.base_value, chain, here.clone(), context);
                Cell::new(
                    Job::Path {
                        base,
                        recipe: path.clone(),
                        chain: chain.clone(),
                        here: here.clone(),
                        context,
                    },
                    here,
                )
            }
        }
    }

    // ---- Expressions ----

    fn shaped(shape: Shape, span: Span) -> Eval<Rc<Shaped>> {
        Ok(Rc::new(Shaped { shape, span }))
    }

    fn eval(&mut self, expr: ExprId, env: &Env) -> Eval<Rc<Shaped>> {
        let ir = self.ir;
        let span = ir.exprs[expr].span;
        match &ir.exprs[expr].kind {
            ExprKind::Unit => Self::shaped(Shape::Unit, span),
            ExprKind::Bool(b) => Self::shaped(Shape::Bool(*b), span),
            ExprKind::Int(i) => Self::shaped(Shape::Int(*i), span),
            ExprKind::Float(f) => Self::shaped(Shape::Float(*f), span),
            ExprKind::Char(c) => Self::shaped(Shape::Char(*c), span),
            ExprKind::Str(s) => Self::shaped(Shape::Str(s.clone()), span),
            ExprKind::Id(s) => Self::shaped(Shape::Id(s.clone()), span),
            ExprKind::UnitName(n) => Self::shaped(Shape::UnitName(n.clone()), span),
            ExprKind::Ref(kind) => self.reference(kind, env, span),
            ExprKind::Block(block) => self.block(*block, None, env, span),
            ExprKind::List(elements) => {
                let cells = self.elements(elements, env);
                Self::shaped(Shape::List(cells), span)
            }
            ExprKind::Tuple(elements) => {
                let cells = self.elements(elements, env);
                Self::shaped(Shape::Tuple(cells), span)
            }
            ExprKind::Named(name, elements) => {
                let cells = self.elements(elements, env);
                Self::shaped(Shape::Named(name.clone(), cells), span)
            }
            ExprKind::Path(base, selectors)
                if selectors
                    .iter()
                    .any(|s| matches!(s.kind, SelectorKind::Context(_))) =>
            {
                self.in_context(*base, selectors, env, span)
            }
            ExprKind::Path(base, selectors) => {
                let mut value = self.eval(*base, &Self::detached(env))?;
                for selector in selectors {
                    let cell = self.select(&value, selector)?;
                    value = self.force(&cell, selector.span)?;
                }
                Ok(value)
            }
            ExprKind::Neg(operand) => {
                let value = self.eval(*operand, &Self::detached(env))?;
                match &value.shape {
                    Shape::Int(i) => match i.checked_neg() {
                        Some(n) => Self::shaped(Shape::Int(n), span),
                        None => Err(self.fail(span, "-: integer overflow")),
                    },
                    Shape::Float(f) => Self::shaped(Shape::Float(-f), span),
                    other => {
                        let kind = other.kind_name();
                        Err(self.fail(span, format!("-: cannot negate {kind}")))
                    }
                }
            }
            ExprKind::Binary(op, lhs, rhs) => self.binary(*op, *lhs, *rhs, env, span),
            ExprKind::Error => Err(Fail::Poison),
        }
    }

    /// The environment of an operand that is not at the entry's location.
    fn detached(env: &Env) -> Env {
        Env {
            here: None,
            outer_below: Below::Missing,
            ..env.clone()
        }
    }

    /// Cells for list, tuple and named tuple elements, at `[i]` or `.i`.
    fn elements(&mut self, elements: &[ExprId], env: &Env) -> Vec<Lazy> {
        elements
            .iter()
            .enumerate()
            .map(|(i, &element)| {
                let here = child_location(&env.here, Step::Index(i));
                let env = Env {
                    here: here.clone(),
                    outer_below: Below::Missing,
                    ..env.clone()
                };
                Cell::new(Job::Expr { expr: element, env }, here)
            })
            .collect()
    }

    // ---- References (`SPEC.md` 6.1) ----

    fn reference(&mut self, kind: &RefKind, env: &Env, span: Span) -> Eval<Rc<Shaped>> {
        match kind {
            RefKind::Below => {
                let cell = self.resolve_below(&env.below, span)?;
                self.force(&cell, span)
            }
            RefKind::SelfValue => {
                let location = self.chain_location(&env.chain, 0, span, "&self")?;
                self.value_at(&location, span)
            }
            RefKind::Parent(n) => {
                let location = self.chain_location(&env.chain, *n, span, "&parent")?;
                self.value_at(&location, span)
            }
            RefKind::Root => {
                let root = self.contexts[env.context].root.clone();
                self.value_at(&root, span)
            }
            RefKind::Default(target) => {
                let Some(defaults) = self.defaults else {
                    return Err(self.fail(
                        span,
                        "&default: defaults come from a schema, and no schema is given",
                    ));
                };
                let path: Vec<String> = target.path.iter().map(|p| p.to_string()).collect();
                match defaults.default_of(target.type_name.as_deref(), &path) {
                    Some(value) => Ok(shaped_from_value(&value)),
                    None => {
                        let what = match &target.type_name {
                            Some(name) if path.is_empty() => format!("`{name}`"),
                            Some(name) => format!("`{name}.{}`", path.join(".")),
                            None if path.is_empty() => "the root type".to_owned(),
                            None => format!("`{}` of the root type", path.join(".")),
                        };
                        Err(Fail::Missing(
                            span,
                            format!("&default: the schema records no default for {what}"),
                        ))
                    }
                }
            }
            RefKind::Bind { up, target } => {
                let location = self.chain_location(&env.chain, *up, span, "bind")?;
                let mut value = self.value_at(&location, span)?;
                for selector in target {
                    let cell = self.select(&value, selector)?;
                    value = self.force(&cell, span)?;
                }
                Ok(value)
            }
            RefKind::ValueBind(id) => {
                let mut link = env.vbinds.as_ref();
                while let Some(l) = link {
                    if l.id == *id {
                        let cell = l.cell.clone();
                        return self.force(&cell, span);
                    }
                    link = l.next.as_ref();
                }
                Err(self.fail(span, "internal error: a value bind is not in scope"))
            }
        }
    }

    /// The location of the block `up` levels out from the innermost.
    fn chain_location(
        &mut self,
        chain: &Chain,
        up: usize,
        span: Span,
        what: &str,
    ) -> Eval<Location> {
        let mut link = chain.as_ref();
        for _ in 0..up {
            match link {
                Some(l) if l.bind_root => {
                    return Err(self.fail(
                        span,
                        format!("{what}: a value bind's block has no block around it"),
                    ));
                }
                Some(l) => link = l.next.as_ref(),
                None => break,
            }
        }
        match link.map(|l| &**l) {
            None if up == 0 => Err(self.fail(span, format!("{what}: not inside a block"))),
            None => Err(self.fail(span, format!("{what}: there is no block above the top level"))),
            Some(ChainLink { location: None, .. }) => Err(self.fail(
                span,
                format!("{what}: this block has no location, so it cannot refer to itself or its surroundings"),
            )),
            Some(ChainLink {
                location: Some(location),
                ..
            }) => Ok(location.clone()),
        }
    }

    /// The final value at a location.
    fn value_at(&mut self, location: &Location, span: Span) -> Eval<Rc<Shaped>> {
        let Some(base) = location.base.upgrade() else {
            return Err(self.fail(span, "internal error: a location outlived its value"));
        };
        let mut value = self.force(&base, span)?;
        for step in location.step_refs() {
            let cell = match (&value.shape, step) {
                (Shape::Keyed(keyed), Step::Key(key)) => {
                    keyed.slots.get(key).map(|s| s.value.clone())
                }
                (Shape::List(v) | Shape::Tuple(v) | Shape::Named(_, v), Step::Index(i)) => {
                    v.get(*i).cloned()
                }
                (Shape::Keyed(keyed), Step::Accessor(i)) => {
                    keyed.slots.get(&Key::Int(*i)).map(|s| s.value.clone())
                }
                (Shape::List(v), Step::Accessor(i)) => {
                    usize::try_from(*i).ok().and_then(|i| v.get(i).cloned())
                }
                _ => None,
            };
            let Some(cell) = cell else {
                return Err(Fail::Missing(
                    span,
                    format!("there is no value at `{}`", location.describe()),
                ));
            };
            value = self.force(&cell, span)?;
        }
        Ok(value)
    }

    fn resolve_below(&mut self, below: &Below, span: Span) -> Eval<Lazy> {
        match below {
            Below::Missing => Err(Fail::Missing(
                span,
                "&: there is no value below here".to_owned(),
            )),
            Below::Value(cell) => Ok(cell.clone()),
            Below::Select(inner, selector) => {
                let cell = self.resolve_below(inner, span)?;
                let value = self.force(&cell, span)?;
                let selector = Selector {
                    kind: selector.kind.clone(),
                    span,
                };
                match self.select(&value, &selector) {
                    Err(Fail::Missing(..)) => Err(Fail::Missing(
                        span,
                        "&: there is no value below here".to_owned(),
                    )),
                    result => result,
                }
            }
        }
    }

    /// The cell a selector names in a value. Missing keys and elements are
    /// missing; selectors that do not apply to the value are errors.
    fn select(&mut self, value: &Shaped, selector: &Selector) -> Eval<Lazy> {
        let span = selector.span;
        let found = match (&value.shape, &selector.kind) {
            (Shape::Keyed(keyed), SelectorKind::Field(name)) => {
                keyed.slots.get(&KeyRef::Str(name)).map(|s| s.value.clone())
            }
            (Shape::Keyed(keyed), SelectorKind::Accessor(i)) => {
                keyed.slots.get(&Key::Int(*i)).map(|s| s.value.clone())
            }
            (Shape::Keyed(_), SelectorKind::Element(i)) => {
                return Err(self.fail(
                    span,
                    format!(
                        ".{i}: a keyed value has no tuple elements; write `[{i}]` for the key {i}"
                    ),
                ));
            }
            (Shape::List(elements), SelectorKind::Accessor(i)) => usize::try_from(*i)
                .ok()
                .and_then(|i| elements.get(i).cloned()),
            (Shape::Tuple(elements) | Shape::Named(_, elements), SelectorKind::Element(i)) => {
                elements.get(*i).cloned()
            }
            (shape, _) => {
                let kind = shape.kind_name();
                let selector = describe_selector(selector);
                return Err(self.fail(span, format!("{selector}: cannot select in {kind}")));
            }
        };
        found.ok_or_else(|| {
            Fail::Missing(
                span,
                format!("{}: there is no such value", describe_selector(selector)),
            )
        })
    }

    // ---- Blocks (`SPEC.md` 5.2) ----

    fn block(
        &mut self,
        id: BlockId,
        start: Option<Rc<Shaped>>,
        env: &Env,
        span: Span,
    ) -> Eval<Rc<Shaped>> {
        let ir = self.ir;
        let block = &ir.blocks[id];
        let is_patch = start.is_some();
        let mut keyed = match &start {
            None => KeyedShape::default(),
            Some(start) => match &start.shape {
                Shape::Keyed(keyed) => keyed.clone(),
                other => {
                    let kind = other.kind_name();
                    return Err(self.fail(span, format!("~: cannot patch {kind} with a block")));
                }
            },
        };
        if let Some(name) = &block.name {
            if let Some(old) = &keyed.name
                && old != name
            {
                return Err(self.fail(span, format!("~: cannot merge `{old}` with `{name}`")));
            }
            keyed.name = Some(name.clone());
        }

        let location = env.here.clone();
        let bind_root = location
            .as_ref()
            .is_some_and(|l| !l.name.is_empty() && l.path.is_none());
        let chain = Some(Rc::new(ChainLink {
            location: location.clone(),
            bind_root,
            next: env.chain.clone(),
        }));
        let outer_below = if is_patch {
            Below::Missing
        } else {
            env.outer_below.clone()
        };

        // Value binds are visible anywhere in the block, and in each other.
        let vbinds = self.bind_cells(
            &block.value_binds,
            &chain,
            env.vbinds.clone(),
            env.context,
            false,
        );

        for &spread in &block.spreads {
            let spread_env = Env {
                chain: chain.clone(),
                here: location.clone(),
                below: env.below.clone(),
                outer_below: Below::Missing,
                vbinds: vbinds.clone(),
                context: env.context,
            };
            let spread_span = ir.exprs[spread].span;
            let source = self.eval(spread, &spread_env)?;
            let Shape::Keyed(source) = &source.shape else {
                let kind = source.shape.kind_name();
                return Err(self.fail(
                    spread_span,
                    format!("..: cannot spread {kind}; only keyed values can be spread"),
                ));
            };
            self.copy_entries(source, &mut keyed, &chain, &location, env.context);
        }

        let scope = BlockScope {
            chain,
            location,
            outer_below,
            vbinds,
            context: env.context,
        };
        for entry in &block.entries {
            self.entry(entry, &mut keyed, &scope);
        }
        if block.defaults {
            keyed.defaults = true;
        }
        Self::shaped(Shape::Keyed(keyed), span)
    }

    /// Sets each of `source`'s entries in `target`, evaluated again at the
    /// target's location, and passes on its defaults (`SPEC.md` 5.2).
    fn copy_entries(
        &mut self,
        source: &KeyedShape,
        target: &mut KeyedShape,
        chain: &Chain,
        location: &Option<Location>,
        context: usize,
    ) {
        for (key, slot) in &source.slots {
            let here = child_location(location, Step::Key(key.clone()));
            let value = self.instantiate(&slot.recipe, &slot.value, chain, here, context);
            target.slots.insert(
                key.clone(),
                Slot {
                    value,
                    recipe: slot.recipe.clone(),
                },
            );
            target.defaulted.remove(key);
        }
        target.defaults |= source.defaults;
        target.defaulted.extend(source.defaulted.iter().cloned());
    }

    fn entry(&mut self, entry: &Entry, keyed: &mut KeyedShape, scope: &BlockScope) {
        let BlockScope {
            chain,
            location,
            outer_below,
            vbinds,
            context,
        } = scope;
        let context = *context;
        let key = &entry.key;
        let existing = keyed.slots.get(key).cloned();
        let here = child_location(location, Step::Key(key.clone()));
        if !entry.path.is_empty() {
            let Some(slot) = existing else {
                let target = describe_target(entry);
                let _ = self.fail(
                    entry.span,
                    format!("{target}: `{key}` does not exist, so it cannot be patched"),
                );
                return;
            };
            let value = entry.value.map(|expr| {
                let mut below = Below::Value(slot.value.clone());
                for selector in &entry.path {
                    below = Below::Select(Rc::new(below), selector.clone());
                }
                Rc::new(EntryRecipe {
                    expr,
                    below,
                    vbinds: vbinds.clone(),
                })
            });
            let recipe = Recipe::Path(Rc::new(PathRecipe {
                base: slot.recipe.clone(),
                base_value: slot.value.clone(),
                path: entry.path.clone(),
                op: entry.op,
                value,
                span: entry.span,
            }));
            let value = self.instantiate(&recipe, &slot.value, chain, here, context);
            keyed.slots.insert(key.clone(), Slot { value, recipe });
            return;
        }
        match entry.op {
            None => {
                if existing.is_none() {
                    let _ = self.fail(entry.span, format!("-: `{key}` does not exist"));
                    return;
                }
                keyed.slots.shift_remove(key);
                keyed.defaulted.insert(key.clone());
            }
            Some(op) => {
                if let Some(slot) = &existing {
                    match op {
                        SetOp::Declare => {
                            let other = self.span_of(&slot.value);
                            let mut diagnostic =
                                Diagnostic::error(entry.span, format!("+: `{key}` already exists"));
                            if let Some(other) = other {
                                diagnostic = diagnostic.with_related(other, "its value");
                            }
                            self.report(diagnostic);
                            return;
                        }
                        SetOp::Ensure => return,
                        SetOp::Set => {}
                    }
                }
                let below = match &existing {
                    Some(slot) => Below::Value(slot.value.clone()),
                    None => {
                        Below::Select(Rc::new(outer_below.clone()), key_selector(key, entry.span))
                    }
                };
                let below = if op == SetOp::Set {
                    below
                } else {
                    Below::Missing
                };
                let recipe = Recipe::Entry(Rc::new(EntryRecipe {
                    expr: entry.value.expect("a set entry has a value"),
                    below,
                    vbinds: vbinds.clone(),
                }));
                let placeholder = Cell::done(Rc::new(Shaped {
                    shape: Shape::Unit,
                    span: entry.span,
                }));
                let value = self.instantiate(&recipe, &placeholder, chain, here, context);
                keyed.slots.insert(key.clone(), Slot { value, recipe });
                keyed.defaulted.remove(key);
            }
        }
    }

    /// The span of a value that is already computed, for related spans.
    fn span_of(&self, cell: &Lazy) -> Option<Span> {
        match &*cell.state.borrow() {
            State::Done(Ok(shaped)) => Some(shaped.span),
            State::Pending(job) => match &**job {
                Job::Expr { expr, .. } => Some(self.ir.exprs[*expr].span),
                Job::Path { .. } => None,
            },
            _ => None,
        }
    }

    // ---- Path entries (`SPEC.md` 5.1) ----

    fn apply_path(
        &mut self,
        value: &Rc<Shaped>,
        path: &[Selector],
        recipe: &PathRecipe,
        chain: &Chain,
        here: Option<Location>,
        context: usize,
    ) -> Eval<Rc<Shaped>> {
        let (selector, rest) = path.split_first().expect("a path entry has a path");
        let step = match (&value.shape, &selector.kind) {
            (Shape::Keyed(_), SelectorKind::Field(name)) => Step::Key(Key::Str(name.to_string())),
            (Shape::Keyed(_), SelectorKind::Accessor(i)) => Step::Key(Key::Int(*i)),
            (_, SelectorKind::Accessor(i)) => {
                Step::Index(usize::try_from(*i).unwrap_or(usize::MAX))
            }
            (_, SelectorKind::Element(i)) => Step::Index(*i),
            (_, SelectorKind::Field(name)) => Step::Key(Key::Str(name.to_string())),
            (_, SelectorKind::Context(_)) => {
                return Err(self.fail(selector.span, "internal error: a condition in a path"));
            }
        };
        let here = child_location(&here, step);
        let symbol = recipe.op.map_or("-", SetOp::symbol);
        if rest.is_empty() {
            return self.apply_last(value, selector, recipe, chain, here, context);
        }
        let child = match self.select(value, selector) {
            Ok(child) => child,
            Err(Fail::Missing(..)) => {
                let step = describe_selector(selector);
                return Err(self.fail(selector.span, format!("{symbol}: `{step}` does not exist")));
            }
            Err(fail) => return Err(fail),
        };
        let child = self.force(&child, selector.span)?;
        let new_child = self.apply_path(&child, rest, recipe, chain, here, context)?;
        Ok(self.with_child(value, selector, Cell::done(new_child)))
    }

    fn apply_last(
        &mut self,
        value: &Rc<Shaped>,
        selector: &Selector,
        recipe: &PathRecipe,
        chain: &Chain,
        here: Option<Location>,
        context: usize,
    ) -> Eval<Rc<Shaped>> {
        let span = selector.span;
        let new_value = |this: &Self| {
            recipe.value.as_ref().map(|entry| {
                let recipe = Recipe::Entry(entry.clone());
                let placeholder = Cell::done(value.clone());
                (
                    this.instantiate(&recipe, &placeholder, chain, here.clone(), context),
                    recipe,
                )
            })
        };
        match (&value.shape, &selector.kind) {
            (Shape::Keyed(keyed), SelectorKind::Field(_) | SelectorKind::Accessor(_)) => {
                let key = match &selector.kind {
                    SelectorKind::Field(name) => Key::Str(name.to_string()),
                    SelectorKind::Accessor(i) => Key::Int(*i),
                    SelectorKind::Element(_) | SelectorKind::Context(_) => unreachable!(),
                };
                let mut keyed = keyed.clone();
                let exists = keyed.slots.contains_key(&key);
                match recipe.op {
                    None if !exists => {
                        return Err(self.fail(span, format!("-: `{key}` does not exist")));
                    }
                    None => {
                        keyed.slots.shift_remove(&key);
                        keyed.defaulted.insert(key);
                    }
                    Some(SetOp::Declare) if exists => {
                        return Err(self.fail(span, format!("+: `{key}` already exists")));
                    }
                    Some(SetOp::Ensure) if exists => return Ok(value.clone()),
                    Some(_) => {
                        let (cell, recipe) = new_value(self).expect("a set entry has a value");
                        keyed.slots.insert(
                            key.clone(),
                            Slot {
                                value: cell,
                                recipe,
                            },
                        );
                        keyed.defaulted.remove(&key);
                    }
                }
                Self::shaped(Shape::Keyed(keyed), value.span)
            }
            (Shape::Keyed(_), SelectorKind::Element(i)) => Err(self.fail(
                span,
                format!(".{i}: a keyed value has no tuple elements; write `[{i}]` for the key {i}"),
            )),
            (Shape::List(elements), SelectorKind::Accessor(i)) => {
                let index = usize::try_from(*i).ok().filter(|&i| i < elements.len());
                let Some(index) = index else {
                    let symbol = recipe.op.map_or("-", SetOp::symbol);
                    return Err(self.fail(span, format!("{symbol}: the list has no element [{i}]")));
                };
                let mut elements = elements.clone();
                match recipe.op {
                    None => {
                        elements.remove(index);
                    }
                    Some(SetOp::Set) => {
                        let (cell, _) = new_value(self).expect("a set entry has a value");
                        elements[index] = cell;
                    }
                    Some(op) => {
                        return Err(self.fail(
                            span,
                            format!("{}: a list element can only be set, with `:`", op.symbol()),
                        ));
                    }
                }
                Self::shaped(Shape::List(elements), value.span)
            }
            (Shape::Tuple(elements) | Shape::Named(_, elements), SelectorKind::Element(i)) => {
                if *i >= elements.len() {
                    let symbol = recipe.op.map_or("-", SetOp::symbol);
                    return Err(self.fail(span, format!("{symbol}: there is no element .{i}")));
                }
                let mut elements = elements.clone();
                match recipe.op {
                    None => return Err(self.fail(span, "-: tuple elements cannot be removed")),
                    Some(SetOp::Set) => {
                        let (cell, _) = new_value(self).expect("a set entry has a value");
                        elements[*i] = cell;
                    }
                    Some(op) => {
                        return Err(self.fail(
                            span,
                            format!("{}: a tuple element can only be set, with `:`", op.symbol()),
                        ));
                    }
                }
                let shape = match &value.shape {
                    Shape::Named(name, _) => Shape::Named(name.clone(), elements),
                    _ => Shape::Tuple(elements),
                };
                Self::shaped(shape, value.span)
            }
            (shape, _) => {
                let kind = shape.kind_name();
                let selector = describe_selector(selector);
                Err(self.fail(span, format!("{selector}: cannot select in {kind}")))
            }
        }
    }

    /// `value` with the child that `selector` names replaced.
    fn with_child(&self, value: &Shaped, selector: &Selector, child: Lazy) -> Rc<Shaped> {
        let shape = match (&value.shape, &selector.kind) {
            (Shape::Keyed(keyed), kind) => {
                let key = match kind {
                    SelectorKind::Field(name) => Key::Str(name.to_string()),
                    SelectorKind::Accessor(i) => Key::Int(*i),
                    SelectorKind::Element(i) => Key::Int(*i as i128),
                    SelectorKind::Context(_) => {
                        unreachable!("with_child follows a successful select")
                    }
                };
                let mut keyed = keyed.clone();
                keyed.slots.insert(
                    key,
                    Slot {
                        value: child,
                        recipe: Recipe::Fixed,
                    },
                );
                Shape::Keyed(keyed)
            }
            (Shape::List(elements), SelectorKind::Accessor(i)) => {
                let mut elements = elements.clone();
                elements[*i as usize] = child;
                Shape::List(elements)
            }
            (Shape::Tuple(elements), SelectorKind::Element(i)) => {
                let mut elements = elements.clone();
                elements[*i] = child;
                Shape::Tuple(elements)
            }
            (Shape::Named(name, elements), SelectorKind::Element(i)) => {
                let mut elements = elements.clone();
                elements[*i] = child;
                Shape::Named(name.clone(), elements)
            }
            _ => unreachable!("with_child follows a successful select"),
        };
        Rc::new(Shaped {
            shape,
            span: value.span,
        })
    }

    // ---- Operators (`SPEC.md` 6.3 to 6.6) ----

    fn binary(
        &mut self,
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
        env: &Env,
        span: Span,
    ) -> Eval<Rc<Shaped>> {
        let ir = self.ir;
        match op {
            BinOp::Fallback => match self.eval(lhs, env) {
                Err(Fail::Missing(..)) => self.eval(rhs, env),
                result => result,
            },
            BinOp::Merge => {
                let left = self.eval(lhs, env)?;
                if let ExprKind::Block(block) = ir.exprs[rhs].kind {
                    let rhs_span = ir.exprs[rhs].span;
                    return self.block(block, Some(left), env, rhs_span);
                }
                let right = self.eval(rhs, env)?;
                self.merge(&left, &right, env, span)
            }
            _ => {
                let detached = Self::detached(env);
                let left = self.eval(lhs, &detached)?;
                let right = self.eval(rhs, &detached)?;
                self.arithmetic(op, &left, &right, span)
            }
        }
    }

    fn merge(&mut self, left: &Shaped, right: &Shaped, env: &Env, span: Span) -> Eval<Rc<Shaped>> {
        match (&left.shape, &right.shape) {
            (Shape::Keyed(a), Shape::Keyed(b)) => {
                if let (Some(x), Some(y)) = (&a.name, &b.name)
                    && x != y
                {
                    return Err(self.fail(span, format!("~: cannot merge `{x}` with `{y}`")));
                }
                let mut keyed = a.clone();
                if keyed.name.is_none() {
                    keyed.name = b.name.clone();
                }
                let chain = Some(Rc::new(ChainLink {
                    location: env.here.clone(),
                    bind_root: false,
                    next: env.chain.clone(),
                }));
                self.copy_entries(b, &mut keyed, &chain, &env.here, env.context);
                Self::shaped(Shape::Keyed(keyed), span)
            }
            (Shape::Named(x, _), Shape::Named(y, _)) | (Shape::UnitName(x), Shape::UnitName(y)) => {
                if x == y {
                    Ok(Rc::new(Shaped {
                        shape: match &right.shape {
                            Shape::Named(n, e) => Shape::Named(n.clone(), e.clone()),
                            _ => Shape::UnitName(y.clone()),
                        },
                        span: right.span,
                    }))
                } else {
                    Err(self.fail(span, format!("~: cannot merge `{x}` with `{y}`")))
                }
            }
            (a, b) => {
                let (a, b) = (a.kind_name(), b.kind_name());
                Err(self.fail(
                    span,
                    format!("~: cannot merge {a} with {b}; use `:` to replace a value, `+` for lists, or a path"),
                ))
            }
        }
    }

    fn arithmetic(
        &mut self,
        op: BinOp,
        left: &Shaped,
        right: &Shaped,
        span: Span,
    ) -> Eval<Rc<Shaped>> {
        use Shape::*;
        let symbol = op.symbol();
        let result = match (&left.shape, &right.shape) {
            (Int(a), Int(b)) => {
                let result = match op {
                    BinOp::Add => a.checked_add(*b),
                    BinOp::Sub => a.checked_sub(*b),
                    BinOp::Mul => a.checked_mul(*b),
                    BinOp::Div | BinOp::Rem if *b == 0 => {
                        return Err(self.fail(span, format!("{symbol}: division by zero")));
                    }
                    BinOp::Div => a.checked_div(*b),
                    BinOp::Rem => a.checked_rem(*b),
                    _ => unreachable!(),
                };
                match result {
                    Some(n) => Int(n),
                    None => return Err(self.fail(span, format!("{symbol}: integer overflow"))),
                }
            }
            (Int(_) | Float(_), Int(_) | Float(_)) => {
                let as_float = |s: &Shape| match s {
                    Int(i) => *i as f64,
                    Float(f) => *f,
                    _ => unreachable!(),
                };
                let (a, b) = (as_float(&left.shape), as_float(&right.shape));
                let result = match op {
                    BinOp::Add => a + b,
                    BinOp::Sub => a - b,
                    BinOp::Mul => a * b,
                    BinOp::Div => a / b,
                    BinOp::Rem => a % b,
                    _ => unreachable!(),
                };
                if !result.is_finite() {
                    return Err(
                        self.fail(span, format!("{symbol}: the result is not a finite float"))
                    );
                }
                Float(result)
            }
            (Str(a), Str(b)) if op == BinOp::Add => Str(format!("{a}{b}").into()),
            (Str(a), other) if op == BinOp::Add && Self::formats(other) => {
                Str(format!("{a}{}", Self::format_scalar(other)).into())
            }
            (other, Str(b)) if op == BinOp::Add && Self::formats(other) => {
                Str(format!("{}{b}", Self::format_scalar(other)).into())
            }
            (List(a), List(b)) if op == BinOp::Add => List(a.iter().chain(b).cloned().collect()),
            (List(a), List(b)) if op == BinOp::Sub => {
                let removed: Vec<Value> = b
                    .iter()
                    .map(|cell| self.full_value(cell, span, &mut Vec::new()))
                    .collect::<Result<_, _>>()?;
                let mut kept = Vec::new();
                for cell in a {
                    let value = self.full_value(cell, span, &mut Vec::new())?;
                    if !removed.iter().any(|r| r.equals(&value)) {
                        kept.push(cell.clone());
                    }
                }
                List(kept)
            }
            (a, b) => {
                let (a, b) = (a.kind_name(), b.kind_name());
                let message = match op {
                    BinOp::Add => format!("+: cannot add {b} to {a}"),
                    BinOp::Sub => format!("-: cannot subtract {b} from {a}"),
                    BinOp::Mul => format!("*: cannot multiply {a} by {b}"),
                    BinOp::Div => format!("/: cannot divide {a} by {b}"),
                    _ => format!("%: cannot take the remainder of {a} by {b}"),
                };
                return Err(self.fail(span, message));
            }
        };
        Self::shaped(result, span)
    }

    /// Whether a value can be formatted into a string (`SPEC.md` 6.6).
    fn formats(shape: &Shape) -> bool {
        matches!(
            shape,
            Shape::Int(_) | Shape::Float(_) | Shape::Char(_) | Shape::Bool(_)
        )
    }

    fn format_scalar(shape: &Shape) -> String {
        match shape {
            Shape::Int(i) => i.to_string(),
            Shape::Float(f) => format_float(*f),
            Shape::Char(c) => c.to_string(),
            Shape::Bool(b) => b.to_string(),
            _ => unreachable!(),
        }
    }

    // ---- Full values ----

    /// Computes a value completely. `visiting` holds the cells whose
    /// contents are being computed, to find values that contain themselves.
    fn full_value(
        &mut self,
        cell: &Lazy,
        span: Span,
        visiting: &mut Vec<(*const Cell, Option<ExprId>)>,
    ) -> Result<Value, Poisoned> {
        let ptr = Rc::as_ptr(cell);
        if visiting.len() >= MAX_VALUE_DEPTH {
            // Spreading a value into itself (`x: { ..&root }`) makes a copy
            // inside the copy, without end; each copy is new, so this is
            // found by depth rather than by identity.
            let mut diagnostic = Diagnostic::error(
                span,
                format!(
                    "this value is nested more than {MAX_VALUE_DEPTH} levels deep; does it spread a value that contains it?"
                ),
            );
            diagnostic.context = cell.location.as_ref().map(Location::describe);
            self.diagnostics.push(diagnostic);
            return Err(Poisoned);
        }
        if visiting.iter().any(|(c, _)| *c == ptr) {
            let mut diagnostic = Diagnostic::error(span, "cycle: this value contains itself");
            diagnostic.context = cell.location.as_ref().map(Location::describe);
            self.diagnostics.push(diagnostic);
            return Err(Poisoned);
        }
        let shaped = match self.force(cell, span) {
            Ok(shaped) => shaped,
            Err(_) => return Err(Poisoned),
        };
        if let Some(expr) = cell.expr
            && visiting.iter().any(|(_, e)| *e == Some(expr))
        {
            let mut diagnostic = Diagnostic::error(
                span,
                "cycle: this value contains a copy of itself, without end; does it spread a value that contains it?",
            );
            diagnostic.context = cell.location.as_ref().map(Location::describe);
            self.diagnostics.push(diagnostic);
            return Err(Poisoned);
        }
        visiting.push((ptr, cell.expr));
        let result = self.shaped_full_value(&shaped, visiting);
        visiting.pop();
        result
    }

    fn shaped_full_value(
        &mut self,
        shaped: &Shaped,
        visiting: &mut Vec<(*const Cell, Option<ExprId>)>,
    ) -> Result<Value, Poisoned> {
        let span = shaped.span;
        let kind = match &shaped.shape {
            Shape::Unit => ValueKind::Unit,
            Shape::Bool(b) => ValueKind::Bool(*b),
            Shape::Int(i) => ValueKind::Int(*i),
            Shape::Float(f) => ValueKind::Float(*f),
            Shape::Char(c) => ValueKind::Char(*c),
            Shape::Str(s) => ValueKind::String(s.to_string()),
            Shape::Id(s) => ValueKind::Id(s.to_string()),
            Shape::UnitName(n) => ValueKind::UnitName(n.to_string()),
            Shape::List(cells) => ValueKind::List(self.all_full_values(cells, span, visiting)?),
            Shape::Tuple(cells) => ValueKind::Tuple(self.all_full_values(cells, span, visiting)?),
            Shape::Named(name, cells) => ValueKind::NamedTuple(
                name.to_string(),
                self.all_full_values(cells, span, visiting)?,
            ),
            Shape::Keyed(keyed) => {
                let cells: Vec<Lazy> = keyed.slots.values().map(|s| s.value.clone()).collect();
                let values = self.all_full_values(&cells, span, visiting)?;
                ValueKind::Keyed(Keyed {
                    name: keyed.name.as_ref().map(|n| n.to_string()),
                    entries: keyed.slots.keys().cloned().zip(values).collect(),
                    defaults: keyed.defaults,
                    defaulted: keyed.defaulted.clone(),
                })
            }
        };
        Ok(Value::new(kind, span))
    }

    /// Computes every cell, so that every independent error is reported,
    /// and fails if any did.
    fn all_full_values(
        &mut self,
        cells: &[Lazy],
        span: Span,
        visiting: &mut Vec<(*const Cell, Option<ExprId>)>,
    ) -> Result<Vec<Value>, Poisoned> {
        let mut values = Vec::with_capacity(cells.len());
        let mut failed = false;
        for cell in cells {
            match self.full_value(cell, span, visiting) {
                Ok(value) => values.push(value),
                Err(_) => failed = true,
            }
        }
        if failed { Err(Poisoned) } else { Ok(values) }
    }
}
