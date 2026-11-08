//! The evaluator's representation of a document: expressions and blocks in
//! arenas, with names resolved.

use std::sync::Arc;

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

    /// Gives back what the vectors reserved while they grew, as they stay as
    /// they are from here on.
    pub(crate) fn shrink(&mut self) {
        self.exprs.shrink_to_fit();
        self.blocks.shrink_to_fit();
        self.value_binds.shrink_to_fit();
        for block in &mut self.blocks {
            block.entries.shrink_to_fit();
        }
    }
}

#[derive(Debug)]
pub(crate) struct Expr {
    pub(crate) kind: ExprKind,
    pub(crate) span: Span,
}

// Documents hold hundreds of thousands of these: what a rare kind of
// expression needs goes behind a box.
const _: () = assert!(size_of::<Expr>() <= 48);

/// An `i128` aligned as a `u64`, which spares every expression the padding
/// that a 16-byte alignment takes.
#[derive(Debug, Clone, Copy)]
#[repr(Rust, packed(8))]
pub(crate) struct Wide(pub(crate) i128);

#[derive(Debug)]
pub(crate) enum ExprKind {
    Unit,
    Bool(bool),
    Int(Wide),
    Float(f64),
    Char(char),
    Str(Arc<str>),
    Id(Arc<str>),
    Ref(RefKind),
    Block(BlockId),
    List(Vec<ExprId>),
    Tuple(Vec<ExprId>),
    Named(Box<(Arc<str>, Vec<ExprId>)>),
    UnitName(Arc<str>),
    Path(ExprId, Box<[Selector]>),
    Neg(ExprId),
    Not(ExprId),
    Binary(BinOp, ExprId, ExprId),
    Macro(MacroKind, Box<[ExprId]>),
    /// An expression that failed to lower; the error is already reported.
    Error,
}

#[derive(Debug, Clone)]
pub(crate) enum RefKind {
    /// `&`
    Below,
    /// `&self`
    SelfValue,
    /// `&parent`, `&parent.&parent` (or `&super`): how many blocks up.
    Parent(usize),
    /// `&top`: the root entry the innermost block is in.
    Top,
    /// `&root`
    Root,
    /// `&default`, with the target whose default it is.
    Default(Box<DefaultTarget>),
    /// `&name` for an entry bind: the bound target, `up` blocks out.
    Bind { up: usize, target: Box<[Selector]> },
    /// `&name` for a value bind or template.
    ValueBind(BindId),
}

/// Whose default `&default` is (`SPEC.md` 6.1.3): a field path from the
/// nearest enclosing type name, or with no type name, from the root type.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DefaultTarget {
    pub(crate) type_name: Option<Arc<str>>,
    /// The root key of a data-mode document the path starts below, with no
    /// type name: a step of the path where the root type is the whole
    /// document (`SPEC.md` 4.1).
    pub(crate) root_key: Option<Arc<str>>,
    pub(crate) path: Vec<Arc<str>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Merge,
    MergeShallow,
    Union,
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
            BinOp::MergeShallow => "~^",
            BinOp::Union => "|",
            BinOp::Fallback => "??",
        }
    }
}

/// A built-in macro (`SPEC.md` 6.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MacroKind {
    Round,
    Floor,
    Ceil,
    Clamp,
}

impl MacroKind {
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "round" => MacroKind::Round,
            "floor" => MacroKind::Floor,
            "ceil" => MacroKind::Ceil,
            "clamp" => MacroKind::Clamp,
            _ => return None,
        })
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            MacroKind::Round => "round!",
            MacroKind::Floor => "floor!",
            MacroKind::Ceil => "ceil!",
            MacroKind::Clamp => "clamp!",
        }
    }

    /// How many arguments the macro takes.
    pub(crate) fn arity(self) -> usize {
        match self {
            MacroKind::Clamp => 3,
            _ => 1,
        }
    }

    /// How a call is written, with its parameters.
    pub(crate) fn signature(self) -> &'static str {
        match self {
            MacroKind::Round => "round!(x)",
            MacroKind::Floor => "floor!(x)",
            MacroKind::Ceil => "ceil!(x)",
            MacroKind::Clamp => "clamp!(x, min, max)",
        }
    }

    /// What the macro does, in Markdown.
    pub(crate) fn doc(self) -> &'static str {
        match self {
            MacroKind::Round => {
                "`x` to the nearest integer, ties away from zero. An integer \
                 is returned as it is, and a result that does not fit an \
                 integer is an error."
            }
            MacroKind::Floor => {
                "The largest integer not above `x`. An integer is returned \
                 as it is, and a result that does not fit an integer is an \
                 error."
            }
            MacroKind::Ceil => {
                "The smallest integer not below `x`. An integer is returned \
                 as it is, and a result that does not fit an integer is an \
                 error."
            }
            MacroKind::Clamp => {
                "`x`, kept between `min` and `max`. The result is an integer \
                 if all three are, and a float otherwise. `min` above `max` \
                 is an error."
            }
        }
    }
}

/// A selector in a path (`SPEC.md` 5.1).
#[derive(Debug, Clone)]
pub(crate) struct Selector {
    pub(crate) kind: SelectorKind,
    pub(crate) span: Span,
    /// `a.b?.c`: the path goes through this step whatever it holds
    /// (`SPEC.md` 5.1).
    pub(crate) optional: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SelectorKind {
    /// `.name` or `["name"]`: a string key.
    Field(Arc<str>),
    /// `.0`: a tuple element.
    Element(usize),
    /// `[3]`, `[-3]`: an integer key, or on a list an index.
    Accessor(i128),
    /// `<a & !b>` in a reference: the context to read it in (`SPEC.md`
    /// 8.4), as variants made active or inactive.
    Context(Vec<(Arc<str>, bool, Span)>),
}

#[derive(Debug)]
pub(crate) struct Block {
    pub(crate) name: Option<Arc<str>>,
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
    /// `^target: expr`: a set that never merges (`SPEC.md` 5.2).
    pub(crate) replace: bool,
    /// `-?target`: a remove that is no error where the target is not set.
    pub(crate) if_set: bool,
    pub(crate) key: Key,
    /// `a?.b`: the path goes through the key whatever it holds.
    pub(crate) key_optional: bool,
    pub(crate) path: Vec<Selector>,
    /// The value; `None` for a remove.
    pub(crate) value: Option<ExprId>,
    /// The target's span.
    pub(crate) span: Span,
}

#[derive(Debug)]
pub(crate) struct ValueBind {
    pub(crate) name: Arc<str>,
    pub(crate) expr: ExprId,
}

/// An operation on a root key, from a data-mode document (`SPEC.md` 7.2).
#[derive(Debug)]
pub(crate) struct RootOp {
    pub(crate) key: Arc<str>,
    /// `None` for a remove.
    pub(crate) op: Option<SetOp>,
    /// `^target: expr`: a set that never merges (`SPEC.md` 5.2).
    pub(crate) replace: bool,
    /// `-?target`: a remove that is no error where the target is not set.
    pub(crate) if_set: bool,
    /// `a?.b`: the path goes through the key whatever it holds.
    pub(crate) key_optional: bool,
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

/// `#![variant(group: name)]`: a data-mode document adds a variant to an
/// open group (`SPEC.md` 8.1).
#[derive(Debug, Clone)]
pub(crate) struct DeclaredVariant {
    pub(crate) group: Arc<str>,
    pub(crate) name: Arc<str>,
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
