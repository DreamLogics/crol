//! The evaluator's representation of a document: expressions and blocks in
//! arenas, with names resolved.

use std::rc::Rc;

use crate::source::Span;
use crate::value::Key;

pub(crate) type ExprId = usize;
pub(crate) type BlockId = usize;
pub(crate) type BindId = usize;

#[derive(Debug, Default)]
pub(crate) struct Ir {
    pub(crate) exprs: Vec<Expr>,
    pub(crate) blocks: Vec<Block>,
    pub(crate) value_binds: Vec<ValueBind>,
}

impl Ir {
    pub(crate) fn add_expr(&mut self, kind: ExprKind, span: Span) -> ExprId {
        self.exprs.push(Expr { kind, span });
        self.exprs.len() - 1
    }
}

#[derive(Debug)]
pub(crate) struct Expr {
    pub(crate) kind: ExprKind,
    pub(crate) span: Span,
}

#[derive(Debug)]
pub(crate) enum ExprKind {
    Unit,
    Bool(bool),
    Int(i128),
    Float(f64),
    Char(char),
    Str(Rc<str>),
    Id(Rc<str>),
    Ref(RefKind),
    Block(BlockId),
    List(Vec<ExprId>),
    Tuple(Vec<ExprId>),
    Named(Rc<str>, Vec<ExprId>),
    UnitName(Rc<str>),
    Path(ExprId, Vec<Selector>),
    Neg(ExprId),
    Binary(BinOp, ExprId, ExprId),
    /// An expression that failed to lower; the error is already reported.
    Error,
}

#[derive(Debug, Clone)]
pub(crate) enum RefKind {
    /// `&`
    Below,
    /// `&self`
    SelfValue,
    /// `&parent`, `&parent.&parent`: how many blocks up.
    Parent(usize),
    /// `&root`
    Root,
    /// `&default`, with the target whose default it is.
    Default(DefaultTarget),
    /// `&name` for an entry bind: the bound target, `up` blocks out.
    Bind { up: usize, target: Vec<Selector> },
    /// `&name` for a value bind or template.
    ValueBind(BindId),
}

/// Whose default `&default` is (`SPEC.md` 6.1.3): a field path from the
/// nearest enclosing type name, or with no type name, from the root type.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DefaultTarget {
    pub(crate) type_name: Option<Rc<str>>,
    /// The root key of a data-mode document the path starts below, with no
    /// type name: a step of the path where the root type is the whole
    /// document (`SPEC.md` 4.1).
    pub(crate) root_key: Option<Rc<str>>,
    pub(crate) path: Vec<Rc<str>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Merge,
    Fallback,
}

impl BinOp {
    pub(crate) fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::Merge => "~",
            BinOp::Fallback => "|",
        }
    }
}

/// A selector in a path (`SPEC.md` 5.1).
#[derive(Debug, Clone)]
pub(crate) struct Selector {
    pub(crate) kind: SelectorKind,
    pub(crate) span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SelectorKind {
    /// `.name` or `["name"]`: a string key.
    Field(Rc<str>),
    /// `.0`: a tuple element.
    Element(usize),
    /// `[3]`, `[-3]`: an integer key, or on a list an index.
    Accessor(i128),
    /// `<a & !b>` in a reference: the context to read it in (`SPEC.md`
    /// 8.4), as variants made active or inactive.
    Context(Vec<(Rc<str>, bool, Span)>),
}

#[derive(Debug)]
pub(crate) struct Block {
    pub(crate) name: Option<Rc<str>>,
    pub(crate) spreads: Vec<ExprId>,
    pub(crate) entries: Vec<Entry>,
    pub(crate) defaults: bool,
    pub(crate) value_binds: Vec<BindId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SetOp {
    /// `target: expr`
    Set,
    /// `+target: expr`
    Declare,
    /// `?target: expr`
    Ensure,
}

impl SetOp {
    pub(crate) fn symbol(self) -> &'static str {
        match self {
            SetOp::Set => ":",
            SetOp::Declare => "+",
            SetOp::Ensure => "?",
        }
    }
}

#[derive(Debug)]
pub(crate) struct Entry {
    /// `None` for a remove.
    pub(crate) op: Option<SetOp>,
    pub(crate) key: Key,
    pub(crate) path: Vec<Selector>,
    /// The value; `None` for a remove.
    pub(crate) value: Option<ExprId>,
    /// The target's span.
    pub(crate) span: Span,
}

#[derive(Debug)]
pub(crate) struct ValueBind {
    pub(crate) name: Rc<str>,
    pub(crate) expr: ExprId,
}

/// An operation on a root key, from a data-mode document (`SPEC.md` 7.2).
#[derive(Debug)]
pub(crate) struct RootOp {
    pub(crate) key: Rc<str>,
    /// `None` for a remove.
    pub(crate) op: Option<SetOp>,
    pub(crate) path: Vec<Selector>,
    pub(crate) value: Option<ExprId>,
    /// The target's span.
    pub(crate) span: Span,
    /// Whether the value uses the value below (`&`), which makes a set a
    /// patch rather than a definition.
    pub(crate) uses_below: bool,
    pub(crate) layer: usize,
    pub(crate) document: usize,
    /// The condition, for a variant operation (`SPEC.md` 8.2).
    pub(crate) condition: Option<crate::cond::Cond>,
    /// For an operation lifted out of a block, the paths from the root key
    /// of the blocks around it, innermost first, so that `&self`, `&parent`
    /// and binds see them. Empty for a root entry.
    pub(crate) blocks: Vec<Vec<Selector>>,
}

/// A `..` at the root of a data-mode document (`SPEC.md` 4), under a
/// condition if it is in a variant block.
#[derive(Debug)]
pub(crate) struct RootDefaults {
    pub(crate) condition: Option<crate::cond::Cond>,
    pub(crate) span: Span,
}

impl RootOp {
    /// Whether this is a definition (`SPEC.md` 7.2): a root entry without a
    /// path that declares, ensures, or sets without using the value below.
    pub(crate) fn is_definition(&self) -> bool {
        self.path.is_empty()
            && match self.op {
                Some(SetOp::Declare | SetOp::Ensure) => true,
                Some(SetOp::Set) => !self.uses_below,
                None => false,
            }
    }

    /// What the operation is, as `crol explain` shows it.
    pub(crate) fn kind(&self) -> crate::StepKind {
        if self.op.is_none() {
            crate::StepKind::Remove
        } else if self.is_definition() {
            crate::StepKind::Definition
        } else {
            crate::StepKind::Patch
        }
    }
}
