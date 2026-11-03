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

use crate::diagnostic::Diagnostic;
use crate::ir::*;
use crate::source::Span;
use crate::value::{Key, Keyed, Value, ValueKind, format_float};

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
    Pending(Job),
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
    },
}

impl Cell {
    fn new(job: Job, location: Option<Location>) -> Lazy {
        let expr = match &job {
            Job::Expr { expr, .. } => Some(*expr),
            Job::Path { .. } => None,
        };
        Rc::new(Cell {
            state: RefCell::new(State::Pending(job)),
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

/// A path from the document's value, or from a value bind's.
#[derive(Clone)]
struct Location {
    base: Weak<Cell>,
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
}

impl Location {
    fn child(&self, step: Step) -> Location {
        Location {
            base: self.base.clone(),
            name: self.name.clone(),
            path: Some(Rc::new(PathLink {
                step,
                parent: self.path.clone(),
            })),
        }
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
    }
}

pub(crate) struct Evaluator<'ir> {
    ir: &'ir Ir,
    diagnostics: Vec<Diagnostic>,
    /// The cells being computed, with the span of what forced each and
    /// where each value ends up.
    stack: Vec<(*const Cell, Span, Option<Location>)>,
    root: Option<Location>,
    /// Cells computed so far, against [`MAX_STEPS`].
    steps: u64,
}

/// Evaluates a lowered value-mode document.
pub(crate) fn evaluate(ir: &Ir, root: ExprId) -> (Option<Value>, Vec<Diagnostic>) {
    let mut evaluator = Evaluator {
        ir,
        diagnostics: Vec::new(),
        stack: Vec::new(),
        root: None,
        steps: 0,
    };
    let root_cell = Rc::new(Cell {
        state: RefCell::new(State::Running),
        expr: Some(root),
        location: None,
    });
    let location = Location {
        base: Rc::downgrade(&root_cell),
        name: Rc::from(""),
        path: None,
    };
    *root_cell.state.borrow_mut() = State::Pending(Job::Expr {
        expr: root,
        env: Env {
            chain: None,
            here: Some(location.clone()),
            below: Below::Missing,
            outer_below: Below::Missing,
            vbinds: None,
        },
    });
    evaluator.root = Some(location);
    let span = ir.exprs[root].span;
    let value = evaluator.full_value(&root_cell, span, &mut Vec::new()).ok();
    (value, evaluator.diagnostics)
}

impl Evaluator<'_> {
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
                .find_map(|(_, _, location)| location.as_ref().map(Location::describe));
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
                    State::Pending(job) => job,
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
        self.stack
            .push((Rc::as_ptr(cell), span, cell.location.clone()));
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
            .position(|(c, _, _)| *c == Rc::as_ptr(cell))
            .unwrap_or(0);
        let mut diagnostic = Diagnostic::error(span, "cycle: this value depends on itself");
        for (_, through, _) in &self.stack[start + 1..] {
            diagnostic = diagnostic.with_related(*through, "through this reference");
        }
        if let Some((_, first, _)) = self.stack.get(start) {
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
            } => {
                let shaped = self.force(&base, recipe.span)?;
                self.apply_path(&shaped, &recipe.path, &recipe, &chain, here)
            }
        }
    }

    fn instantiate(
        &self,
        recipe: &Recipe,
        value: &Lazy,
        chain: &Chain,
        here: Option<Location>,
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
                    },
                },
                here,
            ),
            Recipe::Path(path) => {
                let base = self.instantiate(&path.base, &path.base_value, chain, here.clone());
                Cell::new(
                    Job::Path {
                        base,
                        recipe: path.clone(),
                        chain: chain.clone(),
                        here: here.clone(),
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
                let root = self
                    .root
                    .clone()
                    .expect("the root is set before evaluating");
                self.value_at(&root, span)
            }
            RefKind::Default => Err(self.fail(
                span,
                "&default: defaults come from a schema, and no schema is given",
            )),
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
        for step in &location.steps() {
            let cell = match (&value.shape, step) {
                (Shape::Keyed(keyed), Step::Key(key)) => {
                    keyed.slots.get(key).map(|s| s.value.clone())
                }
                (Shape::List(v) | Shape::Tuple(v) | Shape::Named(_, v), Step::Index(i)) => {
                    v.get(*i).cloned()
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
            (Shape::Keyed(keyed), SelectorKind::Field(name)) => keyed
                .slots
                .get(&Key::Str(name.to_string()))
                .map(|s| s.value.clone()),
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
        let mut vbinds = env.vbinds.clone();
        let mut bind_cells = Vec::new();
        for &bind in &block.value_binds {
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
            bind_cells.push((bind, cell));
        }
        for (bind, cell) in bind_cells {
            let here = Location {
                base: Rc::downgrade(&cell),
                name: ir.value_binds[bind].name.clone(),
                path: None,
            };
            *cell.state.borrow_mut() = State::Pending(Job::Expr {
                expr: ir.value_binds[bind].expr,
                env: Env {
                    chain: chain.clone(),
                    here: Some(here),
                    below: Below::Missing,
                    outer_below: Below::Missing,
                    vbinds: vbinds.clone(),
                },
            });
        }

        for &spread in &block.spreads {
            let spread_env = Env {
                chain: chain.clone(),
                here: location.clone(),
                below: env.below.clone(),
                outer_below: Below::Missing,
                vbinds: vbinds.clone(),
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
            self.copy_entries(source, &mut keyed, &chain, &location);
        }

        for entry in &block.entries {
            self.entry(entry, &mut keyed, &chain, &location, &outer_below, &vbinds);
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
    ) {
        for (key, slot) in &source.slots {
            let here = child_location(location, Step::Key(key.clone()));
            let value = self.instantiate(&slot.recipe, &slot.value, chain, here);
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

    fn entry(
        &mut self,
        entry: &Entry,
        keyed: &mut KeyedShape,
        chain: &Chain,
        location: &Option<Location>,
        outer_below: &Below,
        vbinds: &VBinds,
    ) {
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
            let value = self.instantiate(&recipe, &slot.value, chain, here);
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
                let value = self.instantiate(&recipe, &placeholder, chain, here);
                keyed.slots.insert(key.clone(), Slot { value, recipe });
                keyed.defaulted.remove(key);
            }
        }
    }

    /// The span of a value that is already computed, for related spans.
    fn span_of(&self, cell: &Lazy) -> Option<Span> {
        match &*cell.state.borrow() {
            State::Done(Ok(shaped)) => Some(shaped.span),
            State::Pending(Job::Expr { expr, .. }) => Some(self.ir.exprs[*expr].span),
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
        };
        let here = child_location(&here, step);
        let symbol = recipe.op.map_or("-", SetOp::symbol);
        if rest.is_empty() {
            return self.apply_last(value, selector, recipe, chain, here);
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
        let new_child = self.apply_path(&child, rest, recipe, chain, here)?;
        Ok(self.with_child(value, selector, Cell::done(new_child)))
    }

    fn apply_last(
        &mut self,
        value: &Rc<Shaped>,
        selector: &Selector,
        recipe: &PathRecipe,
        chain: &Chain,
        here: Option<Location>,
    ) -> Eval<Rc<Shaped>> {
        let span = selector.span;
        let new_value = |this: &Self| {
            recipe.value.as_ref().map(|entry| {
                let recipe = Recipe::Entry(entry.clone());
                let placeholder = Cell::done(value.clone());
                (
                    this.instantiate(&recipe, &placeholder, chain, here.clone()),
                    recipe,
                )
            })
        };
        match (&value.shape, &selector.kind) {
            (Shape::Keyed(keyed), SelectorKind::Field(_) | SelectorKind::Accessor(_)) => {
                let key = match &selector.kind {
                    SelectorKind::Field(name) => Key::Str(name.to_string()),
                    SelectorKind::Accessor(i) => Key::Int(*i),
                    SelectorKind::Element(_) => unreachable!(),
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
                self.copy_entries(b, &mut keyed, &chain, &env.here);
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
