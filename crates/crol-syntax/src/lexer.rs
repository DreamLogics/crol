//! The lexer: turns source text into tokens (`SPEC.md` section 2).
//!
//! Lexing never fails. Every byte of the input ends up in exactly one token,
//! and problems such as an unterminated string are reported as errors next
//! to the tokens, so the parser can build a lossless tree from them.

use rowan::{TextRange, TextSize};

use crate::SyntaxError;
use crate::SyntaxKind::{self, *};

/// A token: its kind and its length in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    /// The kind of the token.
    pub kind: SyntaxKind,
    /// The length of the token's text, in bytes.
    pub len: TextSize,
}

/// Splits `text` into tokens. The tokens' lengths add up to `text.len()`.
pub fn tokenize(text: &str) -> (Vec<Token>, Vec<SyntaxError>) {
    let mut lexer = Lexer {
        text,
        pos: 0,
        tokens: Vec::new(),
        errors: Vec::new(),
        after_dot: false,
    };
    if text.starts_with('\u{FEFF}') {
        lexer.pos = '\u{FEFF}'.len_utf8();
        lexer.push(BOM, 0);
    }
    while lexer.pos < text.len() {
        let start = lexer.pos;
        let kind = lexer.token();
        debug_assert!(lexer.pos > start, "the lexer must make progress");
        lexer.push(kind, start);
    }
    (lexer.tokens, lexer.errors)
}

struct Lexer<'a> {
    text: &'a str,
    pos: usize,
    tokens: Vec<Token>,
    errors: Vec<SyntaxError>,
    /// Whether the last token other than trivia was `.`, so that a number
    /// is a tuple index (`.0.1` is `.` `0` `.` `1`, `SPEC.md` section 9).
    after_dot: bool,
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

impl Lexer<'_> {
    fn push(&mut self, kind: SyntaxKind, start: usize) {
        let len = TextSize::try_from(self.pos - start).expect("source text exceeds 4 GiB");
        self.tokens.push(Token { kind, len });
        if !kind.is_trivia() {
            self.after_dot = kind == DOT;
        }
    }

    fn rest(&self) -> &str {
        &self.text[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn peek_nth(&self, n: usize) -> Option<char> {
        self.rest().chars().nth(n)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += c.len_utf8();
            true
        } else {
            false
        }
    }

    fn eat_while(&mut self, pred: impl Fn(char) -> bool) {
        while self.peek().is_some_and(&pred) {
            self.bump();
        }
    }

    fn error(&mut self, start: usize, message: impl Into<String>) {
        self.error_range(start, self.pos, message);
    }

    fn error_range(&mut self, start: usize, end: usize, message: impl Into<String>) {
        let range = TextRange::new(
            TextSize::try_from(start).expect("source text exceeds 4 GiB"),
            TextSize::try_from(end).expect("source text exceeds 4 GiB"),
        );
        self.errors.push(SyntaxError::new(message, range));
    }

    fn token(&mut self) -> SyntaxKind {
        let start = self.pos;
        let c = self.bump().expect("token() is only called before the end");
        match c {
            ' ' | '\t' | '\r' | '\n' => {
                self.eat_while(|c| matches!(c, ' ' | '\t' | '\r' | '\n'));
                WHITESPACE
            }
            '/' if self.eat('/') => {
                self.eat_while(|c| c != '\n');
                LINE_COMMENT
            }
            '/' if self.eat('*') => self.block_comment(start),
            'r' if matches!(self.peek(), Some('"' | '#')) => self.raw_string(start),
            c if is_ident_start(c) => {
                self.eat_while(is_ident_continue);
                match &self.text[start..self.pos] {
                    "_" => {
                        self.error(start, "`_` is not an identifier");
                        ERROR_TOKEN
                    }
                    "true" => TRUE_KW,
                    "false" => FALSE_KW,
                    _ => IDENT,
                }
            }
            '0'..='9' => self.number(start, c),
            '"' => self.string(start),
            '\'' => self.char(start),
            '{' => L_BRACE,
            '}' => R_BRACE,
            '[' => L_BRACK,
            ']' => R_BRACK,
            '(' => L_PAREN,
            ')' => R_PAREN,
            '<' => L_ANGLE,
            '>' => R_ANGLE,
            ',' => COMMA,
            ':' if self.eat(':') => COLON2,
            ':' if self.eat('=') => COLON_EQ,
            ':' => COLON,
            '.' if self.eat('.') => {
                if self.peek() == Some('.') {
                    self.eat_while(|c| c == '.');
                    self.error(start, "`...` is not valid; a spread is written `..`");
                    ERROR_TOKEN
                } else {
                    DOT2
                }
            }
            '.' => DOT,
            '+' => PLUS,
            '-' => MINUS,
            '*' => STAR,
            '/' => SLASH,
            '%' => PERCENT,
            '~' if self.eat('^') => TILDE_CARET,
            '~' => TILDE,
            '|' => PIPE,
            '&' => AMP,
            '^' => CARET,
            '@' => AT,
            '?' => QUESTION,
            '=' => EQ,
            '#' => POUND,
            ';' => SEMI,
            '!' => BANG,
            c => {
                self.error(
                    start,
                    format!("unexpected character `{}`", c.escape_debug()),
                );
                ERROR_TOKEN
            }
        }
    }

    fn block_comment(&mut self, start: usize) -> SyntaxKind {
        let mut depth = 1usize;
        while depth > 0 {
            if self.rest().starts_with("/*") {
                self.pos += 2;
                depth += 1;
            } else if self.rest().starts_with("*/") {
                self.pos += 2;
                depth -= 1;
            } else if self.bump().is_none() {
                self.error_range(start, start + 2, "unterminated block comment");
                break;
            }
        }
        BLOCK_COMMENT
    }

    fn number(&mut self, start: usize, first: char) -> SyntaxKind {
        let radix = match (first, self.peek()) {
            ('0', Some('x')) => 16,
            ('0', Some('o')) => 8,
            ('0', Some('b')) => 2,
            _ => 10,
        };
        if radix != 10 {
            self.bump();
            // Letters are taken in too, so that `0x1Fu8` is one (bad) token.
            let digits_start = self.pos;
            self.eat_while(is_ident_continue);
            let digits = &self.text[digits_start..self.pos];
            if !digits.chars().any(|c| c != '_') {
                self.error(start, "missing digits after the integer base prefix");
            } else if let Some(bad) = digits.chars().find(|&c| c != '_' && !c.is_digit(radix)) {
                if bad.is_ascii_digit() || bad.is_ascii_hexdigit() && radix == 16 {
                    self.error(
                        start,
                        format!("invalid digit `{bad}` for a base {radix} integer"),
                    );
                } else {
                    self.error(start, "number literals cannot have a suffix");
                }
            }
            return INT;
        }

        self.eat_while(|c| c.is_ascii_digit() || c == '_');
        let mut kind = INT;
        if !self.after_dot {
            if self.peek() == Some('.') {
                match self.peek_nth(1) {
                    Some(c) if c.is_ascii_digit() => {
                        self.bump();
                        self.eat_while(|c| c.is_ascii_digit() || c == '_');
                        kind = FLOAT;
                    }
                    // `1.x` is a selector and `1..` a spread; neither is a float.
                    Some(c) if is_ident_start(c) || c == '.' => {}
                    _ => {
                        self.bump();
                        self.error(start, "a float needs a digit after `.`: write `1.0`");
                        return FLOAT;
                    }
                }
            }
            if matches!(self.peek(), Some('e' | 'E')) {
                let sign = usize::from(matches!(self.peek_nth(1), Some('+' | '-')));
                if self.peek_nth(1 + sign).is_some_and(|c| c.is_ascii_digit()) {
                    self.pos += 1 + sign;
                    self.eat_while(|c| c.is_ascii_digit() || c == '_');
                    kind = FLOAT;
                }
            }
        }
        if self.peek().is_some_and(is_ident_continue) {
            self.eat_while(is_ident_continue);
            self.error(start, "number literals cannot have a suffix");
        }
        kind
    }

    /// Lexes an escape after `\`. `quote` is the literal's quote character.
    fn escape(&mut self, quote: char) {
        let start = self.pos - 1;
        match self.bump() {
            Some('n' | 'r' | 't' | '\\' | '0' | '"' | '\'') => {}
            Some('\n') if quote == '"' => {}
            Some('\r') if quote == '"' && self.eat('\n') => {}
            Some('x') => {
                let digits: String = self.rest().chars().take(2).collect();
                match u8::from_str_radix(&digits, 16) {
                    Ok(value) if digits.len() == 2 && value <= 0x7F => self.pos += 2,
                    Ok(_) if digits.len() == 2 => {
                        self.pos += 2;
                        self.error(start, "a `\\x` escape must be at most `\\x7F`");
                    }
                    _ => self.error(start, "a `\\x` escape needs two hex digits"),
                }
            }
            Some('u') => {
                if !self.eat('{') {
                    self.error(start, "a `\\u` escape is written `\\u{1F600}`");
                    return;
                }
                let digits_start = self.pos;
                self.eat_while(|c| c.is_ascii_hexdigit() || c == '_');
                let digits: String = self.text[digits_start..self.pos]
                    .chars()
                    .filter(|&c| c != '_')
                    .collect();
                if !self.eat('}') {
                    self.error(start, "unterminated `\\u{...}` escape");
                } else if digits.is_empty() || digits.len() > 6 {
                    self.error(start, "a `\\u{...}` escape needs 1 to 6 hex digits");
                } else if u32::from_str_radix(&digits, 16)
                    .ok()
                    .and_then(char::from_u32)
                    .is_none()
                {
                    self.error(start, "a `\\u{...}` escape must be a Unicode scalar value");
                }
            }
            Some(c) => self.error(start, format!("unknown escape `\\{}`", c.escape_debug())),
            None => {}
        }
    }

    fn string(&mut self, start: usize) -> SyntaxKind {
        loop {
            match self.bump() {
                Some('"') => break,
                Some('\\') => self.escape('"'),
                Some(_) => {}
                None => {
                    self.error_range(start, start + 1, "unterminated string");
                    break;
                }
            }
        }
        self.suffix_check(start);
        STRING
    }

    fn raw_string(&mut self, start: usize) -> SyntaxKind {
        let hashes_start = self.pos;
        self.eat_while(|c| c == '#');
        let hashes = self.pos - hashes_start;
        if !self.eat('"') {
            // `r#type` and the like.
            self.eat_while(is_ident_continue);
            self.error(start, "raw identifiers are not supported");
            return ERROR_TOKEN;
        }
        let close: String = std::iter::once('"')
            .chain(std::iter::repeat_n('#', hashes))
            .collect();
        match self.rest().find(&close) {
            Some(offset) => self.pos += offset + close.len(),
            None => {
                self.pos = self.text.len();
                self.error_range(start, hashes_start + hashes + 1, "unterminated raw string");
            }
        }
        self.suffix_check(start);
        RAW_STRING
    }

    fn char(&mut self, start: usize) -> SyntaxKind {
        match self.bump() {
            Some('\\') => self.escape('\''),
            Some('\'') => {
                self.error(start, "empty char literal");
                return CHAR;
            }
            Some('\n') | None => {
                self.error_range(start, start + 1, "unterminated char literal");
                return CHAR;
            }
            Some(_) => {}
        }
        if !self.eat('\'') {
            self.error_range(start, start + 1, "unterminated char literal");
        }
        CHAR
    }

    /// String and char literals take no suffix either.
    fn suffix_check(&mut self, start: usize) {
        if self.peek().is_some_and(is_ident_start) {
            // The suffix stays in the literal's token, so the tree stays sensible.
            self.eat_while(is_ident_continue);
            self.error(start, "string literals cannot have a suffix");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use expect_test::{Expect, expect};
    use std::fmt::Write;

    fn check(text: &str, expect: Expect) {
        let (tokens, errors) = tokenize(text);
        let mut out = String::new();
        let mut pos = 0usize;
        for token in tokens {
            let end = pos + usize::from(token.len);
            writeln!(out, "{:?} {:?}", token.kind, &text[pos..end]).unwrap();
            pos = end;
        }
        assert_eq!(pos, text.len(), "tokens must cover the whole text");
        for error in errors {
            writeln!(out, "error {:?}: {}", error.range(), error.message()).unwrap();
        }
        expect.assert_eq(&out);
    }

    #[test]
    fn punctuation() {
        check(
            "{}[]()<>,: :: := . .. +-*/%~|&^@?=#!",
            expect![[r##"
                L_BRACE "{"
                R_BRACE "}"
                L_BRACK "["
                R_BRACK "]"
                L_PAREN "("
                R_PAREN ")"
                L_ANGLE "<"
                R_ANGLE ">"
                COMMA ","
                COLON ":"
                WHITESPACE " "
                COLON2 "::"
                WHITESPACE " "
                COLON_EQ ":="
                WHITESPACE " "
                DOT "."
                WHITESPACE " "
                DOT2 ".."
                WHITESPACE " "
                PLUS "+"
                MINUS "-"
                STAR "*"
                SLASH "/"
                PERCENT "%"
                TILDE "~"
                PIPE "|"
                AMP "&"
                CARET "^"
                AT "@"
                QUESTION "?"
                EQ "="
                POUND "#"
                BANG "!"
            "##]],
        );
    }

    #[test]
    fn triple_dot_is_an_error() {
        check(
            "...x",
            expect![[r#"
                ERROR_TOKEN "..."
                IDENT "x"
                error 0..3: `...` is not valid; a spread is written `..`
            "#]],
        );
    }

    #[test]
    fn identifiers_and_keywords() {
        check(
            "true false self _x _ r#type",
            expect![[r#"
                TRUE_KW "true"
                WHITESPACE " "
                FALSE_KW "false"
                WHITESPACE " "
                IDENT "self"
                WHITESPACE " "
                IDENT "_x"
                WHITESPACE " "
                ERROR_TOKEN "_"
                WHITESPACE " "
                ERROR_TOKEN "r#type"
                error 19..20: `_` is not an identifier
                error 21..27: raw identifiers are not supported
            "#]],
        );
    }

    #[test]
    fn numbers() {
        check(
            "12 0x1F 0o17 0b1010 1_000 1.5 1e3 2.5E-3 1_000.0",
            expect![[r#"
                INT "12"
                WHITESPACE " "
                INT "0x1F"
                WHITESPACE " "
                INT "0o17"
                WHITESPACE " "
                INT "0b1010"
                WHITESPACE " "
                INT "1_000"
                WHITESPACE " "
                FLOAT "1.5"
                WHITESPACE " "
                FLOAT "1e3"
                WHITESPACE " "
                FLOAT "2.5E-3"
                WHITESPACE " "
                FLOAT "1_000.0"
            "#]],
        );
    }

    #[test]
    fn number_errors() {
        check(
            "1u8 1. 0x 0b12 1.5f32",
            expect![[r#"
                INT "1u8"
                WHITESPACE " "
                FLOAT "1."
                WHITESPACE " "
                INT "0x"
                WHITESPACE " "
                INT "0b12"
                WHITESPACE " "
                FLOAT "1.5f32"
                error 0..3: number literals cannot have a suffix
                error 4..6: a float needs a digit after `.`: write `1.0`
                error 7..9: missing digits after the integer base prefix
                error 10..14: invalid digit `2` for a base 2 integer
                error 15..21: number literals cannot have a suffix
            "#]],
        );
    }

    #[test]
    fn tuple_indices_after_dot() {
        check(
            "x.0.1 1.x",
            expect![[r#"
                IDENT "x"
                DOT "."
                INT "0"
                DOT "."
                INT "1"
                WHITESPACE " "
                INT "1"
                DOT "."
                IDENT "x"
            "#]],
        );
    }

    #[test]
    fn strings() {
        check(
            r####""a\n\"\u{1F600}\x7F" r"raw" r#"a "quoted" b"# "multi
line" "skip \
    ws""####,
            expect![[r####"
                STRING "\"a\\n\\\"\\u{1F600}\\x7F\""
                WHITESPACE " "
                RAW_STRING "r\"raw\""
                WHITESPACE " "
                RAW_STRING "r#\"a \"quoted\" b\"#"
                WHITESPACE " "
                STRING "\"multi\nline\""
                WHITESPACE " "
                STRING "\"skip \\\n    ws\""
            "####]],
        );
    }

    #[test]
    fn string_errors() {
        check(
            r#""\q\x80\u{D800}" "open"#,
            expect![[r#"
                STRING "\"\\q\\x80\\u{D800}\""
                WHITESPACE " "
                STRING "\"open"
                error 1..3: unknown escape `\q`
                error 3..7: a `\x` escape must be at most `\x7F`
                error 7..15: a `\u{...}` escape must be a Unicode scalar value
                error 17..18: unterminated string
            "#]],
        );
    }

    #[test]
    fn chars() {
        check(
            r"'a' '\n' '\u{E9}' '' 'ab'",
            expect![[r#"
                CHAR "'a'"
                WHITESPACE " "
                CHAR "'\\n'"
                WHITESPACE " "
                CHAR "'\\u{E9}'"
                WHITESPACE " "
                CHAR "''"
                WHITESPACE " "
                CHAR "'a"
                IDENT "b"
                CHAR "'"
                error 18..20: empty char literal
                error 21..22: unterminated char literal
                error 24..25: unterminated char literal
            "#]],
        );
    }

    #[test]
    fn comments() {
        check(
            "// line\n/* outer /* inner */ still */ x /* open",
            expect![[r#"
                LINE_COMMENT "// line"
                WHITESPACE "\n"
                BLOCK_COMMENT "/* outer /* inner */ still */"
                WHITESPACE " "
                IDENT "x"
                WHITESPACE " "
                BLOCK_COMMENT "/* open"
                error 40..42: unterminated block comment
            "#]],
        );
    }

    #[test]
    fn byte_order_mark() {
        check(
            "\u{FEFF}x",
            expect![[r#"
                BOM "\u{feff}"
                IDENT "x"
            "#]],
        );
    }

    #[test]
    fn unexpected_characters() {
        check(
            "a $ é",
            expect![[r#"
                IDENT "a"
                WHITESPACE " "
                ERROR_TOKEN "$"
                WHITESPACE " "
                ERROR_TOKEN "é"
                error 2..3: unexpected character `$`
                error 4..6: unexpected character `é`
            "#]],
        );
    }
}
