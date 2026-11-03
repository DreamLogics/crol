//! Writing values as CROL text that evaluates back to them.

use std::fmt::Write as _;

use crol_syntax::format::{FormatConfig, format};

use crate::value::{Key, Value, ValueKind, format_float, is_identifier, quote, quote_char};

/// Writes a value as a value-mode document, formatted. A keyed value without
/// a type name is written as entries, without outer braces.
pub fn to_document(value: &Value, config: &FormatConfig) -> String {
    let mut text = String::new();
    write_value(&mut text, value);
    match format(&text, config) {
        Ok(formatted) => formatted,
        // The printer writes valid syntax; if it ever does not, the
        // unformatted text still says what the value is.
        Err(_) => text + "\n",
    }
}

/// Writes a value on one line, unformatted.
pub fn to_text(value: &Value) -> String {
    let mut text = String::new();
    write_value(&mut text, value);
    text
}

fn write_value(out: &mut String, value: &Value) {
    match &value.kind {
        ValueKind::Unit => out.push_str("()"),
        ValueKind::Bool(b) => {
            let _ = write!(out, "{b}");
        }
        ValueKind::Int(i) => {
            let _ = write!(out, "{i}");
        }
        ValueKind::Float(f) => out.push_str(&format_float(*f)),
        ValueKind::Char(c) => out.push_str(&quote_char(*c)),
        ValueKind::String(s) => out.push_str(&quote(s)),
        ValueKind::Id(id) if is_identifier(id) => {
            let _ = write!(out, "@{id}");
        }
        ValueKind::Id(id) => {
            let _ = write!(out, "@{}", quote(id));
        }
        ValueKind::List(elements) => {
            out.push('[');
            write_elements(out, elements);
            out.push(']');
        }
        ValueKind::Tuple(elements) => {
            out.push('(');
            write_elements(out, elements);
            if elements.len() == 1 {
                out.push(',');
            }
            out.push(')');
        }
        ValueKind::UnitName(name) => out.push_str(name),
        ValueKind::NamedTuple(name, elements) => {
            out.push_str(name);
            out.push('(');
            write_elements(out, elements);
            out.push(')');
        }
        ValueKind::Keyed(keyed) => {
            if let Some(name) = &keyed.name {
                out.push_str(name);
                out.push(' ');
            }
            out.push('{');
            let mut first = true;
            let mut separator = |out: &mut String| {
                if !std::mem::take(&mut first) {
                    out.push_str(", ");
                }
            };
            for (key, value) in &keyed.entries {
                separator(out);
                write_key(out, key);
                out.push_str(": ");
                write_value(out, value);
            }
            // A defaulted field is one removed with `-field`; it must exist
            // to be removed.
            for key in &keyed.defaulted {
                separator(out);
                write_key(out, key);
                out.push_str(": (), -");
                write_key(out, key);
            }
            if keyed.defaults {
                separator(out);
                out.push_str("..");
            }
            out.push('}');
        }
    }
}

fn write_key(out: &mut String, key: &Key) {
    let _ = write!(out, "{key}");
}

fn write_elements(out: &mut String, elements: &[Value]) {
    for (i, element) in elements.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        write_value(out, element);
    }
}
