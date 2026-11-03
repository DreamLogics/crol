//! The kinds of tokens and nodes in the syntax tree.

/// The kind of a token or node.
///
/// Tokens come first, then nodes. The names of nodes follow the grammar of
/// `SPEC.md` section 9.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
#[allow(non_camel_case_types, missing_docs)]
pub enum SyntaxKind {
    // Trivia.
    WHITESPACE = 0,
    /// A leading byte order mark (`SPEC.md` 2.1).
    BOM,
    LINE_COMMENT,
    BLOCK_COMMENT,

    // Literals.
    IDENT,
    INT,
    FLOAT,
    STRING,
    RAW_STRING,
    CHAR,

    // Reserved words (`SPEC.md` 2.3).
    TRUE_KW,
    FALSE_KW,
    // Contextual keywords: the lexer produces `IDENT`, the parser remaps it.
    SELF_KW,
    PARENT_KW,
    ROOT_KW,
    DEFAULT_KW,
    USE_KW,
    AS_KW,
    SCHEMA_KW,
    TYPE_KW,
    EXTENDS_KW,
    SUPER_KW,

    // Punctuation (`SPEC.md` 2.5).
    L_BRACE,
    R_BRACE,
    L_BRACK,
    R_BRACK,
    L_PAREN,
    R_PAREN,
    L_ANGLE,
    R_ANGLE,
    COMMA,
    COLON,
    COLON2,
    COLON_EQ,
    DOT,
    DOT2,
    PLUS,
    MINUS,
    STAR,
    SLASH,
    PERCENT,
    TILDE,
    PIPE,
    AMP,
    CARET,
    AT,
    QUESTION,
    EQ,
    POUND,
    BANG,

    /// Text the lexer could not make into a token.
    ERROR_TOKEN,

    // Nodes.
    /// A whole document.
    DOCUMENT,
    /// `#![...]`
    ATTRIBUTE,
    /// `use(a::B, c::D as E)`
    USE_ATTR,
    /// `schema(...)`
    SCHEMA_ATTR,
    /// `type(Name)`
    TYPE_ATTR,
    /// `extends(...)`
    EXTENDS_ATTR,
    /// `::a::b`, `super::a`, `a::b`
    MODULE_PATH,
    /// `app::net::Config<u32> as Alias`
    USE_PATH,
    /// `as Alias`
    USE_ALIAS,
    /// `<A, B>` in a use path or type expression.
    TYPE_ARG_LIST,
    /// A type expression: `u32`, `List(String)`, `app::Pair<A, B>`.
    TYPE_EXPR,
    /// `(A, B)` after a type expression's name.
    TYPE_PAREN_LIST,

    /// `target: expr`, `+target: expr` or `?target: expr`, optionally bound.
    ENTRY,
    /// `-target`
    REMOVE_ENTRY,
    /// `name := expr`
    VALUE_BIND,
    /// `<c> { entries }`
    VARIANT_BLOCK,
    /// `..operand`
    SPREAD,
    /// `..`
    DEFAULTS,
    /// `name =` before an entry.
    BIND,

    /// A key followed by selectors.
    TARGET,
    /// The first part of a target: an identifier, string, integer or accessor.
    KEY,
    /// `.name`
    FIELD_SELECTOR,
    /// `.0`
    INDEX_SELECTOR,
    /// `[key]`
    ACCESSOR,
    /// `<condition + condition>` after a target or expression.
    VARIANT,
    /// `.&parent` in a position where it is not allowed.
    PARENT_SELECTOR,

    /// A variant name in a condition.
    COND_NAME,
    /// `!c`
    COND_NOT,
    /// `a & b`, `a | b`, `a ^ b`
    COND_BINARY,
    /// `(c)`
    COND_PAREN,

    /// A string, char, integer, float or boolean literal.
    LITERAL,
    /// `@name` or `@"text"`
    ID_EXPR,
    /// `&`, `&name`, `&self`, `&parent.&parent`, `&root`, `&default`
    REFERENCE,
    /// `Name? { entries }`
    BLOCK,
    /// `[a, b]`
    LIST_EXPR,
    /// `()`, `(a,)`, `(a, b)`
    TUPLE_EXPR,
    /// `Name(a, b)`
    NAMED_TUPLE,
    /// `(a, b)` after a named tuple's name.
    ARG_LIST,
    /// `None`, `Red`
    UNIT_NAME,
    /// `(expr)`
    PAREN_EXPR,
    /// An expression followed by selectors: `&root.web.port`, `[1, 2][0]`.
    PATH_EXPR,
    /// `-expr`
    PREFIX_EXPR,
    /// `a + b`, `a ~ b`, `a | b`, ...
    BINARY_EXPR,

    /// Tokens the parser skipped while recovering from an error.
    ERROR,

    #[doc(hidden)]
    __LAST,
}

use SyntaxKind::*;

impl SyntaxKind {
    /// Whitespace, comments and the byte order mark.
    pub fn is_trivia(self) -> bool {
        matches!(self, WHITESPACE | BOM | LINE_COMMENT | BLOCK_COMMENT)
    }

    /// Reserved words and contextual keywords.
    pub fn is_keyword(self) -> bool {
        (TRUE_KW as u16..=SUPER_KW as u16).contains(&(self as u16))
    }

    /// Tokens, as opposed to nodes.
    pub fn is_token(self) -> bool {
        (self as u16) <= ERROR_TOKEN as u16
    }

    /// The contextual keyword spelled `text` that may follow `&`.
    pub(crate) fn reference_keyword(text: &str) -> Option<SyntaxKind> {
        Some(match text {
            "self" => SELF_KW,
            "parent" => PARENT_KW,
            "root" => ROOT_KW,
            "default" => DEFAULT_KW,
            _ => return None,
        })
    }
}

impl From<SyntaxKind> for rowan::SyntaxKind {
    fn from(kind: SyntaxKind) -> Self {
        rowan::SyntaxKind(kind as u16)
    }
}

/// The rowan language of CROL syntax trees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CrolLanguage {}

impl rowan::Language for CrolLanguage {
    type Kind = SyntaxKind;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> SyntaxKind {
        assert!(raw.0 < __LAST as u16, "invalid syntax kind {}", raw.0);
        // Kinds are dense from 0 to `__LAST`; map through a table rather than
        // transmuting, since the crate forbids unsafe code.
        KINDS[raw.0 as usize]
    }

    fn kind_to_raw(kind: SyntaxKind) -> rowan::SyntaxKind {
        kind.into()
    }
}

const KINDS: [SyntaxKind; __LAST as usize] = [
    WHITESPACE,
    BOM,
    LINE_COMMENT,
    BLOCK_COMMENT,
    IDENT,
    INT,
    FLOAT,
    STRING,
    RAW_STRING,
    CHAR,
    TRUE_KW,
    FALSE_KW,
    SELF_KW,
    PARENT_KW,
    ROOT_KW,
    DEFAULT_KW,
    USE_KW,
    AS_KW,
    SCHEMA_KW,
    TYPE_KW,
    EXTENDS_KW,
    SUPER_KW,
    L_BRACE,
    R_BRACE,
    L_BRACK,
    R_BRACK,
    L_PAREN,
    R_PAREN,
    L_ANGLE,
    R_ANGLE,
    COMMA,
    COLON,
    COLON2,
    COLON_EQ,
    DOT,
    DOT2,
    PLUS,
    MINUS,
    STAR,
    SLASH,
    PERCENT,
    TILDE,
    PIPE,
    AMP,
    CARET,
    AT,
    QUESTION,
    EQ,
    POUND,
    BANG,
    ERROR_TOKEN,
    DOCUMENT,
    ATTRIBUTE,
    USE_ATTR,
    SCHEMA_ATTR,
    TYPE_ATTR,
    EXTENDS_ATTR,
    MODULE_PATH,
    USE_PATH,
    USE_ALIAS,
    TYPE_ARG_LIST,
    TYPE_EXPR,
    TYPE_PAREN_LIST,
    ENTRY,
    REMOVE_ENTRY,
    VALUE_BIND,
    VARIANT_BLOCK,
    SPREAD,
    DEFAULTS,
    BIND,
    TARGET,
    KEY,
    FIELD_SELECTOR,
    INDEX_SELECTOR,
    ACCESSOR,
    VARIANT,
    PARENT_SELECTOR,
    COND_NAME,
    COND_NOT,
    COND_BINARY,
    COND_PAREN,
    LITERAL,
    ID_EXPR,
    REFERENCE,
    BLOCK,
    LIST_EXPR,
    TUPLE_EXPR,
    NAMED_TUPLE,
    ARG_LIST,
    UNIT_NAME,
    PAREN_EXPR,
    PATH_EXPR,
    PREFIX_EXPR,
    BINARY_EXPR,
    ERROR,
];

/// A node of the CROL syntax tree.
pub type SyntaxNode = rowan::SyntaxNode<CrolLanguage>;
/// A token of the CROL syntax tree.
pub type SyntaxToken = rowan::SyntaxToken<CrolLanguage>;
/// A node or a token.
pub type SyntaxElement = rowan::SyntaxElement<CrolLanguage>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_table_is_dense() {
        for (i, kind) in KINDS.iter().enumerate() {
            assert_eq!(*kind as usize, i, "{kind:?} is out of place");
        }
    }
}
