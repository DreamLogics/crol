//! The formatter (`SPEC.md` 12.2).
//!
//! The style:
//!
//! - 4-space indentation and a maximum line width (80 by default);
//! - a block, list or tuple goes on one line if it fits and has no comments,
//!   else one entry per line with a trailing comma; a block written with a
//!   line break after its `{` stays broken;
//! - so does the list of a `#![use(...)]`;
//! - a named tuple or macro call with one argument, a block, list or tuple,
//!   breaks inside that argument: `List([` on one line, `])` on the last;
//! - a binary expression that does not fit breaks after an operator, and what
//!   follows is indented one more level, unless it is a container, which
//!   stays and breaks inside;
//! - top-level entries are separated by line breaks alone, with a comma only
//!   where the line break would not separate them (`SPEC.md` 5.3);
//! - spaces around binary operators and after `:` and `,`, none inside
//!   references, ids and selectors;
//! - blank lines between entries are kept, but at most one;
//! - a `..` marker is moved to the end of its block, and duplicates removed;
//! - the outer braces of a document written as one untyped block are removed;
//! - string keys that are identifiers are written bare (`SPEC.md` 3.1).
//!
//! Comments are kept. A comment between entries or elements keeps its place;
//! an entry with a comment anywhere else (inside an expression, say) is
//! written as it was, since there is no good place to move the comment to.

use rowan::NodeOrToken;

use crate::SyntaxKind::{self, *};
use crate::{SyntaxError, SyntaxNode, SyntaxToken};

/// Options for [`format()`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FormatConfig {
    /// The maximum line width, in Unicode scalar values. Lines can be longer
    /// when there is no place to break them.
    pub max_width: usize,
}

impl Default for FormatConfig {
    fn default() -> Self {
        Self { max_width: 80 }
    }
}

impl FormatConfig {
    /// Sets the maximum line width.
    pub fn with_max_width(mut self, max_width: usize) -> Self {
        self.max_width = max_width;
        self
    }
}

/// Formats a document. A document with syntax errors is not formatted; its
/// errors are returned instead.
///
/// ```
/// use crol_syntax::format::{FormatConfig, format};
///
/// let text = "{ +web:Service{port:80,..,replicas:&+1} }";
/// assert_eq!(
///     format(text, &FormatConfig::default()).unwrap(),
///     "+web: Service { port: 80, replicas: & + 1, .. }\n",
/// );
/// ```
pub fn format(text: &str, config: &FormatConfig) -> Result<String, Vec<SyntaxError>> {
    let parse = crate::parse(text);
    if !parse.errors().is_empty() {
        return Err(parse.errors().to_vec());
    }
    let formatter = Formatter {
        max_width: config.max_width,
    };
    Ok(formatter.document(&parse.syntax()))
}

const INDENT: &str = "    ";

/// Something between the brackets of a container, or at the top level.
#[derive(Debug)]
enum Item {
    /// An entry or element, with a comment that follows it on its line.
    Element(SyntaxNode, Option<String>),
    /// A comment on a line of its own.
    Comment(String),
    /// A blank line.
    Blank,
}

fn is_comment(kind: SyntaxKind) -> bool {
    matches!(kind, LINE_COMMENT | BLOCK_COMMENT)
}

fn is_entry(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        ENTRY | REMOVE_ENTRY | VALUE_BIND | VARIANT_BLOCK | SPREAD | DEFAULTS
    )
}

/// The token that opens a container's items, for nodes that are containers.
fn container_open(kind: SyntaxKind) -> Option<SyntaxKind> {
    match kind {
        BLOCK | VARIANT_BLOCK => Some(L_BRACE),
        LIST_EXPR => Some(L_BRACK),
        TUPLE_EXPR | ARG_LIST => Some(L_PAREN),
        _ => None,
    }
}

fn width(text: &str) -> usize {
    text.chars().count()
}

/// The column after writing `text` at column `col`.
fn col_after(col: usize, text: &str) -> usize {
    match text.rfind('\n') {
        Some(i) => width(&text[i + 1..]),
        None => col + width(text),
    }
}

fn first_line_width(text: &str) -> usize {
    width(text.lines().next().unwrap_or(""))
}

fn significant_tokens(node: &SyntaxNode) -> impl Iterator<Item = SyntaxToken> + use<> {
    node.descendants_with_tokens()
        .filter_map(|it| it.into_token())
        .filter(|t| !t.kind().is_trivia())
}

fn child_token(node: &SyntaxNode, kinds: &[SyntaxKind]) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(|it| it.into_token())
        .find(|t| kinds.contains(&t.kind()))
}

/// Whether a top-level entry needs a comma after it when `next` is the entry
/// that follows: a line break does not separate them (`SPEC.md` 5.3).
fn needs_comma_before(next: &SyntaxNode, formatter: &Formatter) -> bool {
    if has_interior_comment(next) {
        // Written as it was, so its start may not be on one line.
        return significant_tokens(next)
            .next()
            .is_some_and(|t| matches!(t.kind(), PLUS | MINUS | L_ANGLE | L_BRACK));
    }
    next.kind() == REMOVE_ENTRY
        && formatter
            .flat(next)
            .is_some_and(|text| text.starts_with("-[") || text[1..].starts_with(char::is_numeric))
}

/// What goes between a base and its selectors: nothing, except after an
/// integer before `.`, where `1 .0` would become the float `1.0`.
fn separator(base: Option<&SyntaxNode>, selectors: &str) -> &'static str {
    let ends_in_int = base
        .and_then(|b| significant_tokens(b).last())
        .is_some_and(|t| t.kind() == INT);
    if ends_in_int && selectors.starts_with('.') {
        " "
    } else {
        ""
    }
}

fn child_node(node: &SyntaxNode, pred: impl Fn(SyntaxKind) -> bool) -> Option<SyntaxNode> {
    node.children().find(|n| pred(n.kind()))
}

fn is_expr(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        LITERAL
            | ID_EXPR
            | REFERENCE
            | BLOCK
            | LIST_EXPR
            | TUPLE_EXPR
            | NAMED_TUPLE
            | MACRO_CALL
            | UNIT_NAME
            | PAREN_EXPR
            | PATH_EXPR
            | PREFIX_EXPR
            | BINARY_EXPR
    )
}

fn is_selector(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        FIELD_SELECTOR | INDEX_SELECTOR | ACCESSOR | VARIANT | PARENT_SELECTOR
    )
}

/// The identifier a string literal spells, if it is a plain identifier that
/// can be written bare (`SPEC.md` 3.1).
fn string_as_identifier(token: &SyntaxToken) -> Option<&str> {
    let text = token.text();
    let inner = match token.kind() {
        STRING => text.strip_prefix('"')?.strip_suffix('"')?,
        RAW_STRING => {
            let hashes = text[1..].bytes().take_while(|&b| b == b'#').count();
            let quoted = &text[1 + hashes..text.len() - hashes];
            quoted.strip_prefix('"')?.strip_suffix('"')?
        }
        _ => return None,
    };
    let mut chars = inner.chars();
    let first = chars.next()?;
    let is_identifier = (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !matches!(inner, "_" | "true" | "false")
        && !inner.contains('\\');
    is_identifier.then_some(inner)
}

/// Whether `node` holds a container. After a binary operator it stays on the
/// line and can break inside. This must not depend on how the source breaks
/// its blocks, or formatting would not be idempotent.
fn holds_container(node: &SyntaxNode) -> bool {
    node.descendants()
        .any(|n| container_open(n.kind()).is_some())
}

/// Whether `node` is a block or variant block written with a line break
/// right after its `{`, which keeps it broken.
fn broken_in_source(node: &SyntaxNode) -> bool {
    if !matches!(node.kind(), BLOCK | VARIANT_BLOCK) {
        return false;
    }
    node.children_with_tokens()
        .skip_while(|it| it.kind() != L_BRACE)
        .skip(1)
        .take_while(|it| it.kind() == WHITESPACE)
        .any(|it| it.as_token().is_some_and(|t| t.text().contains('\n')))
}

/// Whether `node` holds a comment outside the item positions of the
/// containers in it, which the formatter cannot place.
fn has_interior_comment(node: &SyntaxNode) -> bool {
    let open = container_open(node.kind());
    let mut opened = false;
    for element in node.children_with_tokens() {
        match element {
            NodeOrToken::Token(token) => {
                if !opened && is_comment(token.kind()) {
                    return true;
                }
                if Some(token.kind()) == open {
                    opened = true;
                }
            }
            // Items of a container are checked on their own.
            NodeOrToken::Node(child) => {
                if !opened && has_interior_comment(&child) {
                    return true;
                }
            }
        }
    }
    false
}

/// The items of a container: the children after its opening bracket, or all
/// children of a document.
fn items(node: &SyntaxNode) -> Vec<Item> {
    let open = container_open(node.kind());
    let mut items = Vec::new();
    let mut started = open.is_none();
    let mut pending_blank = false;
    let mut newline_since_element = true;
    for element in node.children_with_tokens() {
        match element {
            NodeOrToken::Token(token) => {
                let kind = token.kind();
                if !started {
                    started = Some(kind) == open;
                    continue;
                }
                match kind {
                    WHITESPACE => {
                        let newlines = token.text().matches('\n').count();
                        newline_since_element |= newlines > 0;
                        pending_blank |= newlines > 1;
                    }
                    LINE_COMMENT | BLOCK_COMMENT => {
                        let text = match kind {
                            LINE_COMMENT => token.text().trim_end().to_owned(),
                            _ => token.text().to_owned(),
                        };
                        if !newline_since_element
                            && let Some(Item::Element(_, trailing @ None)) = items.last_mut()
                        {
                            *trailing = Some(text);
                            continue;
                        }
                        if pending_blank && !items.is_empty() {
                            items.push(Item::Blank);
                        }
                        pending_blank = false;
                        items.push(Item::Comment(text));
                    }
                    _ => {}
                }
            }
            NodeOrToken::Node(child) => {
                if !started {
                    continue;
                }
                if pending_blank && !items.is_empty() {
                    items.push(Item::Blank);
                }
                pending_blank = false;
                newline_since_element = false;
                items.push(Item::Element(child, None));
            }
        }
    }
    items
}

/// Moves the first `..` to after the last element and drops the others.
fn move_defaults(items: Vec<Item>) -> Vec<Item> {
    let mut defaults = None;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match item {
            Item::Element(node, trailing) if node.kind() == DEFAULTS => {
                if defaults.is_none() {
                    // The comment after the kept `..` moves with it.
                    defaults = Some(Item::Element(node, trailing));
                } else if let Some(comment) = trailing {
                    // One after a dropped `..` stays where it was.
                    out.push(Item::Comment(comment));
                }
            }
            item => out.push(item),
        }
    }
    if let Some(defaults) = defaults {
        let at = out
            .iter()
            .rposition(|item| matches!(item, Item::Element(..)))
            .map_or(out.len(), |i| i + 1);
        out.insert(at, defaults);
    }
    out
}

/// Drops blank lines at the start and end, and repeated ones.
fn normalize_blanks(items: Vec<Item>) -> Vec<Item> {
    let mut out: Vec<Item> = Vec::with_capacity(items.len());
    for item in items {
        if matches!(item, Item::Blank) && matches!(out.last(), None | Some(Item::Blank)) {
            continue;
        }
        out.push(item);
    }
    if matches!(out.last(), Some(Item::Blank)) {
        out.pop();
    }
    out
}

/// The items of a container, normalized.
fn container_items(node: &SyntaxNode) -> Vec<Item> {
    let items = items(node);
    let items = if matches!(node.kind(), BLOCK | VARIANT_BLOCK) {
        move_defaults(items)
    } else {
        items
    };
    normalize_blanks(items)
}

struct Formatter {
    max_width: usize,
}

impl Formatter {
    fn fits(&self, col: usize, text: &str, trailing: usize) -> bool {
        !text.contains('\n') && col + width(text) + trailing <= self.max_width
    }

    fn document(&self, root: &SyntaxNode) -> String {
        let mut out = String::new();
        if root.first_token().is_some_and(|t| t.kind() == BOM) {
            out.push('\u{FEFF}');
        }
        let mut items = Vec::new();
        for item in self::items(root) {
            match item {
                Item::Element(node, trailing) if self.can_unwrap(&node) => {
                    items.extend(container_items(&node));
                    if let Some(comment) = trailing {
                        items.push(Item::Comment(comment));
                    }
                }
                item => items.push(item),
            }
        }
        let items = normalize_blanks(move_defaults(items));
        self.write_items(&items, 0, &mut out);
        out
    }

    /// Whether a document's block can lose its outer braces: an untyped
    /// block, with no comment before its brace, whose entries read the same
    /// without them (`SPEC.md` 4). `{ -12 }` is the one exception: `-12` on
    /// its own is an expression.
    fn can_unwrap(&self, node: &SyntaxNode) -> bool {
        if node.kind() != BLOCK
            || child_token(node, &[IDENT]).is_some()
            || has_interior_comment(node)
        {
            return false;
        }
        let first = node.children().next();
        !first.is_some_and(|entry| {
            entry.kind() == REMOVE_ENTRY
                && entry
                    .descendants_with_tokens()
                    .filter_map(|it| it.into_token())
                    .find(|t| t.kind() == INT || t.kind() == L_BRACK || t.kind() == IDENT)
                    .is_some_and(|t| t.kind() == INT)
        })
    }

    /// Writes items one per line at `indent`. Entries and elements are
    /// followed by a comma, except at the top level.
    fn write_items(&self, items: &[Item], indent: usize, out: &mut String) {
        let pad = INDENT.repeat(indent);
        for (i, item) in items.iter().enumerate() {
            match item {
                Item::Blank => out.push('\n'),
                Item::Comment(comment) => {
                    out.push_str(&pad);
                    out.push_str(comment);
                    out.push('\n');
                }
                Item::Element(node, trailing) => {
                    let comma = if indent > 0 {
                        is_entry(node.kind()) || is_expr(node.kind())
                    } else {
                        // Attributes and a document's expression never take
                        // one.
                        is_entry(node.kind()) && self.comma_at_top_level(&items[i + 1..])
                    };
                    out.push_str(&pad);
                    out.push_str(&self.item(
                        node,
                        indent,
                        indent * INDENT.len(),
                        usize::from(comma),
                    ));
                    if comma {
                        out.push(',');
                    }
                    if let Some(comment) = trailing {
                        out.push(' ');
                        out.push_str(comment);
                    }
                    out.push('\n');
                }
            }
        }
    }

    /// Whether the top-level entry before `rest` keeps its comma.
    fn comma_at_top_level(&self, rest: &[Item]) -> bool {
        let next = rest.iter().find_map(|item| match item {
            Item::Element(node, _) => Some(node),
            _ => None,
        });
        next.is_some_and(|next| needs_comma_before(next, self))
    }

    /// An entry or element at the start of a line.
    fn item(&self, node: &SyntaxNode, indent: usize, col: usize, trailing: usize) -> String {
        if has_interior_comment(node) {
            return node.text().to_string();
        }
        self.render(node, indent, col, trailing)
    }

    /// `node` written at column `col` of a line indented `indent` levels,
    /// with `trailing` characters to follow it on its last line.
    fn render(&self, node: &SyntaxNode, indent: usize, col: usize, trailing: usize) -> String {
        match node.kind() {
            BLOCK | LIST_EXPR | TUPLE_EXPR | NAMED_TUPLE | MACRO_CALL | VARIANT_BLOCK => {
                if let Some(flat) = self.flat(node)
                    && self.fits(col, &flat, trailing)
                {
                    return flat;
                }
                self.broken(node, indent)
            }
            ENTRY | VALUE_BIND | SPREAD => {
                let prefix = self.entry_prefix(node);
                let value = child_node(node, is_expr);
                let value = value.map_or_else(String::new, |value| {
                    self.render(&value, indent, col + width(&prefix), trailing)
                });
                prefix + &value
            }
            BINARY_EXPR => {
                if let Some(flat) = self.flat(node)
                    && self.fits(col, &flat, trailing)
                {
                    return flat;
                }
                let mut operands = node.children();
                let (Some(lhs), Some(rhs)) = (operands.next(), operands.next()) else {
                    return node.text().to_string();
                };
                let op = self.binary_op(node);
                if holds_container(&rhs) {
                    let op = format!(" {op} ");
                    let opening = self.opening_width(&rhs);
                    let lhs = self.render(&lhs, indent, col, width(&op) + opening);
                    let rhs_col = col_after(col, &lhs) + width(&op);
                    return lhs + &op + &self.render(&rhs, indent, rhs_col, trailing);
                }
                let Some(flat_rhs) = self.flat(&rhs) else {
                    return node.text().to_string();
                };
                let lhs = self.render(&lhs, indent, col, width(&op) + 1);
                let rhs_col = col_after(col, &lhs) + width(&op) + 2;
                let next_line_col = (indent + 1) * INDENT.len();
                // A line break helps only if it moves the operand to the left.
                // A multi-line string would gain nothing.
                if rhs_col <= next_line_col
                    || flat_rhs.contains('\n')
                    || self.fits(rhs_col, &flat_rhs, trailing)
                {
                    return format!("{lhs} {op} ") + &self.render(&rhs, indent, rhs_col, trailing);
                }
                let rhs = self.render(&rhs, indent + 1, next_line_col, trailing);
                format!("{lhs} {op}\n{}{rhs}", INDENT.repeat(indent + 1))
            }
            ATTRIBUTE => {
                let flat = self.attribute(node);
                match self.use_paths(node) {
                    Some(paths) if !self.fits(col, &flat, trailing) => {
                        let pad = INDENT.repeat(indent + 1);
                        let paths: String =
                            paths.iter().map(|path| format!("{pad}{path},\n")).collect();
                        format!("#![use(\n{paths}{})]", INDENT.repeat(indent))
                    }
                    _ => flat,
                }
            }
            PREFIX_EXPR => {
                let operand = node.children().next();
                let operand = operand.map_or_else(String::new, |operand| {
                    self.render(&operand, indent, col + 1, trailing)
                });
                format!("-{operand}")
            }
            PAREN_EXPR => {
                let inner = node.children().next();
                let inner = inner.map_or_else(String::new, |inner| {
                    self.render(&inner, indent, col + 1, trailing + 1)
                });
                format!("({inner})")
            }
            PATH_EXPR => {
                let selectors = self.selectors(node);
                let base = node.children().next();
                let rendered = base.as_ref().map_or_else(String::new, |base| {
                    self.render(base, indent, col, width(&selectors) + trailing)
                });
                rendered + separator(base.as_ref(), &selectors) + &selectors
            }
            _ => self.flat(node).unwrap_or_else(|| node.text().to_string()),
        }
    }

    /// The width of `node` up to the first place it can break.
    fn opening_width(&self, node: &SyntaxNode) -> usize {
        let first = node.children().next();
        let first = |this: &Self| first.as_ref().map_or(0, |n| this.opening_width(n));
        match node.kind() {
            BLOCK | LIST_EXPR | TUPLE_EXPR | NAMED_TUPLE | MACRO_CALL => {
                let (prefix, open, _) = self.container_parts(node);
                width(&prefix) + width(open)
            }
            PREFIX_EXPR | PAREN_EXPR => 1 + first(self),
            PATH_EXPR | BINARY_EXPR => first(self),
            _ => self.flat(node).map_or(1, |s| first_line_width(&s)),
        }
    }

    /// `node` on one line, if it can be: it holds no comments and nothing
    /// that must break.
    fn flat(&self, node: &SyntaxNode) -> Option<String> {
        Some(match node.kind() {
            LITERAL | UNIT_NAME | REFERENCE => {
                significant_tokens(node).map(|t| t.to_string()).collect()
            }
            ID_EXPR => format!("@{}", significant_tokens(node).nth(1)?),
            BLOCK | LIST_EXPR | TUPLE_EXPR | NAMED_TUPLE | MACRO_CALL | VARIANT_BLOCK => {
                self.flat_container(node)?
            }
            PAREN_EXPR => format!("({})", self.flat(&node.children().next()?)?),
            PREFIX_EXPR => format!("-{}", self.flat(&node.children().next()?)?),
            PATH_EXPR => {
                let base = node.children().next()?;
                let selectors = self.selectors(node);
                self.flat(&base)? + separator(Some(&base), &selectors) + &selectors
            }
            BINARY_EXPR => {
                let mut operands = node.children();
                let lhs = self.flat(&operands.next()?)?;
                let rhs = self.flat(&operands.next()?)?;
                format!("{lhs} {} {rhs}", self.binary_op(node))
            }
            ENTRY | VALUE_BIND | SPREAD => {
                self.entry_prefix(node) + &self.flat(&child_node(node, is_expr)?)?
            }
            REMOVE_ENTRY => format!("-{}", self.target(&child_node(node, |k| k == TARGET)?)),
            DEFAULTS => "..".to_owned(),
            ATTRIBUTE => self.attribute(node),
            _ => node.text().to_string(),
        })
    }

    fn binary_op(&self, node: &SyntaxNode) -> String {
        child_token(
            node,
            &[PLUS, MINUS, STAR, SLASH, PERCENT, TILDE, TILDE_CARET, PIPE],
        )
        .map_or_else(String::new, |t| t.to_string())
    }

    /// What comes before an entry's value: `name = +target: `, `name := ` or
    /// `..`.
    fn entry_prefix(&self, node: &SyntaxNode) -> String {
        match node.kind() {
            ENTRY => {
                let mut prefix = String::new();
                if let Some(bind) = child_node(node, |k| k == BIND)
                    && let Some(name) = child_token(&bind, &[IDENT])
                {
                    prefix.push_str(name.text());
                    prefix.push_str(" = ");
                }
                if let Some(op) = child_token(node, &[PLUS, QUESTION, CARET]) {
                    prefix.push_str(op.text());
                }
                if let Some(target) = child_node(node, |k| k == TARGET) {
                    prefix.push_str(&self.target(&target));
                }
                prefix.push_str(": ");
                prefix
            }
            VALUE_BIND => {
                let name = child_token(node, &[IDENT]).map_or_else(String::new, |t| t.to_string());
                format!("{name} := ")
            }
            _ => "..".to_owned(),
        }
    }

    fn target(&self, target: &SyntaxNode) -> String {
        let mut out = String::new();
        if let Some(key) = child_node(target, |k| k == KEY) {
            if let Some(accessor) = child_node(&key, |k| k == ACCESSOR) {
                let token = child_token(&accessor, &[STRING, RAW_STRING]);
                match token.as_ref().and_then(string_as_identifier) {
                    Some(name) => out.push_str(name),
                    None => out.push_str(&self.accessor(&accessor)),
                }
            } else if let Some(token) = significant_tokens(&key).next() {
                out.push_str(string_as_identifier(&token).unwrap_or(token.text()));
            }
        }
        let selectors = self.selectors(target);
        out.push_str(separator(
            child_node(target, |k| k == KEY).as_ref(),
            &selectors,
        ));
        out.push_str(&selectors);
        out
    }

    /// The selectors that are children of `node`.
    fn selectors(&self, node: &SyntaxNode) -> String {
        let mut out = String::new();
        for selector in node.children().filter(|n| is_selector(n.kind())) {
            match selector.kind() {
                ACCESSOR => {
                    let token = child_token(&selector, &[STRING, RAW_STRING]);
                    match token.as_ref().and_then(string_as_identifier) {
                        Some(name) => {
                            out.push('.');
                            out.push_str(name);
                        }
                        None => out.push_str(&self.accessor(&selector)),
                    }
                }
                VARIANT => out.push_str(&self.variant(&selector)),
                _ => out.extend(significant_tokens(&selector).map(|t| t.to_string())),
            }
        }
        out
    }

    fn accessor(&self, accessor: &SyntaxNode) -> String {
        significant_tokens(accessor)
            .map(|t| t.to_string())
            .collect()
    }

    fn variant(&self, variant: &SyntaxNode) -> String {
        let conditions: Vec<_> = variant.children().map(|c| self.condition(&c)).collect();
        format!("<{}>", conditions.join(" + "))
    }

    fn condition(&self, condition: &SyntaxNode) -> String {
        let mut children = condition.children();
        match condition.kind() {
            COND_NOT => format!(
                "!{}",
                children
                    .next()
                    .map_or_else(String::new, |c| self.condition(&c))
            ),
            COND_PAREN => format!(
                "({})",
                children
                    .next()
                    .map_or_else(String::new, |c| self.condition(&c))
            ),
            COND_BINARY => {
                let op = child_token(condition, &[AMP, PIPE, CARET])
                    .map_or_else(String::new, |t| t.to_string());
                let lhs = children
                    .next()
                    .map_or_else(String::new, |c| self.condition(&c));
                let rhs = children
                    .next()
                    .map_or_else(String::new, |c| self.condition(&c));
                format!("{lhs} {op} {rhs}")
            }
            _ => significant_tokens(condition)
                .map(|t| t.to_string())
                .collect(),
        }
    }

    /// `#![use(a::B, c::D as E)]`
    fn attribute(&self, node: &SyntaxNode) -> String {
        match self.use_paths(node) {
            Some(paths) => format!("#![use({})]", paths.join(", ")),
            None => self.spaced_tokens(node),
        }
    }

    /// The paths of a `#![use(...)]` attribute, if `node` is one.
    fn use_paths(&self, node: &SyntaxNode) -> Option<Vec<String>> {
        let paths = child_node(node, |k| k == USE_ATTR)?
            .children()
            .filter(|path| path.kind() == USE_PATH)
            .map(|path| self.spaced_tokens(&path))
            .collect();
        Some(paths)
    }

    fn spaced_tokens(&self, node: &SyntaxNode) -> String {
        significant_tokens(node)
            .map(|t| match t.kind() {
                COMMA => ", ".to_owned(),
                AS_KW => " as ".to_owned(),
                _ => t.to_string(),
            })
            .collect()
    }

    /// What comes before a container's opening bracket, and its brackets.
    fn container_parts(&self, node: &SyntaxNode) -> (String, &'static str, &'static str) {
        match node.kind() {
            BLOCK => {
                let name =
                    child_token(node, &[IDENT]).map_or_else(String::new, |t| format!("{t} "));
                (name, "{", "}")
            }
            VARIANT_BLOCK => {
                let variant = child_node(node, |k| k == VARIANT);
                let variant = variant.map_or_else(String::new, |v| self.variant(&v) + " ");
                (variant, "{", "}")
            }
            LIST_EXPR => (String::new(), "[", "]"),
            NAMED_TUPLE => {
                let name = child_token(node, &[IDENT]).map_or_else(String::new, |t| t.to_string());
                (name, "(", ")")
            }
            MACRO_CALL => {
                let name = child_token(node, &[IDENT]).map_or_else(String::new, |t| t.to_string());
                (name + "!", "(", ")")
            }
            _ => (String::new(), "(", ")"),
        }
    }

    /// The node holding a container's items: a named tuple's argument list,
    /// or the container itself.
    fn items_node(node: &SyntaxNode) -> SyntaxNode {
        match node.kind() {
            NAMED_TUPLE | MACRO_CALL => {
                child_node(node, |k| k == ARG_LIST).unwrap_or_else(|| node.clone())
            }
            _ => node.clone(),
        }
    }

    fn flat_container(&self, node: &SyntaxNode) -> Option<String> {
        if broken_in_source(node) {
            return None;
        }
        let mut elements = Vec::new();
        for item in container_items(&Self::items_node(node)) {
            match item {
                Item::Element(element, None) if !has_interior_comment(&element) => {
                    elements.push(self.flat(&element)?);
                }
                Item::Blank => {}
                _ => return None,
            }
        }
        let (prefix, open, close) = self.container_parts(node);
        let spaced = open == "{";
        Some(match elements.len() {
            0 => format!("{prefix}{open}{close}"),
            1 if node.kind() == TUPLE_EXPR => format!("({},)", elements[0]),
            _ if spaced => format!("{prefix}{open} {} {close}", elements.join(", ")),
            _ => format!("{prefix}{open}{}{close}", elements.join(", ")),
        })
    }

    /// The only argument of a named tuple or macro call, if it is a container
    /// with something in it and no comment around it. The two then break as
    /// one, so `List([` shares a line. This must not depend on how the source
    /// breaks its blocks, or formatting would not be idempotent.
    fn sole_container(node: &SyntaxNode) -> Option<SyntaxNode> {
        if !matches!(node.kind(), NAMED_TUPLE | MACRO_CALL) {
            return None;
        }
        let items = container_items(&Self::items_node(node));
        let [Item::Element(argument, None)] = items.as_slice() else {
            return None;
        };
        let is_container = matches!(
            argument.kind(),
            BLOCK | LIST_EXPR | TUPLE_EXPR | NAMED_TUPLE | MACRO_CALL
        );
        (is_container
            && !has_interior_comment(argument)
            && !container_items(&Self::items_node(argument)).is_empty())
        .then(|| argument.clone())
    }

    fn broken(&self, node: &SyntaxNode, indent: usize) -> String {
        let (prefix, open, close) = self.container_parts(node);
        if let Some(argument) = Self::sole_container(node) {
            return format!("{prefix}{open}{}{close}", self.broken(&argument, indent));
        }
        let mut out = format!("{prefix}{open}\n");
        let items = container_items(&Self::items_node(node));
        self.write_items(&items, indent + 1, &mut out);
        out.push_str(&INDENT.repeat(indent));
        out.push_str(close);
        out
    }
}
