//! Evaluated values (`SPEC.md` section 3).

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use indexmap::IndexMap;

use crate::source::Span;

/// A key of a keyed value: a string or an integer (`SPEC.md` 3.1). Integer
/// keys sort before string keys, as in the canonical encoding.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Key {
    /// An integer key: `12: "a"`, `[-3]: "a"`.
    Int(i128),
    /// A string key: `port: 80`, `"cache url": 1`. Values with the same
    /// field share its name.
    Str(Arc<str>),
}

/// A borrowed [`Key`], for looking up entries without allocating. It hashes
/// as the `Key` it stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum KeyRef<'a> {
    Int(i128),
    Str(&'a str),
}

impl indexmap::Equivalent<Key> for KeyRef<'_> {
    fn equivalent(&self, key: &Key) -> bool {
        match (self, key) {
            (KeyRef::Int(a), Key::Int(b)) => a == b,
            (KeyRef::Str(a), Key::Str(b)) => *a == &**b,
            _ => false,
        }
    }
}

impl Key {
    /// Whether the key can be written bare: a string that is an identifier
    /// and not reserved.
    pub fn is_identifier(&self) -> bool {
        match self {
            Key::Str(s) => is_identifier(s),
            Key::Int(_) => false,
        }
    }
}

/// Whether `text` is a CROL identifier that is not reserved (`SPEC.md` 2.3).
pub fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !matches!(text, "_" | "true" | "false")
}

/// Displays a key as an entry key: `port`, `"cache url"`, `12`, `[-3]`.
impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Key::Str(s) if is_identifier(s) => f.write_str(s),
            Key::Str(s) => write!(f, "{}", quote(s)),
            Key::Int(i) if *i < 0 => write!(f, "[{i}]"),
            Key::Int(i) => write!(f, "{i}"),
        }
    }
}

/// A string literal for `s`, with escapes.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\0"),
            c if c.is_control() => out.push_str(&format!("\\u{{{:X}}}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A char literal for `c`, with escapes.
pub fn quote_char(c: char) -> String {
    match c {
        '\'' => "'\\''".to_owned(),
        '\\' => "'\\\\'".to_owned(),
        '\n' => "'\\n'".to_owned(),
        '\r' => "'\\r'".to_owned(),
        '\t' => "'\\t'".to_owned(),
        '\0' => "'\\0'".to_owned(),
        c if c.is_control() => format!("'\\u{{{:X}}}'", c as u32),
        c => format!("'{c}'"),
    }
}

/// Formats a float as CROL writes it (`SPEC.md` 6.6): the shortest text
/// that reads back as the same value, with `.0` if it would otherwise look
/// like an integer.
pub fn format_float(f: f64) -> String {
    // Rust's `{:?}` is the shortest round-trip form, and adds `.0`.
    format!("{f:?}")
}

/// A keyed value: a struct, struct variant or map (`SPEC.md` 3.1).
#[derive(Debug, Clone, Default)]
pub struct Keyed {
    /// The type name, `Service` in `Service { ... }`.
    pub name: Option<String>,
    /// The entries, in insertion order.
    pub entries: IndexMap<Key, Value>,
    /// Set by `..`: fields the value does not hold take their defaults.
    pub defaults: bool,
    /// Fields removed with `-field`, which take their defaults.
    pub defaulted: BTreeSet<Key>,
}

/// The kind and contents of a [`Value`].
#[derive(Debug, Clone)]
pub enum ValueKind {
    /// `()`
    Unit,
    /// `true`, `false`
    Bool(bool),
    /// An integer; 128-bit during evaluation.
    Int(i128),
    /// A float; always finite.
    Float(f64),
    /// `'a'`
    Char(char),
    /// `"a"`
    String(String),
    /// `@web`: a root key, as a value (`SPEC.md` 3.2).
    Id(String),
    /// `[a, b]`
    List(Vec<Value>),
    /// `(a, b)`, `(a,)`
    Tuple(Vec<Value>),
    /// `None`, `Red`: a unit variant or unit struct.
    UnitName(String),
    /// `Some(1)`, `Rgb(1, 2, 3)`
    NamedTuple(String, Vec<Value>),
    /// `{ ... }`, `Service { ... }`
    Keyed(Box<Keyed>),
}

/// An evaluated value, with the span of the expression that produced it.
#[derive(Debug, Clone)]
pub struct Value {
    /// What the value is.
    pub kind: ValueKind,
    /// Where it was written.
    pub span: Span,
}

impl Value {
    /// Creates a value.
    pub fn new(kind: ValueKind, span: Span) -> Self {
        Self { kind, span }
    }

    /// The name of the value's kind, for messages: `an integer`, `a list`.
    pub fn kind_name(&self) -> &'static str {
        self.kind.kind_name()
    }

    /// Equality as `SPEC.md` 3.3 defines it: the same kind and structurally
    /// equal. Integers and floats are never equal; keyed values compare
    /// their type names and entries as sets, ignoring entry order, the
    /// defaults flag and defaulted fields.
    pub fn equals(&self, other: &Value) -> bool {
        use ValueKind::*;
        match (&self.kind, &other.kind) {
            (Unit, Unit) => true,
            (Bool(a), Bool(b)) => a == b,
            (Int(a), Int(b)) => a == b,
            (Float(a), Float(b)) => a == b,
            (Char(a), Char(b)) => a == b,
            (String(a), String(b)) | (Id(a), Id(b)) | (UnitName(a), UnitName(b)) => a == b,
            (List(a), List(b)) | (Tuple(a), Tuple(b)) => all_equal(a, b, Value::equals),
            (NamedTuple(n, a), NamedTuple(m, b)) => n == m && all_equal(a, b, Value::equals),
            (Keyed(a), Keyed(b)) => {
                a.name == b.name
                    && a.entries.len() == b.entries.len()
                    && a.entries
                        .iter()
                        .all(|(k, v)| b.entries.get(k).is_some_and(|w| v.equals(w)))
            }
            _ => false,
        }
    }

    /// Identity (`SPEC.md` 3.3): equal, with the same entry order, defaults
    /// flags and defaulted fields, and floats with the same bits.
    pub fn identical(&self, other: &Value) -> bool {
        use ValueKind::*;
        match (&self.kind, &other.kind) {
            (Float(a), Float(b)) => a.to_bits() == b.to_bits(),
            (List(a), List(b)) | (Tuple(a), Tuple(b)) => all_equal(a, b, Value::identical),
            (NamedTuple(n, a), NamedTuple(m, b)) => n == m && all_equal(a, b, Value::identical),
            (Keyed(a), Keyed(b)) => {
                a.name == b.name
                    && a.defaults == b.defaults
                    && a.defaulted == b.defaulted
                    && a.entries.len() == b.entries.len()
                    && a.entries
                        .iter()
                        .zip(&b.entries)
                        .all(|((k, v), (l, w))| k == l && v.identical(w))
            }
            _ => self.equals(other),
        }
    }
}

fn all_equal(a: &[Value], b: &[Value], eq: fn(&Value, &Value) -> bool) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| eq(x, y))
}

impl ValueKind {
    /// The name of the kind, for messages: `an integer`, `a list`.
    pub fn kind_name(&self) -> &'static str {
        match self {
            ValueKind::Unit => "the unit value",
            ValueKind::Bool(_) => "a boolean",
            ValueKind::Int(_) => "an integer",
            ValueKind::Float(_) => "a float",
            ValueKind::Char(_) => "a char",
            ValueKind::String(_) => "a string",
            ValueKind::Id(_) => "an id",
            ValueKind::List(_) => "a list",
            ValueKind::Tuple(_) => "a tuple",
            ValueKind::UnitName(_) => "a unit name",
            ValueKind::NamedTuple(..) => "a named tuple",
            ValueKind::Keyed(_) => "a keyed value",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrowed_keys_find_entries() {
        let mut map = IndexMap::new();
        map.insert(Key::Str("port".into()), 1);
        map.insert(Key::Int(3), 2);
        assert_eq!(map.get(&KeyRef::Str("port")), Some(&1));
        assert_eq!(map.get(&KeyRef::Int(3)), Some(&2));
        assert_eq!(map.get(&KeyRef::Str("3")), None);
    }

    #[test]
    fn floats_format_as_spec_6_6() {
        assert_eq!(format_float(1.0), "1.0");
        assert_eq!(format_float(0.1), "0.1");
        assert_eq!(format_float(1e30), "1e30");
        assert_eq!(format_float(-2.5), "-2.5");
        assert_eq!(format_float(1e-7), "1e-7");
    }

    #[test]
    fn keys_display_as_written() {
        assert_eq!(Key::Str("port".into()).to_string(), "port");
        assert_eq!(Key::Str("cache url".into()).to_string(), "\"cache url\"");
        assert_eq!(Key::Str("true".into()).to_string(), "\"true\"");
        assert_eq!(Key::Int(12).to_string(), "12");
        assert_eq!(Key::Int(-3).to_string(), "[-3]");
        assert!(Key::Int(1) < Key::Str("a".into()));
    }
}
