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
    /// `&default`
    Default,
    /// `&name` for an entry bind: the bound target, `up` blocks out.
    Bind { up: usize, target: Vec<Selector> },
    /// `&name` for a value bind or template.
    ValueBind(BindId),
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
