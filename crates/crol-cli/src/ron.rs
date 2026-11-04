//! A RON reader for `crol from-ron` (`SPEC.md` 11).
//!
//! RON has its own parser here because converting needs what `ron::Value`
//! drops: struct and variant names. Structs become blocks ending in `..`,
//! since RON lets a struct leave out fields that have serde defaults and the
//! converted document must load the same way. Comments are not kept.

use crol_eval::{Key, Keyed, Span, Value, ValueKind};
use crol_syntax::{LineIndex, TextSize};
use indexmap::IndexMap;

/// A RON syntax error, with its line and column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RonError {
    pub(crate) line: u32,
    pub(crate) col: u32,
    pub(crate) message: String,
}

/// Parses a RON value file.
pub(crate) fn parse(text: &str) -> std::result::Result<Value, RonError> {
    let mut parser = Parser { text, pos: 0 };
    let result = parser.document();
    result.map_err(|(pos, message)| {
        let index = LineIndex::new(text);
        let lc = index.line_col(text, TextSize::try_from(pos).unwrap_or_default());
        RonError {
            line: lc.line,
            col: lc.col,
            message,
        }
    })
}

type Result<T> = std::result::Result<T, (usize, String)>;

struct Parser<'t> {
    text: &'t str,
    pos: usize,
}

fn value(kind: ValueKind) -> Value {
    Value::new(kind, Span::detached())
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Whether `text` starts with a raw string, `r"` or `r#"` (not the raw
/// identifier `r#type`).
fn is_raw_string(text: &str) -> bool {
    text.strip_prefix('r')
        .is_some_and(|rest| rest.trim_start_matches('#').starts_with('"'))
}

impl Parser<'_> {
    fn rest(&self) -> &str {
        &self.text[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn fail<T>(&self, message: impl Into<String>) -> Result<T> {
        Err((self.pos, message.into()))
    }

    fn skip_trivia(&mut self) -> Result<()> {
        loop {
            let rest = self.rest();
            if rest.starts_with("//") {
                self.pos += rest.find('\n').unwrap_or(rest.len());
            } else if rest.starts_with("/*") {
                let start = self.pos;
                self.pos += 2;
                let mut depth = 1;
                while depth > 0 {
                    if self.rest().starts_with("/*") {
                        self.pos += 2;
                        depth += 1;
                    } else if self.rest().starts_with("*/") {
                        self.pos += 2;
                        depth -= 1;
                    } else if self.bump().is_none() {
                        return Err((start, "unterminated block comment".to_owned()));
                    }
                }
            } else if self.peek().is_some_and(char::is_whitespace) {
                self.bump();
            } else {
                return Ok(());
            }
        }
    }

    fn eat(&mut self, c: char) -> Result<bool> {
        self.skip_trivia()?;
        if self.peek() == Some(c) {
            self.bump();
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn expect(&mut self, c: char) -> Result<()> {
        if self.eat(c)? {
            Ok(())
        } else {
            self.fail(format!("expected `{c}`"))
        }
    }

    fn document(&mut self) -> Result<Value> {
        // Extensions such as `#![enable(implicit_some)]`: CROL accepts bare
        // values for options anyway, so they are skipped.
        loop {
            self.skip_trivia()?;
            if !self.rest().starts_with("#!") {
                break;
            }
            match self.rest().find(']') {
                Some(end) => self.pos += end + 1,
                None => return self.fail("unterminated attribute"),
            }
        }
        let value = self.value()?;
        self.skip_trivia()?;
        if self.pos < self.text.len() {
            return self.fail("expected the end of the file");
        }
        Ok(value)
    }

    fn value(&mut self) -> Result<Value> {
        self.skip_trivia()?;
        let Some(c) = self.peek() else {
            return self.fail("expected a value");
        };
        match c {
            '[' => {
                self.bump();
                let elements = self.values(']')?;
                Ok(value(ValueKind::List(elements)))
            }
            '{' => self.map(),
            '(' => self.parens(None),
            '"' => Ok(value(ValueKind::String(self.string()?.into()))),
            '\'' => self.char(),
            'b' if is_raw_string(&self.rest()[1..]) || self.rest().starts_with("b\"") => {
                self.bump();
                let text = if self.peek() == Some('r') {
                    self.raw_string()?
                } else {
                    self.string()?
                };
                let bytes = text
                    .bytes()
                    .map(|b| value(ValueKind::Int(b.into())))
                    .collect();
                Ok(value(ValueKind::List(bytes)))
            }
            'r' if is_raw_string(self.rest()) => {
                Ok(value(ValueKind::String(self.raw_string()?.into())))
            }
            c if c.is_ascii_digit() || matches!(c, '+' | '-' | '.') => self.number(),
            c if is_ident_start(c) => {
                let start = self.pos;
                let name = self.identifier()?;
                match name.as_str() {
                    "true" => return Ok(value(ValueKind::Bool(true))),
                    "false" => return Ok(value(ValueKind::Bool(false))),
                    "inf" | "NaN" => {
                        return Err((
                            start,
                            "CROL floats are finite; `inf` and `NaN` cannot be converted"
                                .to_owned(),
                        ));
                    }
                    _ => {}
                }
                self.skip_trivia()?;
                if self.peek() == Some('(') {
                    self.parens(Some(name))
                } else {
                    Ok(value(ValueKind::UnitName(name.into())))
                }
            }
            c => self.fail(format!("unexpected `{c}`")),
        }
    }

    fn identifier(&mut self) -> Result<String> {
        if self.rest().starts_with("r#") {
            self.pos += 2;
        }
        let start = self.pos;
        while self.peek().is_some_and(is_ident_continue) {
            self.bump();
        }
        if start == self.pos {
            return self.fail("expected an identifier");
        }
        Ok(self.text[start..self.pos].to_owned())
    }

    /// Comma-separated values up to `close`.
    fn values(&mut self, close: char) -> Result<Vec<Value>> {
        let mut values = Vec::new();
        loop {
            if self.eat(close)? {
                return Ok(values);
            }
            values.push(self.value()?);
            if !self.eat(',')? {
                self.expect(close)?;
                return Ok(values);
            }
        }
    }

    fn map(&mut self) -> Result<Value> {
        self.bump(); // `{`
        let mut entries = IndexMap::new();
        loop {
            if self.eat('}')? {
                break;
            }
            self.skip_trivia()?;
            let key_pos = self.pos;
            let key = match self.value()?.kind {
                ValueKind::String(s) => Key::Str(s),
                ValueKind::Int(i) => Key::Int(i),
                ValueKind::UnitName(name) => Key::Str(name),
                other => {
                    return Err((
                        key_pos,
                        format!(
                            "CROL map keys are strings, integers or unit variants; {} cannot be a key",
                            other.kind_name()
                        ),
                    ));
                }
            };
            self.expect(':')?;
            entries.insert(key, self.value()?);
            if !self.eat(',')? {
                self.expect('}')?;
                break;
            }
        }
        Ok(value(ValueKind::Keyed(Box::new(Keyed {
            entries,
            ..Keyed::default()
        }))))
    }

    /// `(...)` after a name or on its own: a struct with named fields, a
    /// tuple, or the unit value.
    fn parens(&mut self, name: Option<String>) -> Result<Value> {
        self.bump(); // `(`
        if self.is_struct()? {
            let mut entries = IndexMap::new();
            loop {
                if self.eat(')')? {
                    break;
                }
                self.skip_trivia()?;
                let field = self.identifier()?;
                self.expect(':')?;
                entries.insert(Key::Str(field.into()), self.value()?);
                if !self.eat(',')? {
                    self.expect(')')?;
                    break;
                }
            }
            return Ok(value(ValueKind::Keyed(Box::new(Keyed {
                name: name.map(Into::into),
                entries,
                defaults: true,
                defaulted: Default::default(),
            }))));
        }
        let elements = self.values(')')?;
        Ok(value(match name {
            Some(name) => ValueKind::NamedTuple(name.into(), elements),
            None if elements.is_empty() => ValueKind::Unit,
            None => ValueKind::Tuple(elements),
        }))
    }

    /// Whether the parentheses just opened hold named fields: an identifier
    /// followed by `:` (not `::`).
    fn is_struct(&mut self) -> Result<bool> {
        self.skip_trivia()?;
        let saved = self.pos;
        let mut result = false;
        if self.peek().is_some_and(is_ident_start) || self.rest().starts_with("r#") {
            let _ = self.identifier();
            self.skip_trivia()?;
            result = self.peek() == Some(':') && !self.rest().starts_with("::");
        }
        self.pos = saved;
        Ok(result)
    }

    fn number(&mut self) -> Result<Value> {
        let start = self.pos;
        let negative = match self.peek() {
            Some('-') => {
                self.bump();
                true
            }
            Some('+') => {
                self.bump();
                false
            }
            _ => false,
        };
        if self.rest().starts_with("inf") || self.rest().starts_with("NaN") {
            return self.fail("CROL floats are finite; `inf` and `NaN` cannot be converted");
        }
        let radix = match self.rest().get(..2) {
            Some("0x") => 16,
            Some("0o") => 8,
            Some("0b") => 2,
            _ => 10,
        };
        if radix != 10 {
            self.pos += 2;
            let digits_start = self.pos;
            while self.peek().is_some_and(|c| c.is_digit(radix) || c == '_') {
                self.bump();
            }
            let digits: String = self.text[digits_start..self.pos]
                .chars()
                .filter(|&c| c != '_')
                .collect();
            self.skip_suffix();
            let signed = if negative {
                format!("-{digits}")
            } else {
                digits
            };
            return i128::from_str_radix(&signed, radix)
                .map(|i| value(ValueKind::Int(i)))
                .map_err(|_| (start, "integer out of range".to_owned()));
        }
        let mut is_float = false;
        while let Some(c) = self.peek() {
            match c {
                '0'..='9' | '_' => {}
                '.' => is_float = true,
                'e' | 'E' => {
                    is_float = true;
                    self.bump();
                    if matches!(self.peek(), Some('+' | '-')) {
                        self.bump();
                    }
                    continue;
                }
                _ => break,
            }
            self.bump();
        }
        let digits: String = self.text[start..self.pos]
            .chars()
            .filter(|&c| c != '_' && c != '+')
            .collect();
        if self.rest().starts_with('f') {
            is_float = true;
        }
        self.skip_suffix();
        if is_float {
            // RON accepts `1.` and `.5`; Rust's parser takes both.
            match digits.parse::<f64>() {
                Ok(f) if f.is_finite() => Ok(value(ValueKind::Float(f))),
                Ok(_) => Err((
                    start,
                    "CROL floats are finite; this float is too large".to_owned(),
                )),
                Err(_) => Err((start, format!("invalid number `{digits}`"))),
            }
        } else {
            digits
                .parse::<i128>()
                .map(|i| value(ValueKind::Int(i)))
                .map_err(|_| (start, format!("invalid integer `{digits}`")))
        }
    }

    /// `1u8`, `2.0f32`: CROL has no suffixes; the type is the Rust field's.
    fn skip_suffix(&mut self) {
        while self.peek().is_some_and(is_ident_continue) {
            self.bump();
        }
    }

    fn string(&mut self) -> Result<String> {
        let start = self.pos;
        self.bump(); // `"`
        let mut out = String::new();
        loop {
            match self.bump() {
                None => return Err((start, "unterminated string".to_owned())),
                Some('"') => return Ok(out),
                Some('\\') => out.push_str(&self.escape()?),
                Some(c) => out.push(c),
            }
        }
    }

    fn raw_string(&mut self) -> Result<String> {
        let start = self.pos;
        self.bump(); // `r`
        let hashes = self.rest().bytes().take_while(|&b| b == b'#').count();
        self.pos += hashes;
        if self.bump() != Some('"') {
            return Err((start, "expected `\"`".to_owned()));
        }
        let close: String = std::iter::once('"')
            .chain(std::iter::repeat_n('#', hashes))
            .collect();
        match self.rest().find(&close) {
            Some(end) => {
                let text = self.rest()[..end].to_owned();
                self.pos += end + close.len();
                Ok(text)
            }
            None => Err((start, "unterminated raw string".to_owned())),
        }
    }

    fn char(&mut self) -> Result<Value> {
        let start = self.pos;
        self.bump(); // `'`
        let c = match self.bump() {
            Some('\\') => self.escape()?,
            Some(c) => c.to_string(),
            None => return Err((start, "unterminated char".to_owned())),
        };
        if self.bump() != Some('\'') {
            return Err((start, "unterminated char".to_owned()));
        }
        let mut chars = c.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => Ok(value(ValueKind::Char(c))),
            _ => Err((start, "a char holds one character".to_owned())),
        }
    }

    fn escape(&mut self) -> Result<String> {
        let start = self.pos - 1;
        let c = match self.bump() {
            Some('n') => '\n',
            Some('r') => '\r',
            Some('t') => '\t',
            Some('0') => '\0',
            Some('\\') => '\\',
            Some('"') => '"',
            Some('\'') => '\'',
            Some('x') => {
                let digits = self.rest().get(..2).unwrap_or_default().to_owned();
                self.pos += digits.len();
                u8::from_str_radix(&digits, 16)
                    .ok()
                    .map(char::from)
                    .ok_or((start, "invalid `\\x` escape".to_owned()))?
            }
            Some('u') => {
                if self.bump() != Some('{') {
                    return Err((start, "invalid `\\u` escape".to_owned()));
                }
                let end = self
                    .rest()
                    .find('}')
                    .ok_or((start, "invalid `\\u` escape".to_owned()))?;
                let digits = self.rest()[..end].replace('_', "");
                self.pos += end + 1;
                u32::from_str_radix(&digits, 16)
                    .ok()
                    .and_then(char::from_u32)
                    .ok_or((start, "invalid `\\u` escape".to_owned()))?
            }
            Some('\n') => {
                while self.peek().is_some_and(char::is_whitespace) {
                    self.bump();
                }
                return Ok(String::new());
            }
            _ => return Err((start, "unknown escape".to_owned())),
        };
        Ok(c.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crol_eval::print;

    fn convert(ron: &str) -> String {
        match parse(ron) {
            Ok(value) => print::to_document(&value, &Default::default()),
            Err(e) => format!("{}:{}: {}\n", e.line, e.col, e.message),
        }
    }

    #[test]
    fn structs_tuples_and_enums() {
        let ron = r##"
            #![enable(implicit_some)]
            // A comment.
            Config(
                name: "app",
                port: 8_080u16,
                ratio: .5,
                whole: 1.,
                hex: 0xFF,
                neg: -3,
                tags: ["a", r#"b "q""#],
                point: (1, 2),
                one: (1),
                unit: (),
                color: Rgb(1, 2, 3),
                mode: Fast,
                inner: (x: 1),
                opt: Some(None),
                c: '\u{E9}',
                bytes: b"hi",
                map: { "a": 1, 2: "b", Red: 3 },
                r#type: true,
            )
        "##;
        expect_test::expect![[r#"
            Config {
                name: "app",
                port: 8080,
                ratio: 0.5,
                whole: 1.0,
                hex: 255,
                neg: -3,
                tags: ["a", "b \"q\""],
                point: (1, 2),
                one: (1,),
                unit: (),
                color: Rgb(1, 2, 3),
                mode: Fast,
                inner: { x: 1, .. },
                opt: Some(None),
                c: 'é',
                bytes: [104, 105],
                map: { a: 1, 2: "b", Red: 3 },
                type: true,
                ..,
            }
        "#]]
        .assert_eq(&convert(ron));
    }

    #[test]
    fn errors() {
        assert_eq!(
            convert("(a: inf)"),
            "1:5: CROL floats are finite; `inf` and `NaN` cannot be converted\n"
        );
        assert_eq!(
            convert("{ (1, 2): 3 }"),
            "1:3: CROL map keys are strings, integers or unit variants; a tuple cannot be a key\n"
        );
        assert_eq!(convert("[1, 2"), "1:6: expected `]`\n");
    }
}
