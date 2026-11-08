//! The parser: turns tokens into a lossless syntax tree (`SPEC.md` section 9).
//!
//! It is a hand-written recursive descent parser that recovers from errors:
//! it always produces a tree holding every token of the input, and reports
//! every problem it finds on the way.

use rowan::{Checkpoint, GreenNode, GreenNodeBuilder, TextRange, TextSize};

use crate::SyntaxError;
use crate::SyntaxKind::{self, *};
use crate::lexer::{Token, tokenize};

/// Stands for the end of the input; it is never put in a tree.
const EOF: SyntaxKind = SyntaxKind::__LAST;

/// The deepest nesting the parser accepts, counting brackets, prefix
/// operators and chains of binary operators. Everything that walks the tree
/// recursively (the formatter, the evaluator) relies on this bound, so that
/// no input can overflow the stack.
pub const MAX_NESTING: usize = 256;

pub(crate) fn parse(text: &str) -> (GreenNode, Vec<SyntaxError>) {
    let (tokens, mut errors) = tokenize(text);
    let mut starts = Vec::with_capacity(tokens.len() + 1);
    let mut offset = TextSize::new(0);
    for token in &tokens {
        starts.push(offset);
        offset += token.len;
    }
    starts.push(offset);

    let mut significant = Vec::with_capacity(tokens.len());
    let mut starts_line = Vec::with_capacity(tokens.len());
    let mut line_break = false;
    for (i, token) in tokens.iter().enumerate() {
        if token.kind.is_trivia() {
            let range = TextRange::new(starts[i], starts[i + 1]);
            line_break |= text[range].contains('\n');
        } else {
            significant.push(i);
            starts_line.push(std::mem::take(&mut line_break));
        }
    }
    let mut p = Parser {
        text,
        significant,
        starts_line,
        next_significant: 0,
        tokens,
        starts,
        pos: 0,
        last_end: TextSize::new(0),
        builder: GreenNodeBuilder::new(),
        errors: Vec::new(),
        depth: 0,
        reported_depth: false,
        among_entries: false,
    };
    p.document();
    errors.append(&mut p.errors);
    errors.sort_by_key(|e| (e.range().start(), e.range().end()));
    (p.builder.finish(), errors)
}

struct Parser<'t> {
    text: &'t str,
    tokens: Vec<Token>,
    /// The byte offset of each token, and of the end of the text.
    starts: Vec<TextSize>,
    /// The index of the next token, trivia included.
    pos: usize,
    /// The indices of the tokens that are not trivia, for constant-time
    /// lookahead.
    significant: Vec<usize>,
    /// For each token in `significant`, whether a line break lies between it
    /// and the token before it, in whitespace or in a comment.
    starts_line: Vec<bool>,
    /// The index in `significant` of the first token at or after `pos`.
    next_significant: usize,
    /// The end of the last token that was not trivia.
    last_end: TextSize,
    builder: GreenNodeBuilder<'static>,
    errors: Vec<SyntaxError>,
    /// The current nesting, against [`MAX_NESTING`].
    depth: usize,
    /// Whether "nested too deeply" has been reported; it is reported once.
    reported_depth: bool,
    /// Whether the innermost bracket around the current token is a block's
    /// (or the document's entries), so that a line break can end an entry
    /// (`SPEC.md` 5.3). In `(...)` and `[...]` it cannot.
    among_entries: bool,
}

/// Tokens that may start an expression.
fn starts_expr(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        INT | FLOAT
            | STRING
            | RAW_STRING
            | CHAR
            | TRUE_KW
            | FALSE_KW
            | AT
            | AMP
            | L_BRACE
            | L_BRACK
            | L_PAREN
            | IDENT
            | MINUS
            | BANG
    )
}

/// Tokens that may start a key.
fn starts_key(kind: SyntaxKind) -> bool {
    matches!(kind, IDENT | STRING | RAW_STRING | INT | L_BRACK)
}

impl Parser<'_> {
    // ---- Token access ----

    /// The index of the `n`-th token from `pos` that is not trivia.
    fn nth_index(&self, n: usize) -> Option<usize> {
        self.significant.get(self.next_significant + n).copied()
    }

    fn nth(&self, n: usize) -> SyntaxKind {
        self.nth_index(n).map_or(EOF, |i| self.tokens[i].kind)
    }

    fn current(&self) -> SyntaxKind {
        self.nth(0)
    }

    fn at(&self, kind: SyntaxKind) -> bool {
        self.current() == kind
    }

    fn token_text(&self, index: usize) -> &str {
        let range = self.token_range(index);
        &self.text[range]
    }

    fn token_range(&self, index: usize) -> TextRange {
        TextRange::new(self.starts[index], self.starts[index + 1])
    }

    fn nth_text(&self, n: usize) -> &str {
        self.nth_index(n).map_or("", |i| self.token_text(i))
    }

    /// Whether the current token directly follows the previous one, with no
    /// trivia in between.
    fn current_is_adjacent(&self) -> bool {
        self.pos < self.tokens.len() && !self.tokens[self.pos].kind.is_trivia()
    }

    /// Whether the `n`-th token from `pos` directly follows the one before it.
    fn nth_is_adjacent(&self, n: usize) -> bool {
        self.tokens
            .get(self.pos + n)
            .is_some_and(|token| !token.kind.is_trivia())
    }

    /// Whether the `n`-th token from `pos` is the first on its line.
    fn nth_starts_line(&self, n: usize) -> bool {
        self.starts_line
            .get(self.next_significant + n)
            .copied()
            .unwrap_or(false)
    }

    /// The range of the current token, or an empty range at the end.
    fn current_range(&self) -> TextRange {
        match self.nth_index(0) {
            Some(i) => self.token_range(i),
            None => TextRange::empty(self.starts[self.tokens.len()]),
        }
    }

    // ---- Tree building ----

    fn skip_trivia(&mut self) {
        while self.pos < self.tokens.len() && self.tokens[self.pos].kind.is_trivia() {
            self.emit(self.tokens[self.pos].kind);
        }
    }

    fn emit(&mut self, kind: SyntaxKind) {
        let range = self.token_range(self.pos);
        self.builder.token(kind.into(), &self.text[range]);
        if !self.tokens[self.pos].kind.is_trivia() {
            self.last_end = range.end();
            self.next_significant += 1;
        }
        self.pos += 1;
    }

    fn bump(&mut self) {
        let kind = self.current();
        self.bump_as(kind);
    }

    /// Adds the current token to the tree as `kind`, for contextual keywords.
    fn bump_as(&mut self, kind: SyntaxKind) {
        assert_ne!(self.current(), EOF, "bump at the end of the input");
        self.skip_trivia();
        self.emit(kind);
    }

    fn eat(&mut self, kind: SyntaxKind) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn start(&mut self, kind: SyntaxKind) {
        self.skip_trivia();
        self.builder.start_node(kind.into());
    }

    fn checkpoint(&mut self) -> Checkpoint {
        self.skip_trivia();
        self.builder.checkpoint()
    }

    fn start_at(&mut self, checkpoint: Checkpoint, kind: SyntaxKind) {
        self.builder.start_node_at(checkpoint, kind.into());
    }

    fn finish(&mut self) {
        self.builder.finish_node();
    }

    // ---- Errors ----

    /// Reports an error at the current token.
    fn error(&mut self, message: impl Into<String>) {
        let range = self.current_range();
        self.errors.push(SyntaxError::new(message, range));
    }

    /// Reports that something is missing, just after the last token.
    fn error_missing(&mut self, message: impl Into<String>) {
        let range = TextRange::empty(self.last_end);
        self.errors.push(SyntaxError::new(message, range));
    }

    fn expect(&mut self, kind: SyntaxKind, what: &str) -> bool {
        if self.eat(kind) {
            true
        } else {
            self.error_missing(format!("expected {what}"));
            false
        }
    }

    /// Reports an error and puts the current token in an `ERROR` node. An
    /// `ERROR_TOKEN` is not reported again: the lexer already did.
    fn error_bump(&mut self, message: impl Into<String>) {
        if !self.at(ERROR_TOKEN) {
            self.error(message);
        }
        if !self.at(EOF) {
            self.start(ERROR);
            self.bump();
            self.finish();
        }
    }

    /// Skips tokens, balancing brackets, until one of `stop` at the outer
    /// level, and puts them in an `ERROR` node.
    fn recover_until(&mut self, stop: &[SyntaxKind]) {
        if self.at(EOF) || stop.contains(&self.current()) {
            return;
        }
        // Skipped tokens are always an error; the caller may have reported
        // it already, at the last token or here.
        if self
            .errors
            .last()
            .is_none_or(|e| e.range().end() < self.last_end)
        {
            let message = format!("unexpected `{}`", self.nth_text(0));
            self.error(message);
        }
        self.start(ERROR);
        let mut depth = 0usize;
        let mut first = true;
        loop {
            let kind = self.current();
            if kind == EOF || depth == 0 && stop.contains(&kind) {
                break;
            }
            // Without commas to stop at, the next entry is where to resume.
            if depth == 0 && !first && self.at_next_entry() {
                break;
            }
            match kind {
                L_BRACE | L_BRACK | L_PAREN => depth += 1,
                R_BRACE | R_BRACK | R_PAREN if depth > 0 => depth -= 1,
                // A closing bracket of something outside: leave it to its
                // owner, unless it is the first token, so that we progress.
                R_BRACE | R_BRACK | R_PAREN if !first => break,
                _ => {}
            }
            self.bump();
            first = false;
        }
        self.finish();
    }

    // ---- Nesting ----

    /// Enters one level of nesting. Beyond [`MAX_NESTING`], reports an error
    /// once, skips the construct at the current token and returns `false`.
    fn enter(&mut self) -> bool {
        if self.depth < MAX_NESTING {
            self.depth += 1;
            return true;
        }
        if !self.reported_depth {
            self.reported_depth = true;
            self.error(format!(
                "nested too deeply; the limit is {MAX_NESTING} levels"
            ));
        }
        self.skip_construct();
        false
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    /// Puts the current token in an `ERROR` node, with everything up to its
    /// matching bracket if it opens one. Iterative, so that it works at any
    /// depth.
    fn skip_construct(&mut self) {
        if self.at(EOF) {
            return;
        }
        self.start(ERROR);
        let mut depth = 0usize;
        loop {
            match self.current() {
                EOF => break,
                L_BRACE | L_BRACK | L_PAREN | L_ANGLE => depth += 1,
                R_BRACE | R_BRACK | R_PAREN | R_ANGLE => depth = depth.saturating_sub(1),
                _ => {}
            }
            self.bump();
            if depth == 0 {
                break;
            }
        }
        self.finish();
    }

    // ---- Documents ----

    fn document(&mut self) {
        // Not `start`: leading trivia belongs inside the root.
        self.builder.start_node(DOCUMENT.into());
        while self.at(POUND) && !self.at_entry_attr() {
            self.attribute();
        }
        if self.document_is_entries() {
            self.entries(EOF);
        } else {
            self.expr();
            if !self.at(EOF) {
                self.error("expected the end of the document");
                self.start(ERROR);
                while !self.at(EOF) {
                    self.bump();
                }
                self.finish();
            }
        }
        self.skip_trivia();
        self.finish();
    }

    /// Whether the document holds entries rather than one expression
    /// (`SPEC.md` section 4).
    fn document_is_entries(&self) -> bool {
        match self.current() {
            EOF | PLUS | QUESTION | CARET | DOT2 | L_ANGLE | POUND => true,
            IDENT if matches!(self.nth(1), EQ | COLON_EQ) => true,
            MINUS => matches!(
                self.nth(1),
                IDENT | STRING | RAW_STRING | L_BRACK | QUESTION
            ),
            _ => self.at_target_then_colon(),
        }
    }

    /// Whether the tokens ahead are a target followed by `:`.
    fn at_target_then_colon(&self) -> bool {
        let mut n = 0;
        let skip_balanced = |n: &mut usize, open: SyntaxKind, close: SyntaxKind| -> bool {
            let mut depth = 0usize;
            loop {
                match self.nth(*n) {
                    EOF => return false,
                    k if k == open => depth += 1,
                    k if k == close => {
                        depth -= 1;
                        if depth == 0 {
                            *n += 1;
                            return true;
                        }
                    }
                    _ => {}
                }
                *n += 1;
            }
        };
        match self.nth(n) {
            // `true: 1` is entries, so that the key gets a proper error.
            IDENT | STRING | RAW_STRING | INT | TRUE_KW | FALSE_KW => n += 1,
            L_BRACK => {
                if !skip_balanced(&mut n, L_BRACK, R_BRACK) {
                    return false;
                }
            }
            _ => return false,
        }
        loop {
            match self.nth(n) {
                DOT if matches!(self.nth(n + 1), IDENT | INT) => n += 2,
                QUESTION if matches!(self.nth(n + 1), DOT | L_BRACK | COLON) => n += 1,
                L_BRACK => {
                    if !skip_balanced(&mut n, L_BRACK, R_BRACK) {
                        return false;
                    }
                }
                L_ANGLE => {
                    if !skip_balanced(&mut n, L_ANGLE, R_ANGLE) {
                        return false;
                    }
                }
                COLON => return true,
                _ => return false,
            }
        }
    }

    // ---- Attributes (`SPEC.md` 4.1) ----

    fn attribute(&mut self) {
        self.start(ATTRIBUTE);
        self.bump(); // `#`
        if !self.current_is_adjacent() || !self.at(BANG) {
            self.error_missing("expected `!` after `#`: attributes are written `#![...]`");
        } else {
            self.bump();
        }
        if self.expect(L_BRACK, "`[`") {
            let kind = match self.nth_text(0) {
                _ if !self.at(IDENT) => None,
                "use" => Some((USE_KW, USE_ATTR)),
                "schema" => Some((SCHEMA_KW, SCHEMA_ATTR)),
                "type" => Some((TYPE_KW, TYPE_ATTR)),
                "extends" => Some((EXTENDS_KW, EXTENDS_ATTR)),
                "variant" => Some((VARIANT_KW, VARIANT_ATTR)),
                _ => None,
            };
            match kind {
                Some((keyword, node)) => {
                    self.start(node);
                    self.bump_as(keyword);
                    if self.expect(L_PAREN, "`(`") {
                        match node {
                            USE_ATTR => self.use_paths(),
                            SCHEMA_ATTR => {
                                if matches!(self.current(), STRING | RAW_STRING) {
                                    self.bump();
                                } else {
                                    self.module_path();
                                }
                            }
                            TYPE_ATTR => {
                                self.expect(IDENT, "a type name");
                            }
                            VARIANT_ATTR => {
                                if self.expect(IDENT, "a group name") {
                                    self.expect(COLON, "`:` after the group name");
                                    self.expect(IDENT, "a variant name");
                                }
                            }
                            _ => self.module_path(),
                        }
                        self.recover_until(&[R_PAREN, R_BRACK]);
                        self.expect(R_PAREN, "`)`");
                    }
                    self.finish();
                }
                None if self.at(IDENT) => {
                    self.error(format!(
                        "unknown attribute `{}`; the attributes are `use`, `schema`, `type`, `extends` and `variant`",
                        self.nth_text(0)
                    ));
                }
                None => self.error("expected an attribute name"),
            }
            self.recover_until(&[R_BRACK]);
            self.expect(R_BRACK, "`]`");
        }
        self.finish();
    }

    fn use_paths(&mut self) {
        loop {
            self.use_path();
            if !self.eat(COMMA) || self.at(R_PAREN) {
                break;
            }
        }
    }

    fn use_path(&mut self) {
        self.start(USE_PATH);
        self.path_segments();
        if self.at(L_ANGLE) {
            self.type_args();
        }
        if self.at(IDENT) && self.nth_text(0) == "as" {
            self.start(USE_ALIAS);
            self.bump_as(AS_KW);
            self.expect(IDENT, "an alias name after `as`");
            self.finish();
        }
        self.finish();
    }

    /// `a::b::c`, the segments of a Rust path.
    fn path_segments(&mut self) {
        if self.expect(IDENT, "a path") {
            while self.at(COLON2) && self.nth(1) == IDENT {
                self.bump();
                self.bump();
            }
        }
    }

    fn type_args(&mut self) {
        self.start(TYPE_ARG_LIST);
        self.bump(); // `<`
        loop {
            self.type_expr();
            if !self.eat(COMMA) || self.at(R_ANGLE) {
                break;
            }
        }
        self.expect(R_ANGLE, "`>`");
        self.finish();
    }

    fn type_expr(&mut self) {
        if !self.enter() {
            return;
        }
        self.type_expr_inner();
        self.leave();
    }

    /// `u32`, `app::Pair<A, B>`, `[T; 3]`, `(A, B)` (`SPEC.md` 6.1.2).
    fn type_expr_inner(&mut self) {
        self.start(TYPE_EXPR);
        match self.current() {
            L_BRACK => {
                self.bump();
                self.type_expr();
                if self.at(SEMI) {
                    self.bump();
                    self.expect(INT, "an array length");
                } else if self.at(R_BRACK) {
                    self.error_missing("expected `; n`: `[T; n]` is an array; a list is `Vec<T>`");
                } else {
                    self.expect(SEMI, "`;`");
                }
                self.expect(R_BRACK, "`]`");
            }
            L_PAREN => {
                self.bump();
                while matches!(self.current(), IDENT | L_BRACK | L_PAREN) {
                    self.type_expr();
                    if !self.eat(COMMA) {
                        break;
                    }
                }
                self.expect(R_PAREN, "`)`");
            }
            _ => {
                self.path_segments();
                if self.at(L_ANGLE) {
                    self.type_args();
                }
            }
        }
        self.finish();
    }

    // ---- Entry attributes (`SPEC.md` 6.1.2) ----

    /// Whether the current token starts `#[type(...)]`, an entry attribute.
    /// Any other `#[...]` before the first entry is taken for a document
    /// attribute without its `!`.
    fn at_entry_attr(&self) -> bool {
        self.at(POUND)
            && self.nth(1) == L_BRACK
            && self.nth(2) == IDENT
            && self.nth_text(2) == "type"
    }

    /// `#[type(...)]`.
    fn entry_attr(&mut self) {
        self.start(ENTRY_ATTR);
        self.bump(); // `#`
        self.bump(); // `[`
        if self.at(IDENT) && self.nth_text(0) == "type" {
            self.bump_as(TYPE_KW);
            if self.expect(L_PAREN, "`(`") {
                self.type_ref();
                self.recover_until(&[R_PAREN, R_BRACK]);
                self.expect(R_PAREN, "`)`");
            }
        } else if self.at(IDENT) {
            self.error(format!(
                "unknown entry attribute `{}`; the only one is `type`, before a value bind",
                self.nth_text(0)
            ));
        } else {
            self.error("expected an attribute name");
        }
        self.recover_until(&[R_BRACK]);
        self.expect(R_BRACK, "`]`");
        self.finish();
    }

    /// A type expression and the steps into it: `Foo.bar.[].0`.
    fn type_ref(&mut self) {
        self.start(TYPE_REF);
        self.type_expr();
        while self.at(DOT) {
            self.start(TYPE_STEP);
            self.bump();
            match self.current() {
                IDENT | INT => self.bump(),
                L_BRACK => {
                    self.bump();
                    self.expect(R_BRACK, "`]`: `.[]` is an element or a map's value");
                }
                _ => self.error_missing("expected a field, an index or `[]` after `.`"),
            }
            self.finish();
        }
        self.finish();
    }

    /// A value bind with its attributes before it, from `#`.
    fn attributed_entry(&mut self) {
        let checkpoint = self.checkpoint();
        self.entry_attr();
        while self.at(POUND) && self.nth(1) == L_BRACK {
            self.error("a value bind takes one `#[type(...)]`");
            self.entry_attr();
        }
        if self.at(IDENT) && self.nth(1) == COLON_EQ {
            self.start_at(checkpoint, VALUE_BIND);
            self.bump();
            self.bump();
            self.expr();
            self.finish();
        } else {
            self.error_missing("expected a value bind after `#[type(...)]`: `name := value`");
            if !matches!(self.current(), COMMA | R_BRACE | EOF) {
                self.entry();
            }
        }
    }

    /// A module path (`SPEC.md` 4.2): `::a::b`, `super::super::a`, `a::b`.
    fn module_path(&mut self) {
        self.start(MODULE_PATH);
        if !self.eat(COLON2) {
            while self.at(IDENT) && self.nth_text(0) == "super" {
                self.bump_as(SUPER_KW);
                self.expect(COLON2, "`::` after `super`");
            }
        }
        if self.at(IDENT) && self.nth_text(0) == "super" {
            self.error("`super` is only allowed at the start of a module path");
        }
        self.path_segments();
        self.finish();
    }

    // ---- Entries (`SPEC.md` section 5) ----

    /// Entries up to `end` (`R_BRACE` or the end of the input), which is not
    /// consumed.
    fn entries(&mut self, end: SyntaxKind) {
        let outer = std::mem::replace(&mut self.among_entries, true);
        self.entries_inner(end);
        self.among_entries = outer;
    }

    fn entries_inner(&mut self, end: SyntaxKind) {
        loop {
            let kind = self.current();
            if kind == end || kind == EOF {
                break;
            }
            if kind == COMMA {
                self.error_bump("expected an entry before `,`");
                continue;
            }
            if kind == R_BRACE || kind == R_BRACK || kind == R_PAREN {
                // A stray closing bracket.
                self.error_bump(format!("unexpected `{}`", self.nth_text(0)));
                continue;
            }
            let before = self.nth_index(0);
            self.entry();
            if self.nth_index(0) == before {
                self.error_bump("expected an entry");
                continue;
            }
            if self.eat(COMMA) {
                continue;
            }
            let kind = self.current();
            if kind == end || kind == EOF {
                break;
            }
            if matches!(kind, R_BRACE | R_BRACK | R_PAREN) {
                // Reported at the top of the loop.
                continue;
            }
            if self.at_entry_start() {
                if !self.nth_starts_line(0) {
                    self.error_missing("expected `,` or a line break between entries");
                } else if kind == MINUS && !self.has_entry_shape(0) {
                    self.error_missing(
                        "expected `,`: a line break does not separate entries before `-` \
                         and an integer or `[`",
                    );
                }
                // Else the line break separates them. A line that is not a
                // whole entry gets its error from `entry`.
            } else if kind == COLON {
                // A key after an operator: `+a: 1 +b: 2`, or `a: 1 +` before
                // the line `b: 2`.
                self.error(
                    "unexpected `:` after an entry: the key before it was read as part of \
                     the value; is a `,` missing, or an operand?",
                );
                self.recover_until(&[COMMA, end, R_BRACE]);
                self.eat(COMMA);
            } else {
                self.error(format!("unexpected `{}` after an entry", self.nth_text(0)));
                self.recover_until(&[COMMA, end, R_BRACE]);
                self.eat(COMMA);
            }
        }
    }

    fn at_entry_start(&self) -> bool {
        match self.current() {
            MINUS | PLUS | QUESTION | CARET | DOT2 | L_ANGLE | POUND => true,
            kind => starts_key(kind),
        }
    }

    /// Whether the current token starts a line that begins a new entry, so
    /// that a value or path before it ends (`SPEC.md` 5.3).
    fn at_next_entry(&self) -> bool {
        self.among_entries && self.nth_starts_line(0) && self.has_entry_shape(0)
    }

    /// Whether the tokens from the `n`-th on have the shape of an entry, on
    /// one line (`SPEC.md` 5.3). Each call looks at one line at most, so the
    /// parser stays linear however many lines ask.
    fn has_entry_shape(&self, n: usize) -> bool {
        let same_line = |n: usize| !self.nth_starts_line(n);
        match self.nth(n) {
            QUESTION | CARET | DOT2 => true,
            POUND => same_line(n + 1) && self.nth(n + 1) == L_BRACK,
            PLUS => same_line(n + 1) && self.has_target_and_colon(n + 1),
            // Not `-12` or `-[key]`: those read as subtracting an integer or
            // a list too, while subtracting a name or a string is an error.
            // `-?` is never an expression, so any key may follow it.
            MINUS if self.nth(n + 1) == QUESTION => {
                same_line(n + 1) && same_line(n + 2) && starts_key(self.nth(n + 2))
            }
            MINUS => same_line(n + 1) && matches!(self.nth(n + 1), IDENT | STRING | RAW_STRING),
            L_ANGLE => self
                .after_variant(n)
                .is_some_and(|n| same_line(n) && self.nth(n) == L_BRACE),
            IDENT if same_line(n + 1) && matches!(self.nth(n + 1), EQ | COLON_EQ) => true,
            _ => self.has_target_and_colon(n),
        }
    }

    /// Whether the tokens from the `n`-th on are a target followed by `:`,
    /// all on one line.
    fn has_target_and_colon(&self, mut n: usize) -> bool {
        match self.nth(n) {
            IDENT | STRING | RAW_STRING | INT | TRUE_KW | FALSE_KW => n += 1,
            L_BRACK => match self.after_accessor(n) {
                Some(after) => n = after,
                None => return false,
            },
            _ => return false,
        }
        loop {
            if self.nth_starts_line(n) {
                return false;
            }
            let after = match self.nth(n) {
                COLON => return true,
                DOT if !self.nth_starts_line(n + 1) && matches!(self.nth(n + 1), IDENT | INT) => {
                    Some(n + 2)
                }
                QUESTION
                    if !self.nth_starts_line(n + 1) && matches!(self.nth(n + 1), DOT | L_BRACK) =>
                {
                    Some(n + 1)
                }
                L_BRACK => self.after_accessor(n),
                L_ANGLE => self.after_variant(n),
                _ => None,
            };
            match after {
                Some(after) => n = after,
                None => return false,
            }
        }
    }

    /// The token after the accessor at the `n`-th token, if it is one with a
    /// single key, on one line.
    fn after_accessor(&self, n: usize) -> Option<usize> {
        let key = if self.nth(n + 1) == MINUS {
            n + 2
        } else {
            n + 1
        };
        let close = key + 1;
        ((n + 1..=close).all(|i| !self.nth_starts_line(i)) && self.nth(close) == R_BRACK)
            .then_some(close + 1)
    }

    /// The token after the `<...>` at the `n`-th token, if it holds only what
    /// a condition can hold, on one line.
    fn after_variant(&self, mut n: usize) -> Option<usize> {
        loop {
            n += 1;
            if self.nth_starts_line(n) {
                return None;
            }
            match self.nth(n) {
                R_ANGLE => return Some(n + 1),
                IDENT | BANG | AMP | PIPE | CARET | PLUS | L_PAREN | R_PAREN => {}
                _ => return None,
            }
        }
    }

    fn entry(&mut self) {
        match self.current() {
            POUND if self.nth(1) == L_BRACK => self.attributed_entry(),
            IDENT if self.nth(1) == COLON_EQ => {
                self.start(VALUE_BIND);
                self.bump();
                self.bump();
                self.expr();
                self.finish();
            }
            IDENT if self.nth(1) == EQ => {
                let checkpoint = self.checkpoint();
                self.start(BIND);
                self.bump();
                self.bump();
                self.finish();
                match self.current() {
                    PLUS | QUESTION | CARET => {}
                    kind if starts_key(kind) => {}
                    MINUS => self.error("a bind cannot name a remove"),
                    L_ANGLE => self.error("a bind cannot name a variant block"),
                    DOT2 => self.error("a bind cannot name a spread or `..`"),
                    _ => self.error_missing("expected an entry after the bind"),
                }
                self.entry_at(checkpoint);
            }
            _ => {
                let checkpoint = self.checkpoint();
                self.entry_at(checkpoint);
            }
        }
    }

    /// An entry, whose node starts at `checkpoint` (before a bind, if any).
    fn entry_at(&mut self, checkpoint: Checkpoint) {
        match self.current() {
            MINUS => {
                self.start_at(checkpoint, REMOVE_ENTRY);
                self.bump();
                if self.at(QUESTION) {
                    if !self.current_is_adjacent() {
                        self.error("a remove that may find nothing is written `-?`, with no space");
                    }
                    self.bump();
                }
                self.start(TARGET);
                self.key();
                self.optional_step(false);
                self.selectors(true, true);
                self.finish();
                self.finish();
            }
            PLUS | QUESTION | CARET => {
                self.start_at(checkpoint, ENTRY);
                self.bump();
                self.target();
                self.entry_value();
                self.finish();
            }
            DOT2 => {
                // With a line break after `..`, `&x` is still its operand,
                // but a whole entry is not.
                if matches!(self.nth(1), COMMA | R_BRACE | EOF)
                    || self.nth_starts_line(1) && self.has_entry_shape(1)
                {
                    self.start_at(checkpoint, DEFAULTS);
                    self.bump();
                } else {
                    self.start_at(checkpoint, SPREAD);
                    self.bump();
                    self.postfix_expr();
                }
                self.finish();
            }
            L_ANGLE => {
                self.start_at(checkpoint, VARIANT_BLOCK);
                self.variant();
                self.block_body();
                self.finish();
            }
            POUND => {
                self.error("attributes must come before the first entry");
                self.attribute();
            }
            TRUE_KW | FALSE_KW => {
                self.start_at(checkpoint, ENTRY);
                self.target();
                self.entry_value();
                self.finish();
            }
            kind if starts_key(kind) => {
                self.start_at(checkpoint, ENTRY);
                self.target();
                if self.at(COLON_EQ) {
                    self.error("a value bind is written `name := value`, with a plain name");
                    self.bump();
                    self.expr();
                } else {
                    self.entry_value();
                }
                self.finish();
            }
            _ => {}
        }
    }

    /// `: expr`. A line that stops without its value, or without its `:` (a
    /// key being typed), does not take in the entry on the next line.
    fn entry_value(&mut self) {
        let colon = self.expect(COLON, "`:`");
        if !self.nth_starts_line(0) {
            self.expr();
        } else if colon {
            // `-x` here negates, as it always did; `-?x` is an entry.
            if self.at_next_entry() && !(self.at(MINUS) && self.nth(1) != QUESTION) {
                self.error_missing("expected a value after `:`");
            } else {
                self.expr();
            }
        }
    }

    /// `{ entries }`, the body of a block or variant block.
    fn block_body(&mut self) {
        if !self.at(L_BRACE) {
            self.error_missing("expected `{`");
            return;
        }
        self.bump();
        self.entries(R_BRACE);
        self.expect(R_BRACE, "`}`");
    }

    fn target(&mut self) {
        self.start(TARGET);
        self.key();
        self.optional_step(false);
        self.selectors(false, true);
        self.finish();
    }

    fn key(&mut self) {
        self.start(KEY);
        match self.current() {
            IDENT | STRING | RAW_STRING | INT => self.bump(),
            L_BRACK => self.accessor(),
            TRUE_KW | FALSE_KW => {
                let word = self.nth_text(0).to_owned();
                self.error(format!(
                    "`{word}` is reserved; write the key as `\"{word}\"`"
                ));
                self.bump();
            }
            _ => self.error_missing("expected a key"),
        }
        self.finish();
    }

    /// Selectors after a key or an expression (`SPEC.md` 5.1). A key before
    /// its `:` takes any selectors; after a value or in a remove,
    /// `stop_at_entry` leaves the next entry alone (`SPEC.md` 5.3).
    fn selectors(&mut self, stop_at_entry: bool, target: bool) {
        loop {
            if stop_at_entry && self.at_next_entry() {
                break;
            }
            let variant = match self.current() {
                DOT => {
                    self.dot_selector();
                    false
                }
                L_BRACK => {
                    self.accessor();
                    false
                }
                L_ANGLE => {
                    self.variant();
                    true
                }
                _ => break,
            };
            if target {
                self.optional_step(variant);
            }
        }
    }

    /// `?` directly after a step of a target, with the next step directly
    /// after it (`SPEC.md` 5.1). Apart, it is the `?` of the next entry.
    fn optional_step(&mut self, after_variant: bool) {
        if !self.current_is_adjacent() || !self.at(QUESTION) {
            return;
        }
        if after_variant {
            self.error("a `?` in a path follows a key or a field, not a condition");
        } else if !self.nth_is_adjacent(1) || !matches!(self.nth(1), DOT | L_BRACK) {
            self.error(
                "a `?` in a path goes before the next step: `a?.b`; to set a value only if \
                 it is not set, write `?a`",
            );
        }
        self.bump();
    }

    fn dot_selector(&mut self) {
        match self.nth(1) {
            IDENT => {
                self.start(FIELD_SELECTOR);
                self.bump();
                self.bump();
            }
            INT => {
                self.start(INDEX_SELECTOR);
                self.bump();
                let text = self.nth_text(0);
                if !text.bytes().all(|b| b.is_ascii_digit()) {
                    self.error("a tuple index is written in decimal digits: `.0`, `.1`, ...");
                }
                self.bump();
            }
            AMP => {
                self.start(PARENT_SELECTOR);
                self.error("`.&parent` and `.&super` are only allowed directly after `&parent` or `&super`");
                self.bump();
                self.bump();
                if self.current_is_adjacent() && self.at(IDENT) {
                    self.bump_as(PARENT_KW);
                }
            }
            TRUE_KW | FALSE_KW => {
                self.start(FIELD_SELECTOR);
                self.bump();
                let word = self.nth_text(0).to_owned();
                self.error(format!(
                    "`{word}` is reserved; write the key as `[\"{word}\"]`"
                ));
                self.bump();
            }
            _ => {
                self.start(FIELD_SELECTOR);
                self.bump();
                self.error_missing("expected a field name or tuple index after `.`");
            }
        }
        self.finish();
    }

    /// `[key]`: a string, or an integer with an optional `-`.
    fn accessor(&mut self) {
        self.start(ACCESSOR);
        self.bump(); // `[`
        match self.current() {
            STRING | RAW_STRING | INT => self.bump(),
            MINUS if self.nth(1) == INT => {
                self.bump();
                self.bump();
            }
            IDENT => {
                let name = self.nth_text(0).to_owned();
                self.error(format!(
                    "an identifier in brackets is not a lookup; write `.{name}`, or `[\"{name}\"]` for the key"
                ));
                self.bump();
            }
            _ => self.error_missing("expected a string or integer key"),
        }
        self.recover_until(&[R_BRACK, COMMA, R_BRACE]);
        self.expect(R_BRACK, "`]`");
        self.finish();
    }

    // ---- Variants (`SPEC.md` 8.2) ----

    /// `<condition + condition>`
    fn variant(&mut self) {
        self.start(VARIANT);
        self.bump(); // `<`
        loop {
            self.condition();
            if !self.eat(PLUS) {
                break;
            }
        }
        self.recover_until(&[R_ANGLE, L_BRACE, COMMA, R_BRACE]);
        self.expect(R_ANGLE, "`>`");
        self.finish();
    }

    fn condition(&mut self) {
        self.cond_binary(0);
    }

    /// Binary conditions, loosest first: `|`, `^`, `&`.
    fn cond_binary(&mut self, level: usize) {
        const LEVELS: [SyntaxKind; 3] = [PIPE, CARET, AMP];
        let Some(&op) = LEVELS.get(level) else {
            self.cond_not();
            return;
        };
        let checkpoint = self.checkpoint();
        self.cond_binary(level + 1);
        while self.at(op) {
            self.start_at(checkpoint, COND_BINARY);
            self.bump();
            self.cond_binary(level + 1);
            self.finish();
        }
    }

    fn cond_not(&mut self) {
        if !self.enter() {
            return;
        }
        self.cond_not_inner();
        self.leave();
    }

    fn cond_not_inner(&mut self) {
        match self.current() {
            BANG => {
                self.start(COND_NOT);
                self.bump();
                self.cond_not();
                self.finish();
            }
            IDENT => {
                self.start(COND_NAME);
                self.bump();
                self.finish();
            }
            L_PAREN => {
                self.start(COND_PAREN);
                self.bump();
                self.condition();
                if self.at(PLUS) {
                    self.error(
                        "a `+` list is only allowed as the outermost operator of a condition",
                    );
                    while self.eat(PLUS) {
                        self.condition();
                    }
                }
                self.expect(R_PAREN, "`)`");
                self.finish();
            }
            _ => self.error_missing("expected a variant name"),
        }
    }

    // ---- Expressions (`SPEC.md` section 6) ----

    fn expr(&mut self) {
        self.binary(0);
    }

    /// Binary operators, loosest first (`SPEC.md` 6.2).
    fn binary(&mut self, level: usize) {
        const LEVELS: [&[SyntaxKind]; 4] = [
            &[QUESTION2],
            &[TILDE, TILDE_CARET],
            &[PLUS, MINUS, PIPE],
            &[STAR, SLASH, PERCENT],
        ];
        let Some(ops) = LEVELS.get(level) else {
            self.unary();
            return;
        };
        let checkpoint = self.checkpoint();
        self.binary(level + 1);
        // Each operator in a chain nests the tree one level deeper.
        let mut chain = 0;
        while ops.contains(&self.current()) && !self.at_next_entry() {
            if self.depth + chain >= MAX_NESTING {
                if !self.reported_depth {
                    self.reported_depth = true;
                    self.error(format!(
                        "this expression is nested too deeply; the limit is {MAX_NESTING} levels"
                    ));
                }
                // Leave the rest of the chain to the caller's recovery.
                break;
            }
            chain += 1;
            self.start_at(checkpoint, BINARY_EXPR);
            self.bump();
            self.binary(level + 1);
            self.finish();
        }
    }

    fn unary(&mut self) {
        if matches!(self.current(), MINUS | BANG) {
            if !self.enter() {
                return;
            }
            self.start(PREFIX_EXPR);
            self.bump();
            self.unary();
            self.finish();
            self.leave();
        } else {
            self.postfix_expr();
        }
    }

    fn postfix_expr(&mut self) {
        let checkpoint = self.checkpoint();
        self.primary();
        if matches!(self.current(), DOT | L_BRACK | L_ANGLE) && !self.at_next_entry() {
            self.start_at(checkpoint, PATH_EXPR);
            self.selectors(true, false);
            self.finish();
        }
    }

    fn primary(&mut self) {
        if !self.enter() {
            return;
        }
        self.primary_inner();
        self.leave();
    }

    /// A primary. Inside its `(` or `[`, a line break means nothing.
    fn primary_inner(&mut self) {
        let outer = self.among_entries;
        let kind = self.current();
        if matches!(kind, L_BRACK | L_PAREN)
            || kind == IDENT && matches!(self.nth(1), L_PAREN | BANG)
        {
            self.among_entries = false;
        }
        self.primary_kinds();
        self.among_entries = outer;
    }

    fn primary_kinds(&mut self) {
        match self.current() {
            INT | FLOAT | STRING | RAW_STRING | CHAR | TRUE_KW | FALSE_KW => {
                self.start(LITERAL);
                self.bump();
                self.finish();
            }
            AT => self.id(),
            AMP => self.reference(),
            L_BRACE => {
                self.start(BLOCK);
                self.block_body();
                self.finish();
            }
            IDENT => match self.nth(1) {
                L_BRACE => {
                    self.start(BLOCK);
                    self.bump();
                    self.block_body();
                    self.finish();
                }
                L_PAREN => {
                    self.start(NAMED_TUPLE);
                    self.bump();
                    self.start(ARG_LIST);
                    self.bump();
                    self.exprs(R_PAREN);
                    self.expect(R_PAREN, "`)`");
                    self.finish();
                    self.finish();
                }
                BANG => {
                    self.start(MACRO_CALL);
                    self.bump();
                    self.bump();
                    if self.at(L_PAREN) {
                        self.start(ARG_LIST);
                        self.bump();
                        self.exprs(R_PAREN);
                        self.expect(R_PAREN, "`)`");
                        self.finish();
                    } else {
                        self.error("a macro takes its arguments in `(...)`");
                    }
                    self.finish();
                }
                _ => {
                    self.start(UNIT_NAME);
                    self.bump();
                    self.finish();
                }
            },
            L_BRACK => {
                self.start(LIST_EXPR);
                self.bump();
                self.exprs(R_BRACK);
                self.expect(R_BRACK, "`]`");
                self.finish();
            }
            L_PAREN => self.paren_or_tuple(),
            DOT if self.nth(1) == INT => {
                self.error("a float needs a digit before `.`: write `0.5`");
                self.start(ERROR);
                self.bump();
                self.bump();
                self.finish();
            }
            _ => self.error_missing("expected an expression"),
        }
    }

    /// `()`, `(a)`, `(a,)`, `(a, b)`
    fn paren_or_tuple(&mut self) {
        let checkpoint = self.checkpoint();
        self.bump(); // `(`
        if self.eat(R_PAREN) {
            self.start_at(checkpoint, TUPLE_EXPR);
            self.finish();
            return;
        }
        self.expr();
        if self.eat(COMMA) {
            self.start_at(checkpoint, TUPLE_EXPR);
            self.exprs(R_PAREN);
        } else {
            self.start_at(checkpoint, PAREN_EXPR);
        }
        self.expect(R_PAREN, "`)`");
        self.finish();
    }

    /// Comma-separated expressions up to `close`, which is not consumed.
    fn exprs(&mut self, close: SyntaxKind) {
        loop {
            let kind = self.current();
            if kind == close || kind == EOF {
                break;
            }
            if !starts_expr(kind) {
                self.error(format!("unexpected `{}`", self.nth_text(0)));
                self.recover_until(&[COMMA, close, R_BRACE]);
                if !self.eat(COMMA) {
                    break;
                }
                continue;
            }
            self.expr();
            if self.eat(COMMA) {
                continue;
            }
            if starts_expr(self.current()) {
                self.error_missing("expected `,`");
            } else {
                break;
            }
        }
    }

    /// `@name` or `@"text"` (`SPEC.md` 3.2).
    fn id(&mut self) {
        self.start(ID_EXPR);
        self.bump(); // `@`
        let adjacent = self.current_is_adjacent();
        match self.current() {
            IDENT | STRING | RAW_STRING => {
                if !adjacent {
                    self.error("an id's name must follow `@` directly");
                }
                self.bump();
            }
            TRUE_KW | FALSE_KW => {
                let word = self.nth_text(0).to_owned();
                self.error(format!(
                    "`{word}` is reserved; write the id as `@\"{word}\"`"
                ));
                self.bump();
            }
            _ => self.error_missing("expected a name or string after `@`"),
        }
        self.finish();
    }

    /// `&`, `&name`, `&self`, `&top`, `&root`, `&default`,
    /// `&parent.&parent`, `&super.&super` (`SPEC.md` 6.1).
    fn reference(&mut self) {
        self.start(REFERENCE);
        self.bump(); // `&`
        if self.at(IDENT) {
            if self.current_is_adjacent() {
                self.reference_name();
            } else if !matches!(self.nth(1), COLON | COLON_EQ | EQ) && !self.at_next_entry() {
                // `& port`; unless it looks like the next entry after a
                // missing separator, take the name in anyway.
                self.error("a reference name must follow `&` directly");
                self.reference_name();
            }
        } else if matches!(self.current(), TRUE_KW | FALSE_KW) && self.current_is_adjacent() {
            self.error("`true` and `false` cannot be bind names");
            self.bump();
        }
        self.finish();
    }

    fn reference_name(&mut self) {
        let keyword = SyntaxKind::reference_keyword(self.nth_text(0));
        self.bump_as(keyword.unwrap_or(IDENT));
        if keyword == Some(PARENT_KW) {
            // `.&parent` or `.&super`, repeated.
            while self.at(DOT) && self.nth(1) == AMP {
                self.bump();
                self.bump();
                if self.current_is_adjacent()
                    && self.at(IDENT)
                    && matches!(self.nth_text(0), "parent" | "super")
                {
                    self.bump_as(PARENT_KW);
                } else {
                    self.error_missing("expected `parent` or `super` after `.&`");
                    break;
                }
            }
        }
    }
}
