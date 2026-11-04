//! A typed view of the syntax tree.
//!
//! Every AST type wraps a [`SyntaxNode`] of one kind and gives typed access
//! to its parts. Parts are optional because the tree of a document with
//! syntax errors may lack them.

use crate::SyntaxKind::{self, *};
use crate::{SyntaxNode, SyntaxToken};

/// A typed view of a syntax node.
pub trait AstNode: Sized {
    /// Whether nodes of `kind` can be viewed as this type.
    fn can_cast(kind: SyntaxKind) -> bool;
    /// Views `node` as this type, if it has the right kind.
    fn cast(node: SyntaxNode) -> Option<Self>;
    /// The underlying node.
    fn syntax(&self) -> &SyntaxNode;
}

macro_rules! ast_node {
    ($(#[$meta:meta])* $name:ident, $kind:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $name(SyntaxNode);

        impl AstNode for $name {
            fn can_cast(kind: SyntaxKind) -> bool {
                kind == $kind
            }

            fn cast(node: SyntaxNode) -> Option<Self> {
                Self::can_cast(node.kind()).then(|| Self(node))
            }

            fn syntax(&self) -> &SyntaxNode {
                &self.0
            }
        }
    };
}

macro_rules! ast_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident($ty:ident)),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        #[allow(missing_docs)]
        pub enum $name {
            $($variant($ty),)*
        }

        impl AstNode for $name {
            fn can_cast(kind: SyntaxKind) -> bool {
                $($ty::can_cast(kind))||*
            }

            fn cast(node: SyntaxNode) -> Option<Self> {
                $(if $ty::can_cast(node.kind()) {
                    return Some(Self::$variant($ty(node)));
                })*
                None
            }

            fn syntax(&self) -> &SyntaxNode {
                match self {
                    $(Self::$variant(it) => it.syntax(),)*
                }
            }
        }
    };
}

fn child<N: AstNode>(parent: &SyntaxNode) -> Option<N> {
    parent.children().find_map(N::cast)
}

fn children<N: AstNode>(parent: &SyntaxNode) -> impl Iterator<Item = N> + use<N> {
    parent.children().filter_map(N::cast)
}

fn token(parent: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxToken> {
    parent
        .children_with_tokens()
        .filter_map(|it| it.into_token())
        .find(|it| it.kind() == kind)
}

fn tokens(parent: &SyntaxNode, kind: SyntaxKind) -> impl Iterator<Item = SyntaxToken> + use<> {
    parent
        .children_with_tokens()
        .filter_map(|it| it.into_token())
        .filter(move |it| it.kind() == kind)
}

/// The first token that is not trivia.
fn first_token(parent: &SyntaxNode) -> Option<SyntaxToken> {
    parent
        .children_with_tokens()
        .filter_map(|it| it.into_token())
        .find(|it| !it.kind().is_trivia())
}

// ---- Documents ----

ast_node!(
    /// A whole document: attributes, then entries or one expression.
    Document, DOCUMENT
);

impl Document {
    /// The document attributes, `#![...]`, before the first entry.
    pub fn attributes(&self) -> impl Iterator<Item = Attribute> + use<> {
        children(&self.0)
    }

    /// The entries, if the document is written as entries (`SPEC.md` 4).
    pub fn entries(&self) -> impl Iterator<Item = AnyEntry> + use<> {
        children(&self.0)
    }

    /// The expression, if the document is one expression.
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }
}

ast_node!(
    /// `#![...]`
    Attribute, ATTRIBUTE
);

ast_enum!(
    /// What a document attribute is.
    AttributeKind {
        Use(UseAttr),
        Schema(SchemaAttr),
        Type(TypeAttr),
        Extends(ExtendsAttr),
    }
);

impl Attribute {
    /// The attribute inside the brackets, if it is a known one.
    pub fn kind(&self) -> Option<AttributeKind> {
        child(&self.0)
    }
}

ast_node!(
    /// `use(a::B, c::D as E)`
    UseAttr, USE_ATTR
);

impl UseAttr {
    /// The paths in the attribute.
    pub fn paths(&self) -> impl Iterator<Item = UsePath> + use<> {
        children(&self.0)
    }
}

ast_node!(
    /// `app::net::Config`, `app::Pair<u32, String> as Pair`
    UsePath, USE_PATH
);

impl UsePath {
    /// The segments of the path; the last is the type's name.
    pub fn segments(&self) -> impl Iterator<Item = SyntaxToken> + use<> {
        tokens(&self.0, IDENT)
    }

    /// The type arguments, for a generic type.
    pub fn type_args(&self) -> Option<TypeArgList> {
        child(&self.0)
    }

    /// The alias after `as`.
    pub fn alias(&self) -> Option<SyntaxToken> {
        child::<UseAlias>(&self.0).and_then(|alias| token(&alias.0, IDENT))
    }
}

ast_node!(
    /// `as Alias`
    UseAlias, USE_ALIAS
);

ast_node!(
    /// `<A, B>`
    TypeArgList, TYPE_ARG_LIST
);

impl TypeArgList {
    /// The type arguments.
    pub fn args(&self) -> impl Iterator<Item = TypeExpr> + use<> {
        children(&self.0)
    }
}

ast_node!(
    /// A type expression: `u32`, `List(String)`, `app::Pair<A, B>`.
    TypeExpr, TYPE_EXPR
);

impl TypeExpr {
    /// The segments of the type's path.
    pub fn segments(&self) -> impl Iterator<Item = SyntaxToken> + use<> {
        tokens(&self.0, IDENT)
    }
}

ast_node!(
    /// `schema(::crol::schema)` or `schema("https://...")`
    SchemaAttr, SCHEMA_ATTR
);

impl SchemaAttr {
    /// The schema's module path, for a file.
    pub fn module_path(&self) -> Option<ModulePath> {
        child(&self.0)
    }

    /// The string, for a URL or a crate.
    pub fn string(&self) -> Option<SyntaxToken> {
        token(&self.0, STRING).or_else(|| token(&self.0, RAW_STRING))
    }
}

ast_node!(
    /// `type(Name)`
    TypeAttr, TYPE_ATTR
);

impl TypeAttr {
    /// The type name.
    pub fn name(&self) -> Option<SyntaxToken> {
        token(&self.0, IDENT)
    }
}

ast_node!(
    /// `extends(super::defaults)`
    ExtendsAttr, EXTENDS_ATTR
);

impl ExtendsAttr {
    /// The module path of the extended documents.
    pub fn module_path(&self) -> Option<ModulePath> {
        child(&self.0)
    }
}

ast_node!(
    /// A module path (`SPEC.md` 4.2): `::a::b`, `super::a`, `a::b`.
    ModulePath, MODULE_PATH
);

/// Where a module path starts (`SPEC.md` 4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModulePathStart {
    /// `::a`: the root directory.
    Root,
    /// `a`: the directory of the document.
    Here,
    /// `super::a`, `super::super::a`, ...: that many directories up.
    Super(usize),
}

impl ModulePath {
    /// Where the path starts.
    pub fn start(&self) -> ModulePathStart {
        if first_token(&self.0).is_some_and(|t| t.kind() == COLON2) {
            return ModulePathStart::Root;
        }
        match tokens(&self.0, SUPER_KW).count() {
            0 => ModulePathStart::Here,
            n => ModulePathStart::Super(n),
        }
    }

    /// The names after the start; every one but the last is a directory.
    pub fn segments(&self) -> impl Iterator<Item = SyntaxToken> + use<> {
        tokens(&self.0, IDENT)
    }
}

// ---- Entries ----

ast_enum!(
    /// Any entry of a document, block or variant block (`SPEC.md` 5).
    AnyEntry {
        Entry(Entry),
        Remove(RemoveEntry),
        ValueBind(ValueBind),
        VariantBlock(VariantBlock),
        Spread(Spread),
        Defaults(Defaults),
    }
);

ast_node!(
    /// `target: expr`, `+target: expr`, `?target: expr` or `^target: expr`,
    /// optionally with a bind before it.
    Entry, ENTRY
);

/// The operation of an [`Entry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryOp {
    /// `target: expr`
    Set,
    /// `+target: expr`
    Declare,
    /// `?target: expr`
    Ensure,
    /// `^target: expr`: a set that never merges (`SPEC.md` 5.2).
    Replace,
}

impl Entry {
    /// The bind, `name =`, before the entry.
    pub fn bind(&self) -> Option<Bind> {
        child(&self.0)
    }

    /// The entry's operation.
    pub fn op(&self) -> EntryOp {
        match self.op_token().map(|t| t.kind()) {
            Some(PLUS) => EntryOp::Declare,
            Some(QUESTION) => EntryOp::Ensure,
            Some(CARET) => EntryOp::Replace,
            _ => EntryOp::Set,
        }
    }

    /// The `+`, `?` or `^` token, if any.
    pub fn op_token(&self) -> Option<SyntaxToken> {
        let mut found: Option<SyntaxToken> = None;
        for token in self
            .0
            .children_with_tokens()
            .filter_map(|it| it.into_token())
        {
            let rank = |kind| [CARET, QUESTION, PLUS].iter().position(|k| *k == kind);
            if let Some(new) = rank(token.kind())
                && found.as_ref().is_none_or(|f| rank(f.kind()) < Some(new))
            {
                found = Some(token);
            }
        }
        found
    }

    /// The target.
    pub fn target(&self) -> Option<Target> {
        child(&self.0)
    }

    /// The value.
    pub fn value(&self) -> Option<Expr> {
        child(&self.0)
    }
}

ast_node!(
    /// `name =`, before an entry (`SPEC.md` 6.1.1).
    Bind, BIND
);

impl Bind {
    /// The bind's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        token(&self.0, IDENT)
    }
}

ast_node!(
    /// `-target`
    RemoveEntry, REMOVE_ENTRY
);

impl RemoveEntry {
    /// A bind before the entry, which is an error.
    pub fn bind(&self) -> Option<Bind> {
        child(&self.0)
    }

    /// The target to remove.
    pub fn target(&self) -> Option<Target> {
        child(&self.0)
    }
}

ast_node!(
    /// `name := expr` (`SPEC.md` 6.1.2)
    ValueBind, VALUE_BIND
);

impl ValueBind {
    /// The bound name.
    pub fn name(&self) -> Option<SyntaxToken> {
        token(&self.0, IDENT)
    }

    /// The bound value.
    pub fn value(&self) -> Option<Expr> {
        child(&self.0)
    }
}

ast_node!(
    /// `<c> { entries }` (`SPEC.md` 8.2)
    VariantBlock, VARIANT_BLOCK
);

impl VariantBlock {
    /// A bind before the block, which is an error.
    pub fn bind(&self) -> Option<Bind> {
        child(&self.0)
    }

    /// The condition.
    pub fn variant(&self) -> Option<Variant> {
        child(&self.0)
    }

    /// The entries inside the braces.
    pub fn entries(&self) -> impl Iterator<Item = AnyEntry> + use<> {
        children(&self.0)
    }
}

ast_node!(
    /// `..operand` (`SPEC.md` 5.2)
    Spread, SPREAD
);

impl Spread {
    /// A bind before the spread, which is an error.
    pub fn bind(&self) -> Option<Bind> {
        child(&self.0)
    }

    /// The value spread in.
    pub fn operand(&self) -> Option<Expr> {
        child(&self.0)
    }
}

ast_node!(
    /// `..`: fields left out take their defaults (`SPEC.md` 10.3).
    Defaults, DEFAULTS
);

impl Defaults {
    /// A bind before the marker, which is an error.
    pub fn bind(&self) -> Option<Bind> {
        child(&self.0)
    }
}

// ---- Targets and selectors ----

ast_node!(
    /// A key followed by selectors (`SPEC.md` 5.1).
    Target, TARGET
);

impl Target {
    /// The key.
    pub fn key(&self) -> Option<Key> {
        child(&self.0)
    }

    /// The selectors after the key.
    pub fn selectors(&self) -> impl Iterator<Item = Selector> + use<> {
        children(&self.0)
    }
}

ast_node!(
    /// The key a target starts with: an identifier, a string, an integer or
    /// an accessor.
    Key, KEY
);

impl Key {
    /// The identifier, string or integer token, if the key is not an
    /// accessor.
    pub fn token(&self) -> Option<SyntaxToken> {
        first_token(&self.0)
    }

    /// The accessor, for a key written `[...]`.
    pub fn accessor(&self) -> Option<Accessor> {
        child(&self.0)
    }
}

ast_enum!(
    /// A selector in a target or after an expression.
    Selector {
        Field(FieldSelector),
        Index(IndexSelector),
        Accessor(Accessor),
        Variant(Variant),
        Parent(ParentSelector),
    }
);

ast_node!(
    /// `.name`
    FieldSelector, FIELD_SELECTOR
);

impl FieldSelector {
    /// The field's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        token(&self.0, IDENT)
    }
}

ast_node!(
    /// `.0`
    IndexSelector, INDEX_SELECTOR
);

impl IndexSelector {
    /// The index token.
    pub fn index(&self) -> Option<SyntaxToken> {
        token(&self.0, INT)
    }
}

ast_node!(
    /// `[key]`
    Accessor, ACCESSOR
);

impl Accessor {
    /// The string or integer inside the brackets.
    pub fn key(&self) -> Option<SyntaxToken> {
        [STRING, RAW_STRING, INT]
            .into_iter()
            .find_map(|kind| token(&self.0, kind))
    }

    /// Whether the integer inside has a `-`.
    pub fn is_negative(&self) -> bool {
        token(&self.0, MINUS).is_some()
    }
}

ast_node!(
    /// `.&parent` where it is not allowed; always an error.
    ParentSelector, PARENT_SELECTOR
);

// ---- Variants ----

ast_node!(
    /// `<condition + condition>` (`SPEC.md` 8.2, 8.4)
    Variant, VARIANT
);

impl Variant {
    /// The conditions of the `+` list; usually just one.
    pub fn conditions(&self) -> impl Iterator<Item = Condition> + use<> {
        children(&self.0)
    }
}

ast_enum!(
    /// A variant condition.
    Condition {
        Name(CondName),
        Not(CondNot),
        Binary(CondBinary),
        Paren(CondParen),
    }
);

ast_node!(
    /// A variant name.
    CondName, COND_NAME
);

impl CondName {
    /// The name.
    pub fn name(&self) -> Option<SyntaxToken> {
        token(&self.0, IDENT)
    }
}

ast_node!(
    /// `!c`
    CondNot, COND_NOT
);

impl CondNot {
    /// The negated condition.
    pub fn operand(&self) -> Option<Condition> {
        child(&self.0)
    }
}

ast_node!(
    /// `a & b`, `a | b` or `a ^ b`
    CondBinary, COND_BINARY
);

/// The operator of a [`CondBinary`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CondOp {
    /// `&`: both hold.
    And,
    /// `|`: either holds.
    Or,
    /// `^`: exactly one holds.
    Xor,
}

impl CondBinary {
    /// The left operand.
    pub fn lhs(&self) -> Option<Condition> {
        children(&self.0).next()
    }

    /// The right operand.
    pub fn rhs(&self) -> Option<Condition> {
        children(&self.0).nth(1)
    }

    /// The operator.
    pub fn op(&self) -> Option<CondOp> {
        self.0
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .find_map(|t| match t.kind() {
                AMP => Some(CondOp::And),
                PIPE => Some(CondOp::Or),
                CARET => Some(CondOp::Xor),
                _ => None,
            })
    }
}

ast_node!(
    /// `(c)`
    CondParen, COND_PAREN
);

impl CondParen {
    /// The condition inside.
    pub fn inner(&self) -> Option<Condition> {
        child(&self.0)
    }
}

// ---- Expressions ----

ast_enum!(
    /// An expression (`SPEC.md` 6).
    Expr {
        Literal(Literal),
        Id(IdExpr),
        Reference(Reference),
        Block(Block),
        List(ListExpr),
        Tuple(TupleExpr),
        NamedTuple(NamedTuple),
        MacroCall(MacroCall),
        UnitName(UnitName),
        Paren(ParenExpr),
        Path(PathExpr),
        Prefix(PrefixExpr),
        Binary(BinaryExpr),
    }
);

ast_node!(
    /// A string, char, integer, float or boolean literal.
    Literal, LITERAL
);

impl Literal {
    /// The literal's token.
    pub fn token(&self) -> Option<SyntaxToken> {
        first_token(&self.0)
    }
}

ast_node!(
    /// `@name` or `@"text"` (`SPEC.md` 3.2)
    IdExpr, ID_EXPR
);

impl IdExpr {
    /// The identifier or string after `@`.
    pub fn name(&self) -> Option<SyntaxToken> {
        [IDENT, STRING, RAW_STRING]
            .into_iter()
            .find_map(|kind| token(&self.0, kind))
    }
}

ast_node!(
    /// `&`, `&name`, `&self`, `&parent`, `&root` or `&default` (`SPEC.md` 6.1)
    Reference, REFERENCE
);

/// What a [`Reference`] refers to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ReferenceKind {
    /// `&`: the value below.
    Below,
    /// `&self`
    SelfValue,
    /// `&parent`, `&parent.&parent`, ...: the number of `parent`s.
    Parent(usize),
    /// `&root`
    Root,
    /// `&default`
    Default,
    /// `&name`: a bind or template.
    Bind(SyntaxToken),
}

impl Reference {
    /// What the reference refers to.
    pub fn kind(&self) -> ReferenceKind {
        let parents = tokens(&self.0, PARENT_KW).count();
        if parents > 0 {
            return ReferenceKind::Parent(parents);
        }
        let name = self
            .0
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .find(|t| matches!(t.kind(), IDENT | SELF_KW | ROOT_KW | DEFAULT_KW));
        match name {
            None => ReferenceKind::Below,
            Some(t) => match t.kind() {
                SELF_KW => ReferenceKind::SelfValue,
                ROOT_KW => ReferenceKind::Root,
                DEFAULT_KW => ReferenceKind::Default,
                _ => ReferenceKind::Bind(t),
            },
        }
    }
}

ast_node!(
    /// `Name? { entries }` (`SPEC.md` 5.2)
    Block, BLOCK
);

impl Block {
    /// The type name before the braces.
    pub fn type_name(&self) -> Option<SyntaxToken> {
        token(&self.0, IDENT)
    }

    /// The entries inside the braces.
    pub fn entries(&self) -> impl Iterator<Item = AnyEntry> + use<> {
        children(&self.0)
    }
}

ast_node!(
    /// `[a, b]`
    ListExpr, LIST_EXPR
);

impl ListExpr {
    /// The elements.
    pub fn elements(&self) -> impl Iterator<Item = Expr> + use<> {
        children(&self.0)
    }
}

ast_node!(
    /// `()`, `(a,)`, `(a, b)`
    TupleExpr, TUPLE_EXPR
);

impl TupleExpr {
    /// The elements; none for the unit value `()`.
    pub fn elements(&self) -> impl Iterator<Item = Expr> + use<> {
        children(&self.0)
    }
}

ast_node!(
    /// `round!(a)`
    MacroCall, MACRO_CALL
);

impl MacroCall {
    /// The macro's name.
    pub fn name(&self) -> Option<SyntaxToken> {
        token(&self.0, IDENT)
    }

    /// The arguments.
    pub fn args(&self) -> impl Iterator<Item = Expr> + use<> {
        child::<ArgList>(&self.0)
            .into_iter()
            .flat_map(|args| children::<Expr>(&args.0))
    }
}

ast_node!(
    /// `Name(a, b)`
    NamedTuple, NAMED_TUPLE
);

impl NamedTuple {
    /// The name.
    pub fn name(&self) -> Option<SyntaxToken> {
        token(&self.0, IDENT)
    }

    /// The elements of the payload.
    pub fn elements(&self) -> impl Iterator<Item = Expr> + use<> {
        child::<ArgList>(&self.0)
            .into_iter()
            .flat_map(|args| children::<Expr>(&args.0))
    }
}

ast_node!(
    /// The parenthesised payload of a [`NamedTuple`] or [`MacroCall`].
    ArgList, ARG_LIST
);

ast_node!(
    /// `None`, `Red`: a unit variant or unit struct.
    UnitName, UNIT_NAME
);

impl UnitName {
    /// The name.
    pub fn name(&self) -> Option<SyntaxToken> {
        token(&self.0, IDENT)
    }
}

ast_node!(
    /// `(expr)`
    ParenExpr, PAREN_EXPR
);

impl ParenExpr {
    /// The expression inside.
    pub fn inner(&self) -> Option<Expr> {
        child(&self.0)
    }
}

ast_node!(
    /// An expression followed by selectors: `&root.web.port`, `[1, 2][0]`.
    PathExpr, PATH_EXPR
);

impl PathExpr {
    /// The expression the selectors apply to.
    pub fn base(&self) -> Option<Expr> {
        child(&self.0)
    }

    /// The selectors.
    pub fn selectors(&self) -> impl Iterator<Item = Selector> + use<> {
        children(&self.0)
    }
}

ast_node!(
    /// `-expr`
    PrefixExpr, PREFIX_EXPR
);

impl PrefixExpr {
    /// The negated expression.
    pub fn operand(&self) -> Option<Expr> {
        child(&self.0)
    }
}

ast_node!(
    /// `a op b`
    BinaryExpr, BINARY_EXPR
);

/// The operator of a [`BinaryExpr`] (`SPEC.md` 6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(missing_docs)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    /// `~`
    Merge,
    /// `~^`
    MergeShallow,
    /// `|`
    Fallback,
}

impl BinaryExpr {
    /// The left operand.
    pub fn lhs(&self) -> Option<Expr> {
        children(&self.0).next()
    }

    /// The right operand.
    pub fn rhs(&self) -> Option<Expr> {
        children(&self.0).nth(1)
    }

    /// The operator's token.
    pub fn op_token(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .find(|t| !t.kind().is_trivia())
    }

    /// The operator.
    pub fn op(&self) -> Option<BinaryOp> {
        Some(match self.op_token()?.kind() {
            PLUS => BinaryOp::Add,
            MINUS => BinaryOp::Sub,
            STAR => BinaryOp::Mul,
            SLASH => BinaryOp::Div,
            PERCENT => BinaryOp::Rem,
            TILDE => BinaryOp::Merge,
            TILDE_CARET => BinaryOp::MergeShallow,
            PIPE => BinaryOp::Fallback,
            _ => return None,
        })
    }
}
