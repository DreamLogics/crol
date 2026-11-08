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
use std::sync::Arc;

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
    /// The root key being evaluated when the cell was made (see
    /// [`Evaluator::window`]).
    born: u32,
    /// The context the cell was made for, as [`context_tag`] gives it, so
    /// that a context's cells can be freed and the base's kept.
    context: u32,
}

/// A context's index as a cell records it.
fn context_tag(context: usize) -> u32 {
    u32::try_from(context).unwrap_or(u32::MAX)
}

enum State {
    /// Boxed, so that computed cells, the most common, stay small.
    Pending(Box<Job>),
    /// A literal, the most common pending cell, which needs no environment.
    Literal(ExprId),
    Running,
    Done(Result<Rc<Shaped>, Poisoned>),
}

/// A value that failed; the failure has been reported.
#[derive(Debug, Clone, Copy)]
struct Poisoned;

/// Why evaluating an expression failed.
enum Fail {
    /// A value is missing (`SPEC.md` 6.5); `??` catches this.
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
    /// Two copies of a value at one place (`Recipe::Merge`).
    Merge {
        left: Lazy,
        right: Lazy,
        chain: Chain,
        here: Option<Location>,
        context: usize,
        span: Span,
    },
    /// A root key of the base: its chain is applied when it is read, so that
    /// what it holds can be freed and made again (`Evaluator::full_base`).
    Key(Arc<str>),
    /// A literal: see `State::Literal`.
    Literal(ExprId),
}

impl Cell {
    fn done(shaped: Rc<Shaped>) -> Lazy {
        Rc::new(Cell {
            state: RefCell::new(State::Done(Ok(shaped))),
            expr: None,
            location: None,
            born: 0,
            context: 0,
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
    Str(Arc<str>),
    Id(Arc<str>),
    List(Vec<Lazy>),
    Tuple(Vec<Lazy>),
    UnitName(Arc<str>),
    Named(Arc<str>, Vec<Lazy>),
    Keyed(Box<KeyedShape>),
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

/// A map for what the evaluator looks up all the time, where hashing is much
/// of the cost, and nobody chooses keys to attack it: the order is that of
/// insertion all the same.
type FastMap<K, V> = IndexMap<K, V, std::hash::BuildHasherDefault<rustc_hash::FxHasher>>;

#[derive(Clone, Default)]
struct KeyedShape {
    name: Option<Arc<str>>,
    slots: FastMap<Key, Slot>,
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
    /// A literal, which reads nothing, so that it needs no more than that.
    Literal(ExprId),
    /// A path entry applied to an earlier value.
    Path(Rc<PathRecipe>),
    /// A value that stays what it is wherever it is copied.
    Fixed,
    /// A value copied over another by a deep merge or a spread: where both
    /// are keyed they merge, and otherwise the right one wins.
    Merge(Rc<MergeRecipe>),
}

struct MergeRecipe {
    left: Slot,
    right: Slot,
    span: Span,
}

struct EntryRecipe {
    expr: ExprId,
    /// `&`. It stays where the entry was written when the entry is copied,
    /// unless there is nothing there (`SPEC.md` 6.1).
    below: Below,
    /// Whether the expression is a block that merges with `below`
    /// (`SPEC.md` 5.2).
    merge: bool,
    /// `^target: expr`: never merged.
    replace: bool,
    /// Value binds, which do not move either (`SPEC.md` 6.1.1).
    vbinds: VBinds,
}

struct PathRecipe {
    base: Recipe,
    base_value: Lazy,
    /// `a?.b`: the path goes through the value at its start whatever it holds.
    key_optional: bool,
    path: Vec<Selector>,
    op: Option<SetOp>,
    /// `-?target`: a remove that is no error where the target is not set.
    if_set: bool,
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
    /// Whether a block here merges with the value below.
    merge: bool,
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
    /// The block's type name, if it has one.
    name: Option<Arc<str>>,
    next: Chain,
}

#[derive(Clone)]
enum Below {
    Missing,
    Value(Lazy),
    Select(Rc<Below>, Selector),
    /// With `init_defaults`: what this is, or where that is missing, the
    /// default there (`SPEC.md` 5.1).
    Defaulted(Rc<DefaultedBelow>),
    /// Below an element of a tuple or named tuple (`SPEC.md` 6.1).
    Element(Rc<ElementBelow>),
    /// A step with `?`: what it holds, through `Some`, or where it is unset,
    /// what it starts from (`SPEC.md` 5.1).
    Optional(Rc<DefaultedBelow>),
}

struct ElementBelow {
    below: Below,
    /// The named tuple's name; `None` for a tuple.
    name: Option<Arc<str>>,
    index: usize,
}

struct DefaultedBelow {
    below: Below,
    chain: Chain,
    here: Option<Location>,
}

impl Below {
    /// Whether this can only ever be missing: nothing was below the entry
    /// where it was written.
    fn is_nothing(&self) -> bool {
        match self {
            Below::Missing => true,
            Below::Value(_) => false,
            Below::Select(inner, _) => inner.is_nothing(),
            Below::Defaulted(defaulted) | Below::Optional(defaulted) => {
                defaulted.below.is_nothing()
            }
            Below::Element(element) => element.below.is_nothing(),
        }
    }
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
    name: Arc<str>,
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

/// The value a path starts from, and whether it has a `?`.
#[derive(Clone, Copy)]
struct PathStart<'a> {
    base: &'a Lazy,
    optional: bool,
}

/// What a path goes on from, and whether it came through a `?`.
#[derive(Clone, Copy)]
struct PathInto<'a> {
    at: &'a Below,
    optional: bool,
}

/// Whether a value is an `Option`'s `None`.
fn is_none(value: &Shaped) -> bool {
    matches!(&value.shape, Shape::UnitName(name) if &**name == "None")
}

/// What a `Some(...)` holds.
fn some_payload(value: &Shaped) -> Option<&Lazy> {
    match &value.shape {
        Shape::Named(name, items) if &**name == "Some" && items.len() == 1 => Some(&items[0]),
        _ => None,
    }
}

/// The step a selector takes; `None` for a context selector.
fn selector_step(selector: &Selector) -> Option<Step> {
    match &selector.kind {
        SelectorKind::Field(name) => Some(Step::Key(Key::Str(name.clone()))),
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

/// Whether two type names allow a merge: they are equal, or one is absent.
fn names_agree(a: &Option<Arc<str>>, b: &Option<Arc<str>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    }
}

/// The field a selector names, if it names one.
pub(crate) fn selector_field(selector: &Selector) -> Option<String> {
    match &selector.kind {
        SelectorKind::Field(name) => Some(name.to_string()),
        _ => None,
    }
}

fn key_selector(key: &Key, span: Span) -> Selector {
    let kind = match key {
        Key::Str(s) => SelectorKind::Field(s.clone()),
        Key::Int(i) => SelectorKind::Accessor(*i),
    };
    Selector {
        kind,
        span,
        optional: false,
    }
}

/// An entry's target as written: `web.limits.cpu`, with its operator.
fn describe_target(entry: &Entry) -> String {
    let mut out = match entry.op {
        None if entry.if_set => "-?".to_owned(),
        None => "-".to_owned(),
        Some(SetOp::Declare) => "+".to_owned(),
        Some(SetOp::Ensure) => "?".to_owned(),
        Some(SetOp::Set) => String::new(),
    };
    out.push_str(&entry.key.to_string());
    if entry.key_optional {
        out.push('?');
    }
    for selector in &entry.path {
        out.push_str(&describe_selector(selector));
    }
    out
}

/// The error for a step of a path entry that is not set, as for a key that
/// is not set: `web.nope.x: `web.nope` is not set, so it cannot be patched`. `here` is where the step would be, and `rest` the steps after
/// it; `op` is `None` for a removal.
fn missing_step(
    selector: &Selector,
    rest: &[Selector],
    op: Option<SetOp>,
    here: Option<&Location>,
) -> String {
    let missing = here.map_or_else(
        || {
            describe_selector(selector)
                .trim_start_matches('.')
                .to_owned()
        },
        Location::describe,
    );
    let Some(op) = op else {
        return format!("-: `{missing}` is not set");
    };
    let prefix = match op {
        SetOp::Set => "",
        SetOp::Declare | SetOp::Ensure => op.symbol(),
    };
    let rest: String = rest.iter().map(describe_selector).collect();
    format!("{prefix}{missing}{rest}: `{missing}` is not set, so it cannot be patched")
}

fn describe_selector(selector: &Selector) -> String {
    let mark = if selector.optional { "?" } else { "" };
    match &selector.kind {
        SelectorKind::Field(name) if crate::value::is_identifier(name) => format!(".{name}{mark}"),
        SelectorKind::Field(name) => format!("[{}]{mark}", crate::value::quote(name)),
        SelectorKind::Element(i) => format!(".{i}{mark}"),
        SelectorKind::Accessor(i) => format!("[{i}]{mark}"),
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
    /// Whether paths start from the defaults of the fields left out.
    init_defaults: bool,
    /// Whether a block as a root key's value merges with the value below.
    merge_root: bool,
    /// Every cell that starts pending. Cells can hold each other in a
    /// cycle (a value bind never read holds its job, whose binds hold the
    /// cell; `a: &self` holds itself), so their states are cleared when
    /// the evaluator is dropped.
    cells: RefCell<Vec<Lazy>>,
    /// Whether the cells of a root key of the base are freed once its value
    /// is computed.
    evict: bool,
    /// Which root key `Evaluator::full_base` is at, counting from 1; 0 outside
    /// it. Cells made meanwhile are that key's, so they can be freed with it.
    window: u32,
    /// The cell of that root key.
    window_cell: *const Cell,
    /// The root keys of the base whose chains are applied only when they are
    /// read, as their values are known from before (`evaluate_again`).
    lazy_base: Option<&'ir std::collections::HashSet<Arc<str>>>,
    /// Whether an older cell was computed meanwhile: what it holds outlives
    /// the key, and may hold cells made meanwhile.
    window_escaped: bool,
}

impl Drop for Evaluator<'_> {
    fn drop(&mut self) {
        for cell in self.cells.get_mut().drain(..) {
            *cell.state.borrow_mut() = State::Running;
        }
    }
}

/// What the entries of a block are evaluated in.
struct BlockScope {
    chain: Chain,
    location: Option<Location>,
    outer_below: Below,
    /// Whether this block merges at all: `~^` does not, in its own entries.
    deep: bool,
    /// Whether a block as an entry's value merges with what the key holds:
    /// in a block with a start value or a spread.
    merging: bool,
    vbinds: VBinds,
    context: usize,
}

/// Where a keyed value's entries are copied to.
#[derive(Clone, Copy)]
struct CopyTo<'a> {
    chain: &'a Chain,
    location: &'a Option<Location>,
    context: usize,
    /// Whether keyed values merge with what the target holds, at any depth.
    deep: bool,
    /// What a key the target lacks has below it.
    outer_below: &'a Below,
    span: Span,
}

/// The root and templates of one context.
struct ContextState {
    context: Context,
    /// Kept alive here; locations refer to it weakly.
    root_cell: Lazy,
    root: Location,
    /// The chain of the root's entries.
    chain: Chain,
    templates: VBinds,
    /// The keys that can differ from the base, if that was found out.
    varying: Option<std::collections::HashSet<Arc<str>>>,
}

/// What a computed cell holds.
fn done_shape(cell: &Lazy) -> Option<Rc<Shaped>> {
    match &*cell.state.borrow() {
        State::Done(Ok(shaped)) => Some(shaped.clone()),
        _ => None,
    }
}

/// The keyed value in `shaped`, if it is one.
fn keyed_shape(shaped: &Option<Rc<Shaped>>) -> Option<&KeyedShape> {
    match &shaped.as_ref()?.shape {
        Shape::Keyed(keyed) => Some(keyed),
        _ => None,
    }
}

fn root_location(cell: &Lazy) -> Location {
    Location {
        base: Rc::downgrade(cell),
        anchor: Anchor::Root,
        name: Arc::from(""),
        path: None,
    }
}

/// Where `&default` finds defaults (`SPEC.md` 6.1.3): usually a schema.
pub trait DefaultsProvider: std::fmt::Debug + Send + Sync {
    /// The default of the field at `path` from the type named `type_name`
    /// (`None`: the root type), or with an empty path the type's own
    /// default; `None` if no default is recorded.
    fn default_of(&self, type_name: Option<&str>, path: &[String]) -> Option<Value>;

    /// What a field that was left out starts from when a path goes through
    /// it (`SPEC.md` 5.1): its default, or for an `Option` that is `None`
    /// by default, the default of what it holds.
    fn start_of(&self, type_name: Option<&str>, path: &[String]) -> Option<Value> {
        self.default_of(type_name, path)
    }

    /// Whether [`broken_constraints`](Self::broken_constraints) has anything
    /// to check.
    fn has_constraints(&self) -> bool {
        false
    }

    /// What is wrong with a root key's value by the constraints on its
    /// fields (`SPEC.md` 10.12): where, and why. `root` gives a root key's
    /// value in the same context.
    fn broken_constraints(
        &self,
        _key: &str,
        _value: &Value,
        _root: &dyn Fn(&str) -> Option<Value>,
    ) -> Vec<(Span, String)> {
        Vec::new()
    }

    /// Whether [`changed_sealed`](Self::changed_sealed) has anything to check.
    fn has_sealed(&self) -> bool {
        false
    }

    /// The paths of the sealed fields of a root key that differ between its
    /// value after the layer that defines it and its final value
    /// (`SPEC.md` 10.13), each as field names.
    fn changed_sealed(&self, _key: &str, _before: &Value, _after: &Value) -> Vec<Vec<String>> {
        Vec::new()
    }

    /// Whether the root type is the whole of a data-mode document, so that a
    /// root key is the first step of a path from it, rather than the type
    /// of each root value (`SPEC.md` 4.1).
    fn root_is_document(&self) -> bool {
        false
    }
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
        ValueKind::String(s) => Shape::Str(s.clone()),
        ValueKind::Id(s) => Shape::Id(s.clone()),
        ValueKind::UnitName(n) => Shape::UnitName(n.clone()),
        ValueKind::List(items) => Shape::List(cells(items)),
        ValueKind::Tuple(items) => Shape::Tuple(cells(items)),
        ValueKind::NamedTuple(n, items) => Shape::Named(n.clone(), cells(items)),
        ValueKind::Keyed(keyed) => Shape::Keyed(Box::new(KeyedShape {
            name: keyed.name.clone(),
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
        })),
    };
    Rc::new(Shaped {
        shape,
        span: value.span,
    })
}

/// The shape of a literal (`Evaluator::is_literal`).
fn literal_shape(kind: &ExprKind) -> Shape {
    match kind {
        ExprKind::Unit => Shape::Unit,
        ExprKind::Bool(b) => Shape::Bool(*b),
        ExprKind::Int(i) => Shape::Int(i.0),
        ExprKind::Float(f) => Shape::Float(*f),
        ExprKind::Char(c) => Shape::Char(*c),
        ExprKind::Str(s) => Shape::Str(s.clone()),
        ExprKind::Id(s) => Shape::Id(s.clone()),
        ExprKind::UnitName(n) => Shape::UnitName(n.clone()),
        _ => unreachable!("only literals are given"),
    }
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
        init_defaults: false,
        merge_root: false,
        cells: RefCell::new(Vec::new()),
        evict: false,
        window: 0,
        window_cell: std::ptr::null(),
        window_escaped: false,
        lazy_base: None,
    };
    let root_cell = Rc::new(Cell {
        state: RefCell::new(State::Running),
        expr: Some(root),
        location: None,
        born: 0,
        context: 0,
    });
    evaluator.cells.borrow_mut().push(root_cell.clone());
    let location = root_location(&root_cell);
    *root_cell.state.borrow_mut() = State::Pending(Box::new(Job::Expr {
        expr: root,
        env: Env {
            chain: None,
            here: Some(location.clone()),
            below: Below::Missing,
            outer_below: Below::Missing,
            merge: false,
            vbinds: None,
            context: 0,
        },
    }));
    evaluator.contexts.push(ContextState {
        context: Context::base(),
        root_cell: root_cell.clone(),
        root: location,
        chain: None,
        templates: None,
        varying: None,
    });
    let span = ir.exprs[root].span;
    let value = evaluator.full_value(&root_cell, span, &mut Vec::new()).ok();
    (value, std::mem::take(&mut evaluator.diagnostics))
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
#[derive(Clone, Copy)]
pub(crate) struct DataInput<'a> {
    pub(crate) ops: &'a [RootOp],
    pub(crate) templates: &'a [BindId],
    pub(crate) root_defaults: &'a [RootDefaults],
    /// Every `#![variant(group: name)]` of the data.
    pub(crate) declared: &'a [DeclaredVariant],
    pub(crate) root_span: Span,
    pub(crate) layer_names: &'a [String],
    /// For each layer, which of its documents are in order other than by
    /// name; empty where that is not checked.
    pub(crate) orders: &'a [crate::order::LayerOrder],
    pub(crate) variants: &'a Variants,
    /// The contexts to evaluate, by chosen variant names; `None` for every
    /// valid context.
    pub(crate) contexts: Option<&'a [Vec<String>]>,
    pub(crate) each_variant: bool,
    pub(crate) max_contexts: usize,
    pub(crate) variant_keys: VariantKeys,
    pub(crate) evaluate_every_key: bool,
    pub(crate) defaults: Option<&'a dyn DefaultsProvider>,
    pub(crate) init_defaults: bool,
    pub(crate) merge_root: bool,
    /// The base root, if it was evaluated before.
    pub(crate) known_base: Option<&'a Value>,
    /// Of the contexts to evaluate, only these, which the evaluation on
    /// threads uses to give each thread its share; `None` for all of them.
    pub(crate) only: Option<&'a [Context]>,
}

/// The evaluated data: the variants (declared, or found in the data), the
/// base root, and how every other evaluated context differs from it.
pub(crate) struct DataResult {
    pub(crate) variants: Variants,
    pub(crate) base: Option<Value>,
    pub(crate) contexts: Vec<ContextData>,
    /// Every context to evaluate, the base first, whether or not this
    /// evaluation did.
    planned: Vec<Context>,
    /// The errors of evaluation, each with the contexts it is in, in the
    /// order found; [`finish`] turns them into diagnostics.
    found: Vec<(Diagnostic, Vec<Context>)>,
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
    /// Whether the root has `..` in the context (`SPEC.md` 4). A `..` in the
    /// base holds in every context.
    pub defaults: bool,
    /// The root keys the context's chains remove, as the `defaulted` of its
    /// root (`SPEC.md` 10.3).
    pub defaulted: BTreeSet<Key>,
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
        keyed.defaults = self.defaults;
        keyed.defaulted.clone_from(&self.defaulted);
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
    data: Arc<PlanData>,
}

impl std::ops::Deref for DataPlan<'_> {
    type Target = PlanData;

    fn deref(&self) -> &PlanData {
        &self.data
    }
}

/// What a plan works out from the operations, which stays the same for as
/// long as they do: an editor looks up one key after the other, and this
/// takes much longer than a lookup.
pub(crate) struct PlanData {
    pub(crate) variants: Variants,
    /// Each operation's condition, resolved; `None` for a base operation.
    conditions: Vec<Option<Resolved>>,
    /// The condition of each `..` at a root, resolved.
    root_defaults: Vec<Option<Resolved>>,
    keys: IndexMap<Arc<str>, KeyPlan>,
    /// For each root key, the keys whose value it reads late.
    dependents: std::collections::HashMap<Arc<str>, Vec<Arc<str>>>,
}

/// What a key's operations read, found from their expressions.
#[derive(Debug, Default, Clone)]
struct Reads {
    /// Root keys read through `&root.key`, or `&self` and the like at the
    /// top level.
    keys: Vec<Arc<str>>,
    /// The whole root, or a root key that cannot be told statically.
    whole: bool,
    /// A reference into another context.
    switch: bool,
    /// A reference to a block around the one it is in, short of the root.
    /// Copied elsewhere, it reads something else (`SPEC.md` 6.1), which can
    /// be any root key.
    climbs: bool,
    /// A spread or a merge, which copies entries.
    copies: bool,
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
            RefKind::Parent(up) | RefKind::Bind { up, .. } if *up > 0 => out.climbs = true,
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
            out.copies |= !block.spreads.is_empty();
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
        ExprKind::List(items) | ExprKind::Tuple(items) => {
            for &item in items {
                collect_reads(ir, item, depth, templates, out);
            }
        }
        ExprKind::Macro(_, items) => {
            for &item in items {
                collect_reads(ir, item, depth, templates, out);
            }
        }
        ExprKind::Named(named) => {
            for &item in &named.1 {
                collect_reads(ir, item, depth, templates, out);
            }
        }
        ExprKind::Neg(operand) | ExprKind::Not(operand) => {
            collect_reads(ir, *operand, depth, templates, out)
        }
        ExprKind::Binary(op, lhs, rhs) => {
            out.copies |= matches!(op, BinOp::Merge | BinOp::MergeShallow);
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
        out.climbs |= reads.climbs;
        out.copies |= reads.copies;
    }
}

struct KeyPlan {
    /// Every operation on the key, in document order.
    all: Vec<usize>,
    /// What the key's operations read late.
    reads: Reads,
    /// Whether the key copies entries that may read what `reads` does not
    /// show: where they land decides, so that it can read any root key.
    moves_references: bool,
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
    in_order(ir, input, &plan, diagnostics)
}

/// Evaluates the contexts one after another, with the plan made and what
/// making it found.
fn in_order(
    ir: &Ir,
    input: &DataInput<'_>,
    plan: &DataPlan<'_>,
    diagnostics: Vec<Diagnostic>,
) -> (Option<DataResult>, Vec<Diagnostic>) {
    let (result, mut diagnostics) = evaluate_raw(ir, input, plan, diagnostics);
    let result = result.map(|mut result| {
        finish(&mut result, input, &mut diagnostics);
        result
    });
    (result, diagnostics)
}

/// The stack of a thread that evaluates: as much as a main thread usually
/// has, as [`MAX_DEPTH`] and [`MAX_VALUE_DEPTH`] were set against that.
const EVAL_STACK: usize = 8 << 20;

/// [`evaluate_data`], with the contexts after the base evaluated on up to
/// `threads` threads. The base is evaluated first, alone; then each thread
/// evaluates its share of the contexts from the base's value, as
/// [`crate::evaluate_contexts`] does, computing what it needs of the base
/// again. The result is the same as evaluating in order, diagnostics
/// included; the cost is the memory the threads hold at once.
pub(crate) fn evaluate_data_threaded(
    ir: &Ir,
    input: &DataInput<'_>,
    threads: usize,
) -> (Option<DataResult>, Vec<Diagnostic>) {
    // One plan for every thread: it is much of the cost of a context.
    let mut diagnostics = Vec::new();
    let Some(plan) = DataPlan::new(ir, input, &mut diagnostics) else {
        return (None, diagnostics);
    };
    // With one context or none threads have no use, and an error in the
    // list of contexts is for the evaluation in order to report. A reference
    // to another context makes contexts depend on each other (`SPEC.md`
    // 8.4), so they are evaluated together.
    let planned = match plan.evaluated_contexts(input) {
        Ok(planned) if planned.len() > 2 && !plan.keys.values().any(|k| k.reads.switch) => planned,
        _ => return in_order(ir, input, &plan, diagnostics),
    };
    // What making the plan found, for the evaluation that reports it.
    let mut plan_diagnostics = std::mem::take(&mut diagnostics);
    // The base alone, unless it is known. Its diagnostics are the static
    // ones, which every thread would find again, so theirs are dropped;
    // with a known base, the first thread's are kept instead.
    let first = match input.known_base {
        Some(_) => DataResult {
            variants: plan.variants.clone(),
            base: None,
            contexts: Vec::new(),
            planned: Vec::new(),
            found: Vec::new(),
        },
        None => {
            let base_input = DataInput {
                only: Some(&[]),
                ..*input
            };
            let reported = std::mem::take(&mut plan_diagnostics);
            let (first, found) = evaluate_raw(ir, &base_input, &plan, reported.clone());
            diagnostics = found;
            plan_diagnostics = reported;
            let Some(first) = first else {
                return (None, diagnostics);
            };
            first
        }
    };
    let base = first.base.as_ref().or(input.known_base);
    let Some(base) = base.filter(|_| first.found.is_empty()) else {
        // The base failed, so every context reports its own errors, in
        // order (`SPEC.md` 12.5): the whole evaluation is done as usual.
        return in_order(ir, input, &plan, plan_diagnostics);
    };
    let todo = &planned[1..];
    let threads = threads.clamp(1, todo.len().max(1));
    let shares: Vec<Vec<Context>> = (0..threads)
        .map(|t| todo.iter().skip(t).step_by(threads).cloned().collect())
        .collect();
    let known_base = input.known_base.is_some();
    let evaluate = |share: &[Context]| {
        let share_input = DataInput {
            known_base: Some(base),
            only: Some(share),
            ..*input
        };
        // Only the first thread reports what making the plan found, and only
        // if the base was not evaluated here, which did.
        let first_share = std::ptr::eq(share.as_ptr(), shares[0].as_ptr());
        let reported = if known_base && first_share {
            plan_diagnostics.clone()
        } else {
            Vec::new()
        };
        evaluate_raw(ir, &share_input, &plan, reported)
    };
    let mut results: Vec<Option<(Option<DataResult>, Vec<Diagnostic>)>> =
        std::thread::scope(|scope| {
            // A thread that cannot start leaves its share to this thread,
            // which takes the first share itself.
            let mut workers = Vec::with_capacity(shares.len());
            for share in &shares[1..] {
                let worker = std::thread::Builder::new()
                    .stack_size(EVAL_STACK)
                    .spawn_scoped(scope, || evaluate(share))
                    .ok();
                workers.push(worker);
            }
            let mut results = vec![Some(evaluate(&shares[0]))];
            for (worker, share) in workers.into_iter().zip(&shares[1..]) {
                results.push(match worker {
                    Some(worker) => match worker.join() {
                        Ok(result) => Some(result),
                        Err(panic) => std::panic::resume_unwind(panic),
                    },
                    None => Some(evaluate(share)),
                });
            }
            results
        });

    // Each context's data in the planned order, and the errors as the
    // evaluation in order would find them: by the first context each is
    // in, then by the order within that context.
    let position: std::collections::HashMap<&Context, usize> = planned
        .iter()
        .enumerate()
        .map(|(i, context)| (context, i))
        .collect();
    let mut contexts: Vec<Option<ContextData>> = todo.iter().map(|_| None).collect();
    let mut found: Vec<((usize, usize), Diagnostic, Vec<usize>)> = Vec::new();
    let mut merge = |result: DataResult, share: usize| {
        for data in result.contexts {
            let index = position[&data.context];
            contexts[index - 1] = Some(data);
        }
        for (n, (diagnostic, in_contexts)) in result.found.into_iter().enumerate() {
            let mut indices: Vec<usize> = in_contexts.iter().map(|c| position[c]).collect();
            match found.iter_mut().find(|(_, d, _)| *d == diagnostic) {
                Some((_, _, all)) => all.append(&mut indices),
                None => found.push(((indices[0], n + share), diagnostic, indices)),
            }
        }
    };
    let DataResult {
        variants,
        base: own_base,
        found: base_found,
        ..
    } = first;
    let base_result = DataResult {
        variants: variants.clone(),
        base: None,
        contexts: Vec::new(),
        planned: Vec::new(),
        found: base_found,
    };
    merge(base_result, 0);
    for (share, result) in results.iter_mut().enumerate() {
        let (result, mut own) = result.take().expect("every share has a result");
        if share == 0 && input.known_base.is_some() {
            diagnostics.append(&mut own);
        }
        let Some(result) = result else {
            return (None, diagnostics);
        };
        merge(result, share);
    }
    found.sort_by_key(|(key, ..)| *key);
    let found = found
        .into_iter()
        .map(|(_, diagnostic, mut indices)| {
            indices.sort_unstable();
            indices.dedup();
            let in_contexts = indices.into_iter().map(|i| planned[i].clone()).collect();
            (diagnostic, in_contexts)
        })
        .collect();
    let mut result = DataResult {
        variants,
        base: own_base,
        contexts: contexts.into_iter().flatten().collect(),
        planned,
        found,
    };
    finish(&mut result, input, &mut diagnostics);
    (Some(result), diagnostics)
}

/// What comes after evaluating: the check of each context's keys against
/// the base (`SPEC.md` 8.5), and the errors found, each saying which
/// contexts it is in where that is not all of them (`SPEC.md` 12.5).
fn finish(result: &mut DataResult, input: &DataInput<'_>, diagnostics: &mut Vec<Diagnostic>) {
    let empty_keyed = Keyed::default();
    let base = result.base.as_ref().or(input.known_base);
    let base_keyed = match base.map(|v| &v.kind) {
        Some(ValueKind::Keyed(keyed)) => keyed,
        _ => &empty_keyed,
    };
    check_variant_keys(
        base_keyed,
        &result.contexts,
        input.variant_keys,
        &result.variants,
        diagnostics,
    );
    for (mut diagnostic, in_contexts) in std::mem::take(&mut result.found) {
        if in_contexts.len() < result.planned.len() {
            let mut names: Vec<String> = in_contexts
                .iter()
                .take(3)
                .map(|context| result.variants.display(context).to_string())
                .collect();
            if in_contexts.len() > 3 {
                names.push(format!("and {} more", in_contexts.len() - 3));
            }
            diagnostic.message = format!("in {}: {}", names.join(", "), diagnostic.message);
        }
        diagnostics.push(diagnostic);
    }
    diagnostics.sort_by_key(|d| (d.span.file, d.span.range.start()));
}

/// [`evaluate_data`] up to, not including, [`finish`].
fn evaluate_raw(
    ir: &Ir,
    input: &DataInput<'_>,
    plan: &DataPlan<'_>,
    mut diagnostics: Vec<Diagnostic>,
) -> (Option<DataResult>, Vec<Diagnostic>) {
    let planned = match plan.evaluated_contexts(input) {
        Ok(contexts) => contexts,
        Err(message) => {
            let span = input.ops.first().map_or(Span::detached(), |op| op.span);
            diagnostics.push(Diagnostic::error(span, message));
            return (None, diagnostics);
        }
    };
    let contexts: Vec<&Context> = match input.only {
        Some(only) => only.iter().collect(),
        None => planned[1..].iter().collect(),
    };
    let mut evaluator = Evaluator {
        ir,
        diagnostics: Vec::new(),
        stack: Vec::new(),
        steps: 0,
        contexts: Vec::new(),
        plan: Some(plan),
        share: false,
        base_created: Vec::new(),
        defaults: input.defaults,
        init_defaults: input.init_defaults,
        merge_root: input.merge_root,
        cells: RefCell::new(Vec::new()),
        evict: false,
        window: 0,
        window_cell: std::ptr::null(),
        window_escaped: false,
        lazy_base: None,
    };
    // Errors in the static structure of the chains are reported once.
    evaluator.check_chains(input, &planned, None);
    diagnostics.append(&mut evaluator.diagnostics);
    if diagnostics.iter().any(Diagnostic::is_error) {
        diagnostics.sort_by_key(|d| (d.span.file, d.span.range.start()));
        return (None, diagnostics);
    }

    let mut found: Vec<(Diagnostic, Vec<Context>)> = Vec::new();
    let mut record = |evaluator: &mut Evaluator<'_>, context: &Context| {
        for diagnostic in evaluator.diagnostics.drain(..) {
            match found.iter_mut().find(|(d, _)| *d == diagnostic) {
                Some((_, in_contexts)) => in_contexts.push(context.clone()),
                None => found.push((diagnostic, vec![context.clone()])),
            }
        }
    };

    // What a context evaluates is not needed once it is done, unless a
    // reference reads another context, which may be in a cell of the base.
    let forget = !plan.keys.values().any(|key| key.reads.switch);
    // Nor is what a root key of the base holds, unless something reads it.
    evaluator.evict =
        input.known_base.is_none() && forget && !plan.keys.values().any(|key| key.reads.whole);

    // The base, in full, unless it is known: then what a context needs of it
    // is computed when it does (`SPEC.md` 8.1).
    let base_id = evaluator.context_state(&planned[0]);
    let base_cell = evaluator.contexts[base_id].root_cell.clone();
    let base = match input.known_base {
        Some(known) => Some(std::borrow::Cow::Borrowed(known)),
        None => evaluator.full_base(&base_cell).ok().map(|mut base| {
            // The root is written nowhere; what is wrong with it as a
            // whole needs a place all the same.
            base.span = input.root_span;
            std::borrow::Cow::Owned(base)
        }),
    };
    let base_failed = base.is_none() || !evaluator.diagnostics.is_empty();
    evaluator.share = !base_failed && !input.evaluate_every_key;
    record(&mut evaluator, &planned[0]);
    let empty_keyed = Keyed::default();
    let base_keyed = match base.as_ref().map(|v| &v.kind) {
        Some(ValueKind::Keyed(keyed)) => keyed,
        _ => &empty_keyed,
    };

    let mut per_context = Vec::with_capacity(contexts.len());
    for context in contexts {
        evaluator.steps = 0;
        let cells_before = evaluator.cells.borrow().len();
        let contexts_before = evaluator.contexts.len();
        let id = evaluator.context_state(context);
        let root_cell = evaluator.contexts[id].root_cell.clone();
        // Only keys that can differ are evaluated again, unless the base
        // failed: then every context's own errors are wanted.
        let varying = evaluator.share.then(|| {
            let known = evaluator.contexts[id].varying.take();
            known.unwrap_or_else(|| plan.varying(context))
        });
        let shaped = done_shape(&root_cell);
        let no_keys = KeyedShape::default();
        let slots = keyed_shape(&shaped).unwrap_or(&no_keys);
        let (defaults, defaulted) = (slots.defaults, slots.defaulted.clone());
        let slots = &slots.slots;
        // A key that cannot differ has the base's slot, so that only the keys
        // that can differ can be new, changed or gone. Each is evaluated in
        // root order.
        let mut evaluated: Vec<usize> = match &varying {
            Some(varying) => varying
                .iter()
                .filter_map(|key| slots.get_index_of(&KeyRef::Str(key)))
                .collect(),
            None => (0..slots.len()).collect(),
        };
        evaluated.sort_unstable();
        // The differences from base keys, by their place there.
        let mut in_base = Vec::new();
        let mut own = Vec::new();
        let mut failed = false;
        let mut visiting = vec![(Rc::as_ptr(&root_cell), None)];
        for (key, slot) in evaluated.into_iter().filter_map(|i| slots.get_index(i)) {
            let base_value = base_keyed.entries.get_full(key);
            match evaluator.full_value(&slot.value, Span::detached(), &mut visiting) {
                Ok(value) => match base_value {
                    Some((_, _, base)) if base.identical(&value) => {}
                    Some((position, ..)) => in_base.push((position, key.clone(), Some(value))),
                    None => own.push((key.clone(), Some(value))),
                },
                Err(_) => failed = true,
            }
        }
        let mut removed = |position: usize, key: &Key| {
            if !slots.contains_key(key) {
                in_base.push((position, key.clone(), None));
            }
        };
        match &varying {
            Some(varying) => {
                for key in varying {
                    if let Some((position, key, _)) = base_keyed.entries.get_full(&KeyRef::Str(key))
                    {
                        removed(position, key);
                    }
                }
            }
            None => {
                for (position, key) in base_keyed.entries.keys().enumerate() {
                    removed(position, key);
                }
            }
        }
        // Keys in root order, and the context's own keys after them.
        in_base.sort_unstable_by_key(|(position, ..)| *position);
        let ordered: IndexMap<Key, Option<Value>> = in_base
            .into_iter()
            .map(|(_, key, change)| (key, change))
            .chain(own)
            .collect();
        record(&mut evaluator, context);
        if forget {
            evaluator.forget(cells_before, contexts_before, id);
        }
        if !failed {
            per_context.push(ContextData {
                context: context.clone(),
                changes: ordered,
                defaults,
                defaulted,
            });
        }
    }
    let variants = plan.variants.clone();
    (
        Some(DataResult {
            variants,
            base: base.and_then(|base| match base {
                std::borrow::Cow::Owned(base) => Some(base),
                std::borrow::Cow::Borrowed(_) => None,
            }),
            contexts: per_context,
            planned,
            found,
        }),
        diagnostics,
    )
}

/// What evaluating again gives, where only some keys can have changed
/// since the evaluation before.
pub(crate) struct Again {
    /// The base root's keys, in root order.
    pub(crate) root: Vec<Key>,
    /// The base values of the keys evaluated again, of those that exist.
    pub(crate) values: std::collections::HashMap<Key, Value>,
    /// Whether the base root has `..`, and the keys its chains remove.
    pub(crate) defaults: bool,
    pub(crate) defaulted: BTreeSet<Key>,
    /// For each context of the evaluation before, in order, its data where
    /// a key evaluated again can differ in it from the base; for the others,
    /// their data before holds, without the keys evaluated again.
    pub(crate) contexts: Vec<Option<ContextData>>,
    /// The keys evaluated again.
    pub(crate) affected: std::collections::HashSet<Arc<str>>,
}

/// Evaluates again where only the keys of `dirty` have operations that
/// changed since an evaluation in which `before` are the contexts after the
/// base and `variants` the variants. The keys that cannot change keep their
/// values: those of the base are computed only where an evaluated key reads
/// them, and a context is evaluated only if an evaluated key can differ in
/// it. `None` where this cannot be done, or anything is to be reported:
/// then the whole evaluation is done again.
pub(crate) fn evaluate_again(
    ir: &Ir,
    input: &DataInput<'_>,
    dirty: &std::collections::HashSet<Arc<str>>,
    variants: &Variants,
    before: &[ContextData],
) -> Option<Again> {
    if input.evaluate_every_key || input.known_base.is_some() || input.only.is_some() {
        return None;
    }
    let mut diagnostics = Vec::new();
    let plan = DataPlan::new(ir, input, &mut diagnostics)?;
    // A reference to another context makes contexts depend on each other.
    if !diagnostics.is_empty()
        || plan.variants != *variants
        || plan.keys.values().any(|key| key.reads.switch)
    {
        return None;
    }
    let planned = plan.evaluated_contexts(input).ok()?;
    if planned.len() != before.len() + 1
        || planned[1..]
            .iter()
            .zip(before)
            .any(|(context, data)| *context != data.context)
    {
        return None;
    }
    let affected = plan.affected(dirty);
    let lazy: std::collections::HashSet<Arc<str>> = plan
        .keys
        .keys()
        .filter(|key| !affected.contains(*key))
        .cloned()
        .collect();
    let mut evaluator = Evaluator {
        ir,
        diagnostics: Vec::new(),
        stack: Vec::new(),
        steps: 0,
        contexts: Vec::new(),
        plan: Some(&plan),
        share: true,
        base_created: Vec::new(),
        defaults: input.defaults,
        init_defaults: input.init_defaults,
        merge_root: input.merge_root,
        cells: RefCell::new(Vec::new()),
        evict: false,
        window: 0,
        window_cell: std::ptr::null(),
        window_escaped: false,
        lazy_base: Some(&lazy),
    };
    // The chains of the other keys are as they were, and had no errors.
    evaluator.check_chains(input, &planned, Some(dirty));
    if !evaluator.diagnostics.is_empty() {
        return None;
    }

    let base_id = evaluator.context_state(&planned[0]);
    let base_cell = evaluator.contexts[base_id].root_cell.clone();
    let base_shaped = done_shape(&base_cell);
    let base = keyed_shape(&base_shaped)?;
    let mut visiting = vec![(Rc::as_ptr(&base_cell), None)];
    let mut values = std::collections::HashMap::new();
    for (key, slot) in &base.slots {
        let Key::Str(name) = key else { continue };
        if affected.contains(name) {
            let value = evaluator
                .full_value(&slot.value, Span::detached(), &mut visiting)
                .ok()?;
            values.insert(key.clone(), value);
        }
    }
    if !evaluator.diagnostics.is_empty() {
        return None;
    }
    let root: Vec<Key> = base.slots.keys().cloned().collect();
    let position: std::collections::HashMap<&Key, usize> =
        root.iter().enumerate().map(|(i, key)| (key, i)).collect();

    let mut contexts = Vec::with_capacity(before.len());
    for data in before {
        let varying = plan.varying(&data.context);
        let mut needed: Vec<&Arc<str>> = affected
            .iter()
            .filter(|key| varying.contains(*key))
            .collect();
        if needed.is_empty() {
            contexts.push(None);
            continue;
        }
        needed.sort();
        evaluator.steps = 0;
        let cells_before = evaluator.cells.borrow().len();
        let contexts_before = evaluator.contexts.len();
        let id = evaluator.context_state(&data.context);
        let root_cell = evaluator.contexts[id].root_cell.clone();
        let shaped = done_shape(&root_cell);
        let slots = keyed_shape(&shaped)?;
        let mut visiting = vec![(Rc::as_ptr(&root_cell), None)];
        let mut fresh = Vec::new();
        for name in needed {
            let key = Key::Str(name.clone());
            let in_base = values.get(&key);
            match slots.slots.get(&KeyRef::Str(name)) {
                Some(slot) => {
                    let value = evaluator
                        .full_value(&slot.value, Span::detached(), &mut visiting)
                        .ok()?;
                    if in_base.is_none_or(|base| !base.identical(&value)) {
                        fresh.push((key, Some(value)));
                    }
                }
                None if in_base.is_some() => fresh.push((key, None)),
                None => {}
            }
        }
        if !evaluator.diagnostics.is_empty() {
            return None;
        }
        // Keys in root order, and the context's own keys after them, in its
        // order, as `evaluate_raw` has them.
        let kept = data.changes.iter().filter(|(key, _)| match key {
            Key::Str(name) => !affected.contains(name),
            Key::Int(_) => true,
        });
        let mut in_base = Vec::new();
        let mut own = Vec::new();
        for (key, change) in kept.map(|(k, c)| (k.clone(), c.clone())).chain(fresh) {
            match position.get(&key) {
                Some(&at) => in_base.push((at, key, change)),
                None => {
                    let at = slots.slots.get_index_of(&key).unwrap_or(usize::MAX);
                    own.push((at, key, change));
                }
            }
        }
        in_base.sort_unstable_by_key(|(at, ..)| *at);
        own.sort_by_key(|(at, ..)| *at);
        let changes = in_base
            .into_iter()
            .chain(own)
            .map(|(_, key, change)| (key, change))
            .collect();
        contexts.push(Some(ContextData {
            context: data.context.clone(),
            changes,
            defaults: slots.defaults,
            defaulted: slots.defaulted.clone(),
        }));
        evaluator.forget(cells_before, contexts_before, id);
    }
    Some(Again {
        root,
        values,
        defaults: base.defaults,
        defaulted: base.defaulted.clone(),
        contexts,
        affected,
    })
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
    /// Whether the operation uses the value below it (`SPEC.md` 7.2). One
    /// that does not decides the value, whatever came before it.
    pub uses_below: bool,
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
    let data = plan_data(ir, input)?;
    explain_planned(ir, input, &data, requests, context)
}

/// What [`explain_planned`] needs of the operations.
pub(crate) fn plan_data(ir: &Ir, input: &DataInput<'_>) -> Result<Arc<PlanData>, Vec<Diagnostic>> {
    let mut diagnostics = Vec::new();
    match DataPlan::new(ir, input, &mut diagnostics) {
        Some(plan) => Ok(plan.data),
        None => Err(diagnostics),
    }
}

/// [`explain`] with what was worked out of the same operations before.
pub(crate) fn explain_planned(
    ir: &Ir,
    input: &DataInput<'_>,
    data: &Arc<PlanData>,
    requests: &[(&str, &[PathStep])],
    context: &[String],
) -> Result<Vec<Vec<ExplainStep>>, Vec<Diagnostic>> {
    let plan = DataPlan {
        ops: input.ops,
        templates: input.templates,
        data: Arc::clone(data),
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
        init_defaults: input.init_defaults,
        merge_root: input.merge_root,
        cells: RefCell::new(Vec::new()),
        evict: false,
        window: 0,
        window_cell: std::ptr::null(),
        window_escaped: false,
        lazy_base: None,
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
    let Some((key, key_plan)) = plan.keys.get_key_value(key) else {
        return Vec::new();
    };
    let order = plan.chain(key_plan, context);
    let root = evaluator.contexts[id].root.clone();
    let templates = evaluator.contexts[id].templates.clone();
    let chain = Some(Rc::new(ChainLink {
        location: Some(root.clone()),
        bind_root: false,
        name: None,
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
            None if op.if_set => "-?".to_owned(),
            None => "-".to_owned(),
            Some(SetOp::Declare) => "+".to_owned(),
            Some(SetOp::Ensure) => "?".to_owned(),
            Some(SetOp::Set) => String::new(),
        };
        entry.push_str(&Key::Str(key.clone()).to_string());
        if op.key_optional {
            entry.push('?');
        }
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
            uses_below: op.uses_below,
        });
    }
    steps
}

impl<'a> DataPlan<'a> {
    /// Resolves conditions against the variants, adding those the data
    /// declares to open groups, or declaring the names the data uses if the
    /// application declared none (`SPEC.md` 8.1), and
    /// groups operations by key.
    fn new(ir: &Ir, input: &DataInput<'a>, diagnostics: &mut Vec<Diagnostic>) -> Option<Self> {
        let mut names = Vec::new();
        for op in input.ops {
            if let Some(condition) = &op.condition {
                condition.names(&mut names);
            }
        }
        for defaults in input.root_defaults {
            if let Some(condition) = &defaults.condition {
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
        for declared in input.declared {
            if let Err(message) = variants.add_to_open_group(&declared.group, &declared.name) {
                diagnostics.push(Diagnostic::error(declared.span, message));
            }
        }
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
        let root_defaults: Vec<Option<Resolved>> = input
            .root_defaults
            .iter()
            .map(|d| d.condition.as_ref().and_then(|c| c.resolve(&variants)))
            .collect();
        // A condition that cannot hold is a mistake (`SPEC.md` 8.2); checked
        // where the contexts are few enough to try.
        if variants.context_count() <= 1 << 16 {
            let all = variants.all_contexts();
            let mut reported = std::collections::HashSet::new();
            let spans = input.ops.iter().map(|op| op.span);
            let root_spans = input.root_defaults.iter().map(|d| d.span);
            for (span, condition) in spans
                .chain(root_spans)
                .zip(conditions.iter().chain(&root_defaults))
            {
                if let Some(condition) = condition
                    && !all.iter().any(|c| condition.holds(c))
                    && reported.insert(condition.normalized())
                {
                    diagnostics.push(Diagnostic::error(
                        span,
                        "this condition cannot hold in any context",
                    ));
                }
            }
        }
        let mut keys: IndexMap<Arc<str>, KeyPlan> = IndexMap::new();
        let mut indices: IndexMap<Arc<str>, Vec<usize>> = IndexMap::new();
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
        let mut dependents: std::collections::HashMap<Arc<str>, Vec<Arc<str>>> = Default::default();
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
                    moves_references: false,
                    base: ordered(base),
                    conditional: ordered(conditional),
                },
            );
        }
        // What a key copies is in itself or in what it reads. Where it went
        // through another key first, that key is here, and this one reads it.
        let moving: Vec<bool> = keys
            .values()
            .map(|plan| {
                let reads = &plan.reads;
                let climbing = |key: &Arc<str>| keys.get(key).is_some_and(|k| k.reads.climbs);
                reads.copies && (reads.climbs || reads.keys.iter().any(climbing))
            })
            .collect();
        for (plan, moving) in keys.values_mut().zip(moving) {
            plan.moves_references = moving;
        }
        Some(Self {
            ops: input.ops,
            templates: input.templates,
            data: Arc::new(PlanData {
                variants,
                conditions,
                root_defaults,
                keys,
                dependents,
            }),
        })
    }

    /// Whether the root has `..` in `context`: the flag ORs, as in a block
    /// (`SPEC.md` 10.3), and nothing replaces the root.
    fn root_has_defaults(&self, context: &Context) -> bool {
        self.root_defaults
            .iter()
            .any(|condition| condition.as_ref().is_none_or(|c| c.holds(context)))
    }

    /// The keys whose value can differ from the base in `context`: those
    /// with an operation whose condition holds there or in the base, as
    /// `x<!b>` does, and those that read them, directly or not. If any key
    /// can differ, so can those that read the whole root, and those that
    /// copy references, which may read it. Every other key has its base value.
    fn varying(&self, context: &Context) -> std::collections::HashSet<Arc<str>> {
        let mut varying = std::collections::HashSet::new();
        let mut queue = Vec::new();
        let base = Context::base();
        for (key, plan) in &self.keys {
            let conditional = plan.conditional.iter().any(|&i| {
                self.conditions[i]
                    .as_ref()
                    .is_some_and(|c| c.holds(context) || c.holds(&base))
            });
            if conditional || plan.reads.switch {
                varying.insert(key.clone());
                queue.push(key.clone());
            }
        }
        if !varying.is_empty() {
            for (key, plan) in &self.keys {
                if (plan.reads.whole || plan.moves_references) && varying.insert(key.clone()) {
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

    /// The keys whose values can change when those of `dirty` have operations
    /// that changed: those, the keys that read them, directly or not, and
    /// if any can change, those that read the whole root or copy references,
    /// as [`DataPlan::varying`] has it.
    fn affected(
        &self,
        dirty: &std::collections::HashSet<Arc<str>>,
    ) -> std::collections::HashSet<Arc<str>> {
        let mut affected = dirty.clone();
        let mut queue: Vec<Arc<str>> = dirty.iter().cloned().collect();
        if !dirty.is_empty() {
            for (key, plan) in &self.keys {
                if (plan.reads.whole || plan.moves_references) && affected.insert(key.clone()) {
                    queue.push(key.clone());
                }
            }
        }
        while let Some(key) = queue.pop() {
            for dependent in self.dependents.get(&key).into_iter().flatten() {
                if affected.insert(dependent.clone()) {
                    queue.push(dependent.clone());
                }
            }
        }
        affected
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
            None if input.each_variant => {
                for id in self.variants.ids() {
                    let context = self.variants.context(&[id])?;
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

    /// Two of a layer's `patches` to one key whose order only the names of
    /// their documents give (`SPEC.md` 7.2): from documents that nothing else
    /// puts in order (7.1), of one rank, as base operations or by
    /// specificity, and with conditions that hold together in a context of
    /// `contexts`. `meets` keeps what is known of pairs of conditions.
    fn unordered(
        &self,
        patches: &[usize],
        order: Option<&crate::order::LayerOrder>,
        contexts: &mut dyn FnMut() -> std::rc::Rc<Vec<Context>>,
        meets: &mut std::collections::HashMap<(String, String), bool>,
    ) -> Option<(usize, usize)> {
        for (n, &first) in patches.iter().enumerate() {
            for &second in &patches[n + 1..] {
                let (a, b) = (&self.ops[first], &self.ops[second]);
                if a.span.file == b.span.file
                    || order.is_some_and(|order| order.ordered(a.span.file, b.span.file))
                {
                    continue;
                }
                let (ca, cb) = (&self.conditions[first], &self.conditions[second]);
                let rank = |c: &Option<Resolved>| c.as_ref().map(|c| c.specificity(&self.variants));
                if rank(ca) != rank(cb) {
                    continue;
                }
                let name =
                    |c: &Option<Resolved>| c.as_ref().map(Resolved::normalized).unwrap_or_default();
                let pair = (name(ca), name(cb));
                let met = match meets.get(&pair) {
                    Some(&met) => met,
                    None => {
                        let holds = |c: &Option<Resolved>, context: &Context| {
                            c.as_ref().is_none_or(|c| c.holds(context))
                        };
                        let met = contexts()
                            .iter()
                            .any(|context| holds(ca, context) && holds(cb, context));
                        meets.insert(pair, met);
                        met
                    }
                };
                if met {
                    return Some((first, second));
                }
            }
        }
        None
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

/// What applying a chain of operations without errors gives, worked out
/// without applying it: the operation that creates the key, and the
/// expression its last value is written as, if it is one; `None` if the
/// chain leaves the key unset. It follows `Evaluator::apply_chain`.
fn chain_outcome(ops: &[RootOp], order: &[usize]) -> Option<(usize, Option<ExprId>)> {
    let mut value: Option<(usize, Option<ExprId>)> = None;
    for &i in order {
        let op = &ops[i];
        let created = value.map_or(i, |(created, _)| created);
        if !op.path.is_empty() {
            value = Some((created, None));
            continue;
        }
        match op.op {
            None => value = None,
            Some(SetOp::Declare | SetOp::Ensure) if value.is_some() => {}
            Some(_) => value = Some((created, op.value)),
        }
    }
    value
}

impl Evaluator<'_> {
    /// Drops the cells of `context` and the contexts made since these
    /// counts, breaking what cycles they hold, as dropping the evaluator
    /// does. Cells of other contexts made meanwhile, the base's as a context
    /// reads a key of it, are kept: the base's values hold them.
    fn forget(&mut self, cells: usize, contexts: usize, context: usize) {
        let tag = context_tag(context);
        let all = self.cells.get_mut();
        let mut kept = cells;
        for i in cells..all.len() {
            if all[i].context == tag {
                *all[i].state.borrow_mut() = State::Running;
            } else {
                all.swap(kept, i);
                kept += 1;
            }
        }
        all.truncate(kept);
        self.contexts.truncate(contexts);
    }

    /// A pending cell, which the evaluator keeps (see `cells`).
    fn cell(&self, job: Job, location: Option<Location>) -> Lazy {
        let expr = match &job {
            Job::Expr { expr, .. } => Some(*expr),
            Job::Literal(expr) => Some(*expr),
            Job::Path { .. } | Job::Merge { .. } | Job::Key(_) => None,
        };
        let context = match &job {
            Job::Expr { env, .. } => env.context,
            Job::Path { context, .. } | Job::Merge { context, .. } => *context,
            Job::Key(_) | Job::Literal(_) => 0,
        };
        let cell = Rc::new(Cell {
            state: RefCell::new(State::Pending(Box::new(job))),
            expr,
            location,
            born: self.window,
            context: context_tag(context),
        });
        self.cells.borrow_mut().push(cell.clone());
        cell
    }

    /// A cell of a literal. It holds nothing, so that the evaluator need not
    /// keep it.
    fn literal_cell(&self, expr: ExprId, location: Option<Location>) -> Lazy {
        Rc::new(Cell {
            state: RefCell::new(State::Literal(expr)),
            expr: Some(expr),
            location,
            born: self.window,
            context: 0,
        })
    }

    /// Whether an expression is a literal.
    fn is_literal(&self, expr: ExprId) -> bool {
        matches!(
            self.ir.exprs[expr].kind,
            ExprKind::Unit
                | ExprKind::Bool(_)
                | ExprKind::Int(_)
                | ExprKind::Float(_)
                | ExprKind::Char(_)
                | ExprKind::Str(_)
                | ExprKind::Id(_)
                | ExprKind::UnitName(_)
        )
    }

    /// Reports the errors in the chains that do not depend on the context:
    /// two definitions under one condition in one layer, and patches from
    /// several documents of one layer (`SPEC.md` 7.2, 8.3).
    fn check_chains(
        &mut self,
        input: &DataInput<'_>,
        contexts: &[Context],
        only: Option<&std::collections::HashSet<Arc<str>>>,
    ) {
        let plan = self.plan.expect("data mode");
        let ops = plan.ops;
        // Patches meet if their conditions hold together in any context, or
        // where the contexts are too many to try, in an evaluated one.
        let mut known: Option<std::rc::Rc<Vec<Context>>> = None;
        let mut every_context = || {
            known
                .get_or_insert_with(|| {
                    std::rc::Rc::new(if plan.variants.context_count() <= 1 << 16 {
                        plan.variants.all_contexts()
                    } else {
                        contexts.to_vec()
                    })
                })
                .clone()
        };
        let mut meets = std::collections::HashMap::new();
        for (key, key_plan) in &plan.keys {
            if only.is_some_and(|only| !only.contains(key)) {
                continue;
            }
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
                let patches: Vec<usize> = in_layer
                    .iter()
                    .copied()
                    .filter(|&i| !ops[i].is_definition())
                    .collect();
                let order = input.orders.get(layer);
                if let Some((first, second)) =
                    plan.unordered(&patches, order, &mut every_context, &mut meets)
                {
                    let name = |i: usize| {
                        order
                            .and_then(|order| order.path(ops[i].span.file))
                            .unwrap_or_default()
                    };
                    let diagnostic = Diagnostic::error(
                        ops[second].span,
                        format!(
                            "`{key}` is patched from `{}` and `{}` of the layer `{layer_name}`, and only their names put the patches in order; put one in a folder below the other's, or name the other in its `#![extends(...)]`",
                            name(first),
                            name(second),
                        ),
                    )
                    .with_related(ops[first].span, "the other patch");
                    self.report(diagnostic);
                }
            }
        }
    }
}

/// Checks the keys of each context against the base (`SPEC.md` 8.5).
fn check_variant_keys(
    base: &Keyed,
    contexts: &[ContextData],
    rule: VariantKeys,
    variants: &Variants,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if rule == VariantKeys::Any {
        return;
    }
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

impl Evaluator<'_> {
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
            born: 0,
            context: context_tag(id),
        });
        self.cells.borrow_mut().push(root_cell.clone());
        let root = root_location(&root_cell);
        // Root entries are in the root, so `&self` there is the root.
        let chain = Some(Rc::new(ChainLink {
            location: Some(root.clone()),
            bind_root: false,
            name: None,
            next: None,
        }));
        self.contexts.push(ContextState {
            context: context.clone(),
            root_cell: root_cell.clone(),
            root: root.clone(),
            chain: chain.clone(),
            templates: None,
            varying: None,
        });
        let plan = self.plan.expect("only data mode has more than one context");
        let templates = self.bind_cells(plan.templates, &chain, None, id, true);
        self.contexts[id].templates = templates.clone();

        // A key that cannot differ from the base shares the base's cell.
        let shared = (self.share && id > 0).then(|| {
            (
                plan.varying(context),
                done_shape(&self.contexts[0].root_cell),
            )
        });
        let no_keys = KeyedShape::default();
        let mut slots = Vec::new();
        // The keys whose chain ends in a remove (`SPEC.md` 10.3).
        let mut defaulted = BTreeSet::new();
        for (key, key_plan) in &plan.keys {
            if let Some((varying, base)) = &shared
                && !varying.contains(key)
            {
                let base = keyed_shape(base).unwrap_or(&no_keys);
                if let Some((position, key, slot)) = base.slots.get_full(&KeyRef::Str(key)) {
                    slots.push((self.base_created[position], key.clone(), slot.clone()));
                } else {
                    let key = Key::Str(key.clone());
                    if base.defaulted.contains(&key) {
                        defaulted.insert(key);
                    }
                }
                continue;
            }
            let order = plan.chain(key_plan, context);
            if id == 0
                && let Some(lazy) = self.lazy_base
                && lazy.contains(key)
            {
                match chain_outcome(plan.ops, &order) {
                    Some((created, expr)) => {
                        let slot = Slot {
                            value: self.lazy_key(key, expr, &root),
                            recipe: Recipe::Fixed,
                        };
                        slots.push((created, Key::Str(key.clone()), slot));
                    }
                    None if !order.is_empty() => {
                        defaulted.insert(Key::Str(key.clone()));
                    }
                    None => {}
                }
                continue;
            }
            let cells_before = self.cells.borrow().len();
            let diagnostics_before = self.diagnostics.len();
            if let Some((created, mut slot)) =
                self.apply_chain(key, &order, &chain, &root, &templates, id)
            {
                // Without errors, the chain can be applied again when the
                // key is read, and the cells made here are not needed.
                if id == 0 && self.evict && self.diagnostics.len() == diagnostics_before {
                    slot.value = self.key_cell(key, &slot.value, cells_before);
                }
                slots.push((created, Key::Str(key.clone()), slot));
            } else if !order.is_empty() {
                defaulted.insert(Key::Str(key.clone()));
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
            defaults: plan.root_has_defaults(context),
            defaulted,
            ..KeyedShape::default()
        };
        *root_cell.state.borrow_mut() = State::Done(Ok(Rc::new(Shaped {
            shape: Shape::Keyed(Box::new(keyed)),
            span: Span::detached(),
        })));
        self.contexts[id].varying = shared.map(|(varying, _)| varying);
        id
    }

    /// A cell that applies the chain of a root key of the base when it is
    /// read, whose last value is `expr`'s if it is an expression's.
    fn lazy_key(&mut self, key: &Arc<str>, expr: Option<ExprId>, root: &Location) -> Lazy {
        let cell = Rc::new(Cell {
            state: RefCell::new(State::Pending(Box::new(Job::Key(key.clone())))),
            expr,
            location: Some(root.child(Step::Key(Key::Str(key.clone())))),
            born: 0,
            context: 0,
        });
        self.cells.get_mut().push(cell.clone());
        cell
    }

    /// A cell that applies the chain of a root key of the base when it is
    /// read, in place of `cell`, the end of that chain, whose cells are those
    /// since `cells_before`.
    fn key_cell(&mut self, key: &Arc<str>, cell: &Lazy, cells_before: usize) -> Lazy {
        let keyed = Rc::new(Cell {
            state: RefCell::new(State::Pending(Box::new(Job::Key(key.clone())))),
            expr: cell.expr,
            location: cell.location.clone(),
            born: 0,
            context: 0,
        });
        let cells = self.cells.get_mut();
        for cell in cells.drain(cells_before..) {
            *cell.state.borrow_mut() = State::Running;
        }
        cells.push(keyed.clone());
        keyed
    }

    /// Applies the chain of a root key of the base, and runs its last
    /// operation here, as the job of the key's cell.
    fn run_key(&mut self, key: &Arc<str>) -> Eval<Rc<Shaped>> {
        let plan = self.plan.expect("only data mode has root keys");
        let base = &self.contexts[0];
        let order = plan.chain(&plan.keys[&**key], &base.context);
        let (root, chain, templates) = (
            base.root.clone(),
            base.chain.clone(),
            base.templates.clone(),
        );
        let (_, slot) = self
            .apply_chain(key, &order, &chain, &root, &templates, 0)
            .expect("a key that exists had a chain without errors");
        let state = std::mem::replace(&mut *slot.value.state.borrow_mut(), State::Running);
        let State::Pending(job) = state else {
            unreachable!("a chain ends in a new cell")
        };
        self.run(*job)
    }

    /// The base root in full. Keys are computed in order, and the cells of a
    /// key that nothing reads are freed as soon as its value is computed, so
    /// that the cells of every key are not alive at once. That is only a
    /// matter of memory: a key read after all is computed again.
    fn full_base(&mut self, root: &Lazy) -> Result<Value, Poisoned> {
        if !self.evict {
            return self.full_value(root, Span::detached(), &mut Vec::new());
        }
        let shaped = self.force(root, Span::detached()).map_err(|_| Poisoned)?;
        let Shape::Keyed(keyed) = &shaped.shape else {
            unreachable!("a data-mode root is keyed")
        };
        let dependents = &self.plan.expect("data mode").dependents;
        let mut visiting = vec![(Rc::as_ptr(root), root.expr)];
        let mut values = Vec::with_capacity(keyed.slots.len());
        let mut failed = false;
        for (key, slot) in &keyed.slots {
            let cells_before = self.cells.borrow().len();
            let diagnostics_before = self.diagnostics.len();
            self.window += 1;
            self.window_cell = Rc::as_ptr(&slot.value);
            self.window_escaped = false;
            match self.full_value(&slot.value, shaped.span, &mut visiting) {
                Ok(value) => values.push(value),
                Err(_) => failed = true,
            }
            let Key::Str(name) = key else {
                unreachable!("root keys are strings")
            };
            if !self.window_escaped
                && self.diagnostics.len() == diagnostics_before
                && !dependents.contains_key(name)
                && matches!(&*slot.value.state.borrow(), State::Done(_))
            {
                self.evict_key(name, &slot.value, cells_before);
            }
        }
        self.window = 0;
        self.window_cell = std::ptr::null();
        if failed {
            return Err(Poisoned);
        }
        let entries = keyed.slots.keys().cloned().zip(values).collect();
        let keyed = Keyed {
            name: None,
            entries,
            defaults: keyed.defaults,
            defaulted: keyed.defaulted.clone(),
        };
        Ok(Value::new(ValueKind::Keyed(Box::new(keyed)), shaped.span))
    }

    /// Frees what a root key computed: its cell starts again, and the cells
    /// since `cells_before` are its own.
    fn evict_key(&mut self, key: &Arc<str>, cell: &Lazy, cells_before: usize) {
        *cell.state.borrow_mut() = State::Pending(Box::new(Job::Key(key.clone())));
        for cell in self.cells.get_mut().drain(cells_before..) {
            *cell.state.borrow_mut() = State::Running;
        }
    }

    /// The chain of blocks an operation is evaluated in: the root, and for an
    /// operation lifted out of blocks, those blocks' locations.
    fn op_chain(
        &self,
        root: &Location,
        root_chain: &Chain,
        key: &Arc<str>,
        blocks: &[Vec<Selector>],
    ) -> Chain {
        let mut chain = root_chain.clone();
        for path in blocks.iter().rev() {
            let mut location = root.child(Step::Key(Key::Str(key.clone())));
            for step in path.iter().filter_map(selector_step) {
                location = location.child(step);
            }
            chain = Some(Rc::new(ChainLink {
                location: Some(location),
                bind_root: false,
                name: None,
                next: chain,
            }));
        }
        chain
    }

    /// Applies a key's operations in order (`SPEC.md` 7.4). Returns the
    /// final value, if the key exists, and the operation that created it.
    fn apply_chain(
        &mut self,
        key: &Arc<str>,
        order: &[usize],
        root_chain: &Chain,
        root: &Location,
        vbinds: &VBinds,
        context: usize,
    ) -> Option<(usize, Slot)> {
        let ops = self.plan.expect("data mode").ops;
        let here = root.child(Step::Key(Key::Str(key.clone())));
        let mut value: Option<(usize, Lazy)> = None;
        for &i in order {
            let op = &ops[i];
            let chain = self.op_chain(root, root_chain, key, &op.blocks);
            if !op.path.is_empty() {
                let filled = if value.is_none() {
                    let below = DefaultedBelow {
                        below: Below::Missing,
                        chain: chain.clone(),
                        here: Some(here.clone()),
                    };
                    self.start_of(&below, op.span, op.key_optional)
                        .map(|cell| (i, cell))
                } else {
                    None
                };
                let Some((created, base)) = value.clone().or(filled) else {
                    // `-?`: a remove below a key that is not set does nothing.
                    if op.if_set {
                        continue;
                    }
                    let mut target = match op.op {
                        None => format!("-{key}"),
                        Some(SetOp::Set) => key.to_string(),
                        Some(set) => format!("{}{key}", set.symbol()),
                    };
                    for selector in &op.path {
                        target.push_str(&describe_selector(selector));
                    }
                    let _ = self.fail(
                        op.span,
                        format!("{target}: `{key}` is not set, so it cannot be patched"),
                    );
                    continue;
                };
                let entry = op.value.map(|expr| {
                    let start = PathStart {
                        base: &base,
                        optional: op.key_optional,
                    };
                    let below = self.path_below(start, &op.path, &chain, &Some(here.clone()));
                    Rc::new(EntryRecipe {
                        expr,
                        below,
                        merge: !op.replace && self.merges_below(expr),
                        replace: op.replace,
                        vbinds: vbinds.clone(),
                    })
                });
                let recipe = Rc::new(PathRecipe {
                    base: Recipe::Fixed,
                    base_value: base.clone(),
                    key_optional: op.key_optional,
                    path: op.path.clone(),
                    op: op.op,
                    if_set: op.if_set,
                    value: entry,
                    span: op.span,
                });
                let cell = self.cell(
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
                    if value.take().is_none() && !op.if_set {
                        let _ = self.fail(op.span, format!("-: `{key}` is not set"));
                    }
                }
                Some(set) => {
                    if let Some((created, _)) = &value {
                        match set {
                            SetOp::Declare => {
                                let diagnostic = Diagnostic::error(
                                    op.span,
                                    format!("+: `{key}` is already set"),
                                )
                                .with_related(ops[*created].span, "it is set here");
                                self.report(diagnostic);
                                continue;
                            }
                            SetOp::Ensure => continue,
                            SetOp::Set => {}
                        }
                    }
                    let below = match (&value, set) {
                        (Some((_, cell)), SetOp::Set) => Below::Value(cell.clone()),
                        (None, SetOp::Set) => {
                            self.defaulted(Below::Missing, &chain, &Some(here.clone()))
                        }
                        _ => Below::Missing,
                    };
                    let cell = self.cell(
                        Job::Expr {
                            expr: op.value.expect("a set has a value"),
                            env: Env {
                                chain,
                                here: Some(here.clone()),
                                below: below.clone(),
                                outer_below: below,
                                merge: self.merge_root
                                    && !op.replace
                                    && self.merges_below(op.value.expect("a set has a value")),
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
                born: self.window,
                context: context_tag(context),
            });
            vbinds = Some(Rc::new(VBindLink {
                id: bind,
                cell: cell.clone(),
                next: vbinds,
            }));
            self.cells.borrow_mut().push(cell.clone());
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
                    merge: false,
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
                State::Literal(expr) => {
                    let expr = *expr;
                    *state = State::Running;
                    Job::Literal(expr)
                }
                State::Pending(_) => {
                    if cell.born != self.window && !std::ptr::eq(Rc::as_ptr(cell), self.window_cell)
                    {
                        self.window_escaped = true;
                    }
                    match std::mem::replace(&mut *state, State::Running) {
                        State::Pending(job) => *job,
                        _ => unreachable!(),
                    }
                }
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
        if let Some((_, first)) = self.stack.get(start)
            && !first.is_detached()
        {
            diagnostic = diagnostic.with_related(*first, "which is evaluated here");
        }
        diagnostic.context = cell.location.as_ref().map(Location::describe);
        self.diagnostics.push(diagnostic);
        Fail::Poison
    }

    fn run(&mut self, job: Job) -> Eval<Rc<Shaped>> {
        match job {
            Job::Expr { expr, env } => self.eval(expr, &env),
            Job::Key(key) => self.run_key(&key),
            Job::Literal(expr) => {
                let Expr { kind, span } = &self.ir.exprs[expr];
                Self::shaped(literal_shape(kind), *span)
            }
            Job::Path {
                base,
                recipe,
                chain,
                here,
                context,
            } => {
                let mut shaped = self.force(&base, recipe.span)?;
                let mut at = Below::Value(base);
                if recipe.key_optional {
                    let below = DefaultedBelow {
                        below: at,
                        chain: chain.clone(),
                        here: here.clone(),
                    };
                    if is_none(&shaped) {
                        let start = self.start_or_empty(&below, recipe.span);
                        shaped = self.force(&start, recipe.span)?;
                    }
                    at = Below::Optional(Rc::new(below));
                }
                let path = &recipe.path;
                let into = PathInto {
                    at: &at,
                    optional: recipe.key_optional,
                };
                self.descend(&shaped, into, path, &recipe, &chain, here, context)
            }
            Job::Merge {
                left,
                right,
                chain,
                here,
                context,
                span,
            } => {
                let new = self.force(&right, span)?;
                let old = match self.force(&left, span) {
                    Ok(old) => old,
                    Err(Fail::Missing(..)) => return Ok(new),
                    Err(fail) => return Err(fail),
                };
                match (&old.shape, &new.shape) {
                    (Shape::Keyed(a), Shape::Keyed(b)) if names_agree(&a.name, &b.name) => {
                        let copy = CopyTo {
                            chain: &chain,
                            location: &here,
                            context,
                            deep: true,
                            outer_below: &Below::Missing,
                            span,
                        };
                        self.merge_keyed(a, b, &copy)
                    }
                    (Shape::Named(x, a), Shape::Named(y, b)) if x == y => {
                        let copy = CopyTo {
                            chain: &chain,
                            location: &here,
                            context,
                            deep: true,
                            outer_below: &Below::Missing,
                            span,
                        };
                        Ok(self.merge_named(x, a, b, new.span, &copy))
                    }
                    _ => Ok(new),
                }
            }
        }
    }

    fn instantiate(
        &self,
        recipe: &Recipe,
        value: Option<&Lazy>,
        chain: &Chain,
        here: Option<Location>,
        context: usize,
    ) -> Lazy {
        match recipe {
            Recipe::Fixed => value.expect("a fixed recipe has its value").clone(),
            Recipe::Literal(expr) => self.literal_cell(*expr, here),
            Recipe::Entry(entry) => self.cell(
                Job::Expr {
                    expr: entry.expr,
                    env: Env {
                        chain: chain.clone(),
                        here: here.clone(),
                        below: entry.below.clone(),
                        outer_below: entry.below.clone(),
                        merge: entry.merge,
                        vbinds: entry.vbinds.clone(),
                        context,
                    },
                },
                here,
            ),
            Recipe::Merge(merge) => {
                let MergeRecipe { left, right, span } = &**merge;
                self.cell(
                    Job::Merge {
                        left: self.instantiate(
                            &left.recipe,
                            Some(&left.value),
                            chain,
                            here.clone(),
                            context,
                        ),
                        right: self.instantiate(
                            &right.recipe,
                            Some(&right.value),
                            chain,
                            here.clone(),
                            context,
                        ),
                        chain: chain.clone(),
                        here: here.clone(),
                        context,
                        span: *span,
                    },
                    here,
                )
            }
            Recipe::Path(path) => {
                let base = self.instantiate(
                    &path.base,
                    Some(&path.base_value),
                    chain,
                    here.clone(),
                    context,
                );
                self.cell(
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
            ExprKind::Ref(kind) => self.reference(kind, env, span),
            ExprKind::Block(block) => {
                let start = if env.merge {
                    self.merge_start(*block, env, span)?
                } else {
                    None
                };
                self.block(*block, start, true, env, span)
            }
            ExprKind::List(elements) => {
                let cells = self.elements(elements, env, None);
                Self::shaped(Shape::List(cells), span)
            }
            ExprKind::Tuple(elements) => {
                let cells = self.elements(elements, env, Some(None));
                Self::shaped(Shape::Tuple(cells), span)
            }
            ExprKind::Named(named) => {
                let (name, elements) = &**named;
                let cells = self.elements(elements, env, Some(Some(name)));
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
            ExprKind::Not(operand) => {
                let value = self.eval(*operand, &Self::detached(env))?;
                match &value.shape {
                    Shape::Bool(b) => Self::shaped(Shape::Bool(!b), span),
                    other => {
                        let kind = other.kind_name();
                        Err(self.fail(span, format!("!: cannot invert {kind}")))
                    }
                }
            }
            ExprKind::Binary(op, lhs, rhs) => self.binary(*op, *lhs, *rhs, env, span),
            ExprKind::Macro(kind, args) => {
                let detached = Self::detached(env);
                let mut values = Vec::with_capacity(args.len());
                for &arg in args {
                    values.push(self.eval(arg, &detached)?);
                }
                self.macro_call(*kind, &values, span)
            }
            ExprKind::Error => Err(Fail::Poison),
            literal => Self::shaped(literal_shape(literal), span),
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
    /// `positional` says that `&` in an element reads the same position of
    /// a tuple (`Some(None)`), or a named tuple of this name, below it.
    fn elements(
        &mut self,
        elements: &[ExprId],
        env: &Env,
        positional: Option<Option<&Arc<str>>>,
    ) -> Vec<Lazy> {
        let positional = positional.filter(|_| !env.below.is_nothing());
        let named = matches!(positional, Some(Some(_)));
        elements
            .iter()
            .enumerate()
            .map(|(i, &element)| {
                let here = child_location(&env.here, Step::Index(i));
                if self.is_literal(element) {
                    return self.literal_cell(element, here);
                }
                let below = match positional {
                    Some(name) => Below::Element(Rc::new(ElementBelow {
                        below: env.below.clone(),
                        name: name.cloned(),
                        index: i,
                    })),
                    None => env.below.clone(),
                };
                let env = Env {
                    here: here.clone(),
                    below,
                    outer_below: Below::Missing,
                    merge: env.merge && named && self.merges_below(element),
                    ..env.clone()
                };
                self.cell(Job::Expr { expr: element, env }, here)
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
                // A root key is a field where the root type is the whole
                // document (`SPEC.md` 4.1).
                let key = target
                    .root_key
                    .as_ref()
                    .filter(|_| defaults.root_is_document());
                let path: Vec<String> = key
                    .into_iter()
                    .chain(&target.path)
                    .map(|p| p.to_string())
                    .collect();
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
                if self.is_unset(&value, false) {
                    return Err(Fail::Missing(
                        span,
                        "&: there is no value below here".to_owned(),
                    ));
                }
                let selector = Selector {
                    kind: selector.kind.clone(),
                    span,
                    optional: false,
                };
                match self.select(&value, &selector) {
                    Err(Fail::Missing(..)) => Err(Fail::Missing(
                        span,
                        "&: there is no value below here".to_owned(),
                    )),
                    result => result,
                }
            }
            Below::Element(element) => {
                let cell = self.resolve_below(&element.below, span)?;
                let value = self.force(&cell, span)?;
                let items = match (&value.shape, &element.name) {
                    (Shape::Named(name, items), Some(wanted)) if name == wanted => items,
                    (Shape::Tuple(items), None) => items,
                    // Of another shape, it is the value below as a whole.
                    _ => return Ok(cell),
                };
                items.get(element.index).cloned().ok_or_else(|| {
                    Fail::Missing(span, "&: there is no value below here".to_owned())
                })
            }
            Below::Defaulted(defaulted) => match self.resolve_below(&defaulted.below, span) {
                Err(Fail::Missing(at, message)) => {
                    match self.default_cell(defaulted, span, false) {
                        Some(cell) => Ok(cell),
                        None => Err(Fail::Missing(at, message)),
                    }
                }
                result => result,
            },
            Below::Optional(optional) => {
                let cell = match self.resolve_below(&optional.below, span) {
                    Ok(cell) => cell,
                    Err(Fail::Missing(..)) => return Ok(self.start_or_empty(optional, span)),
                    Err(fail) => return Err(fail),
                };
                let value = self.force(&cell, span)?;
                if is_none(&value) {
                    return Ok(self.start_or_empty(optional, span));
                }
                Ok(some_payload(&value).cloned().unwrap_or(cell))
            }
        }
    }

    /// What a `?` step starts from where it is unset: its default, or else
    /// an empty block with `..` (`SPEC.md` 5.1).
    fn start_or_empty(&mut self, below: &DefaultedBelow, span: Span) -> Lazy {
        self.default_cell(below, span, true).unwrap_or_else(|| {
            let keyed = Keyed {
                defaults: true,
                ..Keyed::default()
            };
            let value = Value::new(ValueKind::Keyed(Box::new(keyed)), span);
            Cell::done(shaped_from_value(&value))
        })
    }

    /// What a path starts from at a step that is not set: for a `?`, as
    /// [`start_or_empty`](Self::start_or_empty) says, and otherwise the
    /// default with `init_defaults`.
    fn start_of(&mut self, below: &DefaultedBelow, span: Span, optional: bool) -> Option<Lazy> {
        if optional {
            Some(self.start_or_empty(below, span))
        } else {
            self.default_cell(below, span, false)
        }
    }

    /// Whether a path goes through this value from a default instead: with
    /// `init_defaults` or at a `?` step, an `Option` that is `None`
    /// (`SPEC.md` 5.1).
    fn is_unset(&self, value: &Shaped, optional: bool) -> bool {
        (self.init_defaults || optional) && is_none(value)
    }

    /// `below` at `here`, or with `init_defaults`, its default where it is
    /// missing.
    fn defaulted(&self, below: Below, chain: &Chain, here: &Option<Location>) -> Below {
        if self.init_defaults && self.defaults.is_some() {
            Below::Defaulted(Rc::new(DefaultedBelow {
                below,
                chain: chain.clone(),
                here: here.clone(),
            }))
        } else {
            below
        }
    }

    /// What is below a path from `start`, which is at `here`.
    fn path_below(
        &self,
        start: PathStart<'_>,
        path: &[Selector],
        chain: &Chain,
        here: &Option<Location>,
    ) -> Below {
        let optional = |below: Below, optional: bool, here: &Option<Location>| {
            if optional {
                Below::Optional(Rc::new(DefaultedBelow {
                    below,
                    chain: chain.clone(),
                    here: here.clone(),
                }))
            } else {
                below
            }
        };
        let mut below = optional(Below::Value(start.base.clone()), start.optional, here);
        let mut here = here.clone();
        for selector in path {
            if let Some(step) = selector_step(selector) {
                here = child_location(&here, step);
            }
            below = self.defaulted(
                Below::Select(Rc::new(below), selector.clone()),
                chain,
                &here,
            );
            below = optional(below, selector.optional, &here);
        }
        below
    }

    /// The default of what is missing at `defaulted`, if it has one
    /// (`SPEC.md` 5.1).
    fn default_cell(
        &mut self,
        defaulted: &DefaultedBelow,
        span: Span,
        optional: bool,
    ) -> Option<Lazy> {
        if !self.init_defaults && !optional {
            return None;
        }
        let defaults = self.defaults?;
        let from_values = match &defaulted.below {
            Below::Select(parent, selector) => {
                let field = selector_field(selector)?;
                self.type_path(parent, span).map(|(name, mut path)| {
                    path.push(field);
                    (Some(name), path)
                })
            }
            _ => None,
        };
        let (name, path) = from_values.or_else(|| {
            let here = defaulted.here.as_ref()?;
            self.chain_path(&defaulted.chain, here)
        })?;
        let value = defaults.start_of(name.as_deref(), &path)?;
        Some(Cell::done(shaped_from_value(&value)))
    }

    /// The nearest type name at or around what is at `below`, with the
    /// fields from there to it.
    fn type_path(&mut self, below: &Below, span: Span) -> Option<(Arc<str>, Vec<String>)> {
        let value = match self.resolve_below(below, span) {
            Ok(cell) => self.force(&cell, span).ok(),
            Err(_) => None,
        };
        if let Some(value) = value
            && let Shape::Keyed(keyed) = &value.shape
            && let Some(name) = &keyed.name
        {
            return Some((name.clone(), Vec::new()));
        }
        match below {
            Below::Select(inner, selector) => {
                let (name, mut path) = self.type_path(inner, span)?;
                path.push(selector_field(selector)?);
                Some((name, path))
            }
            Below::Defaulted(defaulted) | Below::Optional(defaulted) => {
                self.type_path(&defaulted.below, span)
            }
            Below::Missing | Below::Value(_) | Below::Element(_) => None,
        }
    }

    /// The path to `here` from the nearest block with a type name, or from
    /// the root type (`SPEC.md` 6.1.3).
    fn chain_path(
        &self,
        chain: &Chain,
        here: &Location,
    ) -> Option<(Option<Arc<str>>, Vec<String>)> {
        let fields = |steps: &[&Step]| -> Option<Vec<String>> {
            steps
                .iter()
                .map(|step| match step {
                    Step::Key(Key::Str(name)) => Some(name.to_string()),
                    _ => None,
                })
                .collect()
        };
        let steps = here.step_refs();
        let mut link = chain.as_ref();
        while let Some(l) = link {
            if let (Some(name), Some(location)) = (&l.name, &l.location) {
                let depth = location.step_refs().len();
                return Some((Some(name.clone()), fields(steps.get(depth..)?)?));
            }
            link = l.next.as_ref();
        }
        if here.anchor != Anchor::Root {
            return None;
        }
        // A root key is a step only where the root type is the document.
        let skip = usize::from(!self.defaults?.root_is_document());
        Some((None, fields(steps.get(skip..)?)?))
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

    /// What a block that merges starts from: the value below, if that is
    /// a keyed value of the block's type (`SPEC.md` 5.2). Anything else is
    /// replaced, so a struct variant replaces another one.
    fn merge_start(&mut self, id: BlockId, env: &Env, span: Span) -> Eval<Option<Rc<Shaped>>> {
        let below = match self.resolve_below(&env.below, span) {
            Ok(cell) => self.force(&cell, span),
            Err(fail) => Err(fail),
        };
        let below = match below {
            Ok(below) => below,
            Err(Fail::Missing(..)) => return Ok(None),
            Err(fail) => return Err(fail),
        };
        let name = &self.ir.blocks[id].name;
        Ok(match &below.shape {
            Shape::Keyed(keyed) if names_agree(&keyed.name, name) => Some(below),
            _ => None,
        })
    }

    /// `deep` says whether the block merges nested values with what it
    /// starts from: `~^` does not.
    fn block(
        &mut self,
        id: BlockId,
        start: Option<Rc<Shaped>>,
        deep: bool,
        env: &Env,
        span: Span,
    ) -> Eval<Rc<Shaped>> {
        let ir = self.ir;
        let block = &ir.blocks[id];
        let is_patch = start.is_some();
        let mut keyed = match &start {
            None => KeyedShape::default(),
            Some(start) => match &start.shape {
                Shape::Keyed(keyed) => (**keyed).clone(),
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
            name: keyed.name.clone(),
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
                merge: false,
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
            let copy = CopyTo {
                chain: &chain,
                location: &location,
                context: env.context,
                deep,
                outer_below: &outer_below,
                span: spread_span,
            };
            self.copy_entries(source, &mut keyed, &copy);
        }

        let scope = BlockScope {
            chain,
            location,
            outer_below,
            deep,
            merging: deep && (is_patch || !block.spreads.is_empty()),
            vbinds,
            context: env.context,
        };
        for entry in &block.entries {
            self.entry(entry, &mut keyed, &scope);
        }
        if block.defaults {
            keyed.defaults = true;
        }
        Self::shaped(Shape::Keyed(Box::new(keyed)), span)
    }

    /// Sets each of `source`'s entries in `target`, evaluated again at the
    /// target's location, and passes on its defaults (`SPEC.md` 5.2).
    fn copy_entries(&mut self, source: &KeyedShape, target: &mut KeyedShape, copy: &CopyTo<'_>) {
        for (key, slot) in &source.slots {
            let here = child_location(copy.location, Step::Key(key.clone()));
            let recipe = self.landed(key, slot, target.slots.get(key), copy);
            let value =
                self.instantiate(&recipe, Some(&slot.value), copy.chain, here, copy.context);
            target.slots.insert(key.clone(), Slot { value, recipe });
            target.defaulted.remove(key);
        }
        target.defaults |= source.defaults;
        target.defaulted.extend(source.defaulted.iter().cloned());
    }

    /// How a copied entry is produced where it lands: `&` with nothing
    /// below where it was written reads what is below here (`SPEC.md` 6.1),
    /// and a value over one the key already holds merges, if deep.
    fn landed(&self, key: &Key, slot: &Slot, existing: Option<&Slot>, copy: &CopyTo<'_>) -> Recipe {
        let merged = |recipe: Recipe| match existing {
            Some(old) if copy.deep => Recipe::Merge(Rc::new(MergeRecipe {
                left: old.clone(),
                right: Slot {
                    value: slot.value.clone(),
                    recipe,
                },
                span: copy.span,
            })),
            _ => recipe,
        };
        let Recipe::Entry(entry) = &slot.recipe else {
            // `Recipe::Literal` too: whatever is below, it is the same.
            return merged(slot.recipe.clone());
        };
        if entry.replace {
            return slot.recipe.clone();
        }
        if !entry.below.is_nothing() {
            return merged(slot.recipe.clone());
        }
        // Nothing below there and nothing here: the common spread of a
        // template, which needs no recipe of its own.
        if existing.is_none() && copy.outer_below.is_nothing() && !self.init_defaults {
            return slot.recipe.clone();
        }
        let below = match existing {
            Some(old) => Below::Value(old.value.clone()),
            None => {
                let span = self.ir.exprs[entry.expr].span;
                let here = child_location(copy.location, Step::Key(key.clone()));
                self.defaulted(
                    Below::Select(Rc::new(copy.outer_below.clone()), key_selector(key, span)),
                    copy.chain,
                    &here,
                )
            }
        };
        let block = self.merges_below(entry.expr);
        let recipe = Recipe::Entry(Rc::new(EntryRecipe {
            expr: entry.expr,
            below,
            merge: copy.deep && block,
            replace: false,
            vbinds: entry.vbinds.clone(),
        }));
        if block { recipe } else { merged(recipe) }
    }

    /// Whether an expression is written as a block, or a named tuple with
    /// one among its elements (`SPEC.md` 5.2).
    fn merges_below(&self, expr: ExprId) -> bool {
        match &self.ir.exprs[expr].kind {
            ExprKind::Block(_) => true,
            ExprKind::Named(named) => named.1.iter().any(|&e| self.merges_below(e)),
            _ => false,
        }
    }

    fn entry(&mut self, entry: &Entry, keyed: &mut KeyedShape, scope: &BlockScope) {
        let BlockScope {
            chain,
            location,
            outer_below,
            deep,
            merging,
            vbinds,
            context,
        } = scope;
        let context = *context;
        let key = &entry.key;
        let existing = keyed.slots.get(key).cloned();
        let here = child_location(location, Step::Key(key.clone()));
        if !entry.path.is_empty() {
            let filled = existing.is_none().then(|| DefaultedBelow {
                below: Below::Select(Rc::new(outer_below.clone()), key_selector(key, entry.span)),
                chain: chain.clone(),
                here: here.clone(),
            });
            let filled =
                filled.and_then(|below| self.start_of(&below, entry.span, entry.key_optional));
            let filled = filled.map(|value| Slot {
                value,
                recipe: Recipe::Fixed,
            });
            let Some(slot) = existing.or(filled) else {
                // `-?`: a remove below a key that is not set does nothing.
                if entry.if_set {
                    return;
                }
                let target = describe_target(entry);
                let _ = self.fail(
                    entry.span,
                    format!("{target}: `{key}` is not set, so it cannot be patched"),
                );
                return;
            };
            let value = entry.value.map(|expr| {
                let start = PathStart {
                    base: &slot.value,
                    optional: entry.key_optional,
                };
                let below = self.path_below(start, &entry.path, chain, &here);
                Rc::new(EntryRecipe {
                    expr,
                    below,
                    merge: *deep && !entry.replace && self.merges_below(expr),
                    replace: entry.replace,
                    vbinds: vbinds.clone(),
                })
            });
            let recipe = Recipe::Path(Rc::new(PathRecipe {
                base: slot.recipe.clone(),
                base_value: slot.value.clone(),
                key_optional: entry.key_optional,
                path: entry.path.clone(),
                op: entry.op,
                if_set: entry.if_set,
                value,
                span: entry.span,
            }));
            let value = self.instantiate(&recipe, Some(&slot.value), chain, here, context);
            keyed.slots.insert(key.clone(), Slot { value, recipe });
            return;
        }
        match entry.op {
            None => {
                if existing.is_none() && !entry.if_set {
                    let _ = self.fail(entry.span, format!("-: `{key}` is not set"));
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
                                Diagnostic::error(entry.span, format!("+: `{key}` is already set"));
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
                let expr = entry.value.expect("a set entry has a value");
                let recipe = if self.is_literal(expr) && !entry.replace {
                    Recipe::Literal(expr)
                } else {
                    let below = match &existing {
                        Some(slot) => Below::Value(slot.value.clone()),
                        None => self.defaulted(
                            Below::Select(
                                Rc::new(outer_below.clone()),
                                key_selector(key, entry.span),
                            ),
                            chain,
                            &here,
                        ),
                    };
                    let below = if op == SetOp::Set {
                        below
                    } else {
                        Below::Missing
                    };
                    Recipe::Entry(Rc::new(EntryRecipe {
                        expr,
                        below,
                        merge: *merging && !entry.replace && self.merges_below(expr),
                        replace: entry.replace,
                        vbinds: vbinds.clone(),
                    }))
                };
                let value = self.instantiate(&recipe, None, chain, here, context);
                keyed.slots.insert(key.clone(), Slot { value, recipe });
                keyed.defaulted.remove(key);
            }
        }
    }

    /// Where a cell's value is written, if it has a place of its own.
    fn own_span(&self, cell: &Lazy) -> Option<Span> {
        cell.expr
            .map(|expr| self.ir.exprs[expr].span)
            .or_else(|| self.span_of(cell))
            .filter(|span| !span.is_detached())
    }

    /// The span of a value that is already computed, for related spans.
    fn span_of(&self, cell: &Lazy) -> Option<Span> {
        match &*cell.state.borrow() {
            State::Done(Ok(shaped)) => Some(shaped.span),
            State::Literal(expr) => Some(self.ir.exprs[*expr].span),
            State::Pending(job) => match &**job {
                Job::Expr { expr, .. } | Job::Literal(expr) => Some(self.ir.exprs[*expr].span),
                Job::Path { .. } | Job::Key(_) => None,
                Job::Merge { span, .. } => Some(*span),
            },
            _ => None,
        }
    }

    // ---- Path entries (`SPEC.md` 5.1) ----

    #[allow(clippy::too_many_arguments)]
    fn apply_path(
        &mut self,
        value: &Rc<Shaped>,
        at: &Below,
        path: &[Selector],
        recipe: &PathRecipe,
        chain: &Chain,
        here: Option<Location>,
        context: usize,
    ) -> Eval<Rc<Shaped>> {
        let (selector, rest) = path.split_first().expect("a path entry has a path");
        let step = match (&value.shape, &selector.kind) {
            (Shape::Keyed(_), SelectorKind::Field(name)) => Step::Key(Key::Str(name.clone())),
            (Shape::Keyed(_), SelectorKind::Accessor(i)) => Step::Key(Key::Int(*i)),
            (_, SelectorKind::Accessor(i)) => {
                Step::Index(usize::try_from(*i).unwrap_or(usize::MAX))
            }
            (_, SelectorKind::Element(i)) => Step::Index(*i),
            (_, SelectorKind::Field(name)) => Step::Key(Key::Str(name.clone())),
            (_, SelectorKind::Context(_)) => {
                return Err(self.fail(selector.span, "internal error: a condition in a path"));
            }
        };
        let here = child_location(&here, step);
        if rest.is_empty() {
            return self.apply_last(value, selector, recipe, chain, here, context);
        }
        let child_at = Below::Select(Rc::new(at.clone()), selector.clone());
        let found = match self.select(value, selector) {
            Ok(child) => {
                let forced = self.force(&child, selector.span)?;
                (!self.is_unset(&forced, selector.optional)).then_some(child)
            }
            Err(Fail::Missing(..)) => None,
            Err(fail) => return Err(fail),
        };
        let child = match found {
            Some(child) => child,
            None => {
                let below = DefaultedBelow {
                    below: child_at.clone(),
                    chain: chain.clone(),
                    here: here.clone(),
                };
                // Only a keyed value can take a key it does not have.
                let optional = selector.optional && matches!(value.shape, Shape::Keyed(_));
                let Some(child) = self.start_of(&below, selector.span, optional) else {
                    // `-?`: a remove below a step that is not set does nothing.
                    if recipe.if_set {
                        return Ok(value.clone());
                    }
                    let message = missing_step(selector, rest, recipe.op, here.as_ref());
                    return Err(self.fail(selector.span, message));
                };
                child
            }
        };
        let child = self.force(&child, selector.span)?;
        let child_at = if selector.optional {
            Below::Optional(Rc::new(DefaultedBelow {
                below: child_at,
                chain: chain.clone(),
                here: here.clone(),
            }))
        } else {
            child_at
        };
        let into = PathInto {
            at: &child_at,
            optional: selector.optional,
        };
        let new_child = self.descend(&child, into, rest, recipe, chain, here, context)?;
        Ok(self.with_child(value, selector, Cell::done(new_child)))
    }

    /// Goes on with a path from `value`. A `?` step goes into what a
    /// `Some(...)` holds, and the result stays in it (`SPEC.md` 5.1).
    #[allow(clippy::too_many_arguments)]
    fn descend(
        &mut self,
        value: &Rc<Shaped>,
        into: PathInto<'_>,
        path: &[Selector],
        recipe: &PathRecipe,
        chain: &Chain,
        here: Option<Location>,
        context: usize,
    ) -> Eval<Rc<Shaped>> {
        let held = some_payload(value).filter(|_| into.optional).cloned();
        let Some(held) = held else {
            return self.apply_path(value, into.at, path, recipe, chain, here, context);
        };
        let inner = self.force(&held, recipe.span)?;
        let new = self.apply_path(&inner, into.at, path, recipe, chain, here, context)?;
        let element = Selector {
            kind: SelectorKind::Element(0),
            span: recipe.span,
            optional: false,
        };
        Ok(self.with_child(value, &element, Cell::done(new)))
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
                (
                    this.instantiate(&recipe, None, chain, here.clone(), context),
                    recipe,
                )
            })
        };
        match (&value.shape, &selector.kind) {
            (Shape::Keyed(keyed), SelectorKind::Field(_) | SelectorKind::Accessor(_)) => {
                let key = match &selector.kind {
                    SelectorKind::Field(name) => Key::Str(name.clone()),
                    SelectorKind::Accessor(i) => Key::Int(*i),
                    SelectorKind::Element(_) | SelectorKind::Context(_) => unreachable!(),
                };
                let mut keyed = keyed.clone();
                let exists = keyed.slots.contains_key(&key);
                match recipe.op {
                    None if !exists && !recipe.if_set => {
                        return Err(self.fail(span, format!("-: `{key}` is not set")));
                    }
                    None => {
                        keyed.slots.shift_remove(&key);
                        keyed.defaulted.insert(key);
                    }
                    Some(SetOp::Declare) if exists => {
                        return Err(self.fail(span, format!("+: `{key}` is already set")));
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
                    if recipe.op.is_none() && recipe.if_set {
                        return Ok(value.clone());
                    }
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
                    SelectorKind::Field(name) => Key::Str(name.clone()),
                    SelectorKind::Accessor(i) => Key::Int(*i),
                    SelectorKind::Element(i) => Key::Int(*i as i128),
                    SelectorKind::Context(_) => {
                        unreachable!("with_child follows a successful select")
                    }
                };
                let mut keyed = keyed.clone();
                keyed.defaulted.remove(&key);
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
            BinOp::Merge | BinOp::MergeShallow => {
                let deep = op == BinOp::Merge;
                let left = self.eval(lhs, env)?;
                if let ExprKind::Block(block) = ir.exprs[rhs].kind {
                    let rhs_span = ir.exprs[rhs].span;
                    return self.block(block, Some(left), deep, env, rhs_span);
                }
                // Blocks in a named tuple of the same name are patches too.
                if let (true, Shape::Named(x, _), ExprKind::Named(named)) =
                    (deep, &left.shape, &ir.exprs[rhs].kind)
                    && *x == named.0
                    && self.merges_below(rhs)
                {
                    let patch = Env {
                        below: Below::Value(Cell::done(left)),
                        merge: true,
                        ..env.clone()
                    };
                    return self.eval(rhs, &patch);
                }
                let right = self.eval(rhs, env)?;
                self.merge(&left, &right, deep, env, span)
            }
            BinOp::Union => {
                let detached = Self::detached(env);
                // `|` used to be the fallback: where its left side is
                // missing, say what is written now. It stays missing, so
                // that a `??` around it still catches it.
                let left = self.eval(lhs, &detached).map_err(|fail| match fail {
                    Fail::Missing(at, message) => Fail::Missing(
                        at,
                        format!("{message}; for a fallback where a value is missing, write `??`"),
                    ),
                    fail => fail,
                })?;
                let right = self.eval(rhs, &detached)?;
                self.arithmetic(op, &left, &right, span)
            }
            _ => {
                let detached = Self::detached(env);
                let left = self.eval(lhs, &detached)?;
                let right = self.eval(rhs, &detached)?;
                self.arithmetic(op, &left, &right, span)
            }
        }
    }

    /// `b` over `a`: the type names are for the caller to check.
    fn merge_keyed(
        &mut self,
        a: &KeyedShape,
        b: &KeyedShape,
        copy: &CopyTo<'_>,
    ) -> Eval<Rc<Shaped>> {
        let mut keyed = a.clone();
        if keyed.name.is_none() {
            keyed.name = b.name.clone();
        }
        let chain = Some(Rc::new(ChainLink {
            location: copy.location.clone(),
            bind_root: false,
            name: keyed.name.clone(),
            next: copy.chain.clone(),
        }));
        let inner = CopyTo {
            chain: &chain,
            ..*copy
        };
        self.copy_entries(b, &mut keyed, &inner);
        Self::shaped(Shape::Keyed(Box::new(keyed)), copy.span)
    }

    /// `b` over `a`, two named tuples of one name: an element merges with the
    /// one at its position, as an entry over a key does (`SPEC.md` 6.4).
    fn merge_named(
        &mut self,
        name: &Arc<str>,
        a: &[Lazy],
        b: &[Lazy],
        span: Span,
        copy: &CopyTo<'_>,
    ) -> Rc<Shaped> {
        let elements = b
            .iter()
            .enumerate()
            .map(|(i, right)| {
                let Some(left) = a.get(i) else {
                    return right.clone();
                };
                let here = child_location(copy.location, Step::Index(i));
                self.cell(
                    Job::Merge {
                        left: left.clone(),
                        right: right.clone(),
                        chain: copy.chain.clone(),
                        here: here.clone(),
                        context: copy.context,
                        span: copy.span,
                    },
                    here,
                )
            })
            .collect();
        Rc::new(Shaped {
            shape: Shape::Named(name.clone(), elements),
            span,
        })
    }

    fn merge(
        &mut self,
        left: &Shaped,
        right: &Shaped,
        deep: bool,
        env: &Env,
        span: Span,
    ) -> Eval<Rc<Shaped>> {
        match (&left.shape, &right.shape) {
            (Shape::Keyed(a), Shape::Keyed(b)) => {
                if let (Some(x), Some(y)) = (&a.name, &b.name)
                    && x != y
                {
                    return Err(self.fail(span, format!("~: cannot merge `{x}` with `{y}`")));
                }
                let copy = CopyTo {
                    chain: &env.chain,
                    location: &env.here,
                    context: env.context,
                    deep,
                    outer_below: &Below::Missing,
                    span,
                };
                self.merge_keyed(a, b, &copy)
            }
            (Shape::Named(x, a), Shape::Named(y, b)) if deep && x == y => {
                let copy = CopyTo {
                    chain: &env.chain,
                    location: &env.here,
                    context: env.context,
                    deep,
                    outer_below: &Below::Missing,
                    span,
                };
                Ok(self.merge_named(x, a, b, right.span, &copy))
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

    /// A built-in macro (`SPEC.md` 6.7).
    fn macro_call(&mut self, kind: MacroKind, args: &[Rc<Shaped>], span: Span) -> Eval<Rc<Shaped>> {
        let name = kind.name();
        for arg in args {
            if !matches!(arg.shape, Shape::Int(_) | Shape::Float(_)) {
                let found = arg.shape.kind_name();
                return Err(self.fail(span, format!("{name}: takes numbers, not {found}")));
            }
        }
        if kind == MacroKind::Clamp {
            return self.clamp(&args[0], &args[1], &args[2], span);
        }
        let float = match args[0].shape {
            Shape::Float(f) => f,
            _ => return Ok(args[0].clone()),
        };
        let whole = match kind {
            MacroKind::Round => float.round(),
            MacroKind::Floor => float.floor(),
            _ => float.ceil(),
        };
        // `i128::MAX as f64` rounds up to 2^127, which does not fit.
        if whole >= -(2f64.powi(127)) && whole < 2f64.powi(127) {
            Self::shaped(Shape::Int(whole as i128), span)
        } else {
            Err(self.fail(span, format!("{name}: integer overflow")))
        }
    }

    fn clamp(&mut self, x: &Shaped, lo: &Shaped, hi: &Shaped, span: Span) -> Eval<Rc<Shaped>> {
        if let (Shape::Int(x), Shape::Int(lo), Shape::Int(hi)) = (&x.shape, &lo.shape, &hi.shape) {
            if lo > hi {
                return Err(self.fail(
                    span,
                    format!("clamp!: the minimum {lo} is above the maximum {hi}"),
                ));
            }
            return Self::shaped(Shape::Int(*x.clamp(lo, hi)), span);
        }
        let float = |shaped: &Shaped| match shaped.shape {
            Shape::Int(i) => i as f64,
            Shape::Float(f) => f,
            _ => unreachable!("checked to be numbers"),
        };
        let (x, lo, hi) = (float(x), float(lo), float(hi));
        if lo > hi {
            let (lo, hi) = (format_float(lo), format_float(hi));
            return Err(self.fail(
                span,
                format!("clamp!: the minimum {lo} is above the maximum {hi}"),
            ));
        }
        Self::shaped(Shape::Float(x.clamp(lo, hi)), span)
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
            (Int(a), Int(b)) if op != BinOp::Union => {
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
            (Int(_) | Float(_), Int(_) | Float(_)) if op != BinOp::Union => {
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
            (List(a), List(b)) if op == BinOp::Union => {
                let mut held: Vec<Value> = a
                    .iter()
                    .map(|cell| self.full_value(cell, span, &mut Vec::new()))
                    .collect::<Result<_, _>>()?;
                let mut list = a.clone();
                for cell in b {
                    let value = self.full_value(cell, span, &mut Vec::new())?;
                    if !held.iter().any(|h| h.equals(&value)) {
                        held.push(value);
                        list.push(cell.clone());
                    }
                }
                List(list)
            }
            (a, b) => {
                let (a, b) = (a.kind_name(), b.kind_name());
                let message = match op {
                    BinOp::Add => format!("+: cannot add {b} to {a}"),
                    BinOp::Sub => format!("-: cannot subtract {b} from {a}"),
                    BinOp::Mul => format!("*: cannot multiply {a} by {b}"),
                    BinOp::Div => format!("/: cannot divide {a} by {b}"),
                    BinOp::Union if a == "a list" => {
                        format!("|: cannot add {b} to a list; put it in one: `| [x]`")
                    }
                    BinOp::Union => format!(
                        "|: cannot add {b} to {a}; `|` adds to a list, and a fallback where \
                         a value is missing is written `??`"
                    ),
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
        // The root is in no file, so what it holds speaks for itself.
        let span = if span.is_detached() {
            self.own_span(cell).unwrap_or(span)
        } else {
            span
        };
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
            Shape::Str(s) => ValueKind::String(s.clone()),
            Shape::Id(s) => ValueKind::Id(s.clone()),
            Shape::UnitName(n) => ValueKind::UnitName(n.clone()),
            Shape::List(cells) => ValueKind::List(self.all_full_values(cells, span, visiting)?),
            Shape::Tuple(cells) => ValueKind::Tuple(self.all_full_values(cells, span, visiting)?),
            Shape::Named(name, cells) => {
                ValueKind::NamedTuple(name.clone(), self.all_full_values(cells, span, visiting)?)
            }
            Shape::Keyed(keyed) => {
                let cells: Vec<Lazy> = keyed.slots.values().map(|s| s.value.clone()).collect();
                let values = self.all_full_values(&cells, span, visiting)?;
                ValueKind::Keyed(Box::new(Keyed {
                    name: keyed.name.clone(),
                    entries: keyed.slots.keys().cloned().zip(values).collect(),
                    defaults: keyed.defaults,
                    defaulted: keyed.defaulted.clone(),
                }))
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
