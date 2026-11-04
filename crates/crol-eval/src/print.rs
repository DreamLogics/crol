//! Writing values as CROL text that evaluates back to them.

use std::fmt::Write as _;

use crol_syntax::format::{FormatConfig, format};

use crate::eval::ContextData;
use crate::value::{Key, Keyed, Value, ValueKind, format_float, is_identifier, quote, quote_char};
use crate::variants::Variants;

/// Writes a value as a value-mode document, formatted. A keyed value without
/// a type name is written as entries, without outer braces.
pub fn to_document(value: &Value, config: &FormatConfig) -> String {
    let mut text = String::new();
    write_value(&mut text, value, None);
    match format(&text, config) {
        Ok(formatted) => formatted,
        // The printer writes valid syntax; if it ever does not, the
        // unformatted text still says what the value is.
        Err(_) => text + "\n",
    }
}

/// What a printer says about an entry's value, as a comment after it:
/// `limits: Limits { .. }, // default`.
pub type Note<'a> = Option<&'a dyn Fn(&Value) -> Option<String>>;

/// [`to_document`], with a comment after each entry `note` says something
/// about.
pub fn to_document_noted(
    value: &Value,
    config: &FormatConfig,
    note: &dyn Fn(&Value) -> Option<String>,
) -> String {
    let mut text = String::new();
    write_value(&mut text, value, Some(note));
    match format(&text, config) {
        Ok(formatted) => formatted,
        Err(_) => text + "\n",
    }
}

/// Writes data in every evaluated context as `crol eval --all-contexts`
/// does (`SPEC.md` 11): the base data, then for each other context with
/// differences a block whose condition holds in that context only, with the
/// keys whose value differs (`key: value`) or that are missing (`-key`).
/// A `..` at the root is written too: in the base, or in the block of a
/// context that has it where the base does not. The output evaluates back
/// to the same data.
pub fn to_all_contexts(
    base: &Value,
    contexts: &[ContextData],
    variants: &Variants,
    config: &FormatConfig,
) -> String {
    all_contexts(base, contexts, variants, config, false)
}

/// [`to_all_contexts`] for a root that is a struct (`SPEC.md` 4.1), where a
/// removed root key is a field that takes its default: it is written as
/// removed, since the data is complete only with that.
pub fn to_all_contexts_of_struct(
    base: &Value,
    contexts: &[ContextData],
    variants: &Variants,
    config: &FormatConfig,
) -> String {
    all_contexts(base, contexts, variants, config, true)
}

fn write_removed(out: &mut String, key: &Key) {
    write_key(out, key);
    out.push_str(": (), -");
    write_key(out, key);
    out.push_str(",\n");
}

fn all_contexts(
    base: &Value,
    contexts: &[ContextData],
    variants: &Variants,
    config: &FormatConfig,
    removed: bool,
) -> String {
    let mut text = String::new();
    let empty = Keyed::default();
    let keyed = match &base.kind {
        ValueKind::Keyed(keyed) => keyed,
        _ => &empty,
    };
    for (key, value) in &keyed.entries {
        write_key(&mut text, key);
        text.push_str(": ");
        write_value(&mut text, value, None);
        text.push_str(",\n");
    }
    if removed {
        for key in &keyed.defaulted {
            write_removed(&mut text, key);
        }
    }
    if keyed.defaults {
        text.push_str("..,\n");
    }
    for data in contexts {
        let defaults = data.defaults && !keyed.defaults;
        // Removed in the context, and neither held nor removed in the base,
        // so not among its changes.
        let only_here: Vec<&Key> = data
            .defaulted
            .iter()
            .filter(|key| {
                removed && !keyed.defaulted.contains(*key) && !keyed.entries.contains_key(*key)
            })
            .collect();
        if data.changes.is_empty() && !defaults && only_here.is_empty() {
            continue;
        }
        let mut body = String::new();
        for (key, change) in &data.changes {
            match change {
                None => {
                    body.push('-');
                    write_key(&mut body, key);
                }
                Some(value) => {
                    write_key(&mut body, key);
                    body.push_str(": ");
                    write_value(&mut body, value, None);
                }
            }
            body.push_str(",\n");
        }
        for key in only_here {
            write_removed(&mut body, key);
        }
        if defaults {
            body.push_str("..,\n");
        }
        let condition: Vec<String> = variants
            .ids()
            .map(|id| {
                let name = variants.name(id);
                if data.context.is_active(id) {
                    name.to_owned()
                } else {
                    format!("!{name}")
                }
            })
            .collect();
        text.push_str(&format!("\n<{}> {{\n{body}}},\n", condition.join(" & ")));
    }
    match format(&text, config) {
        Ok(formatted) => formatted,
        Err(_) => text,
    }
}

/// Writes a value on one line, unformatted.
pub fn to_text(value: &Value) -> String {
    let mut text = String::new();
    write_value(&mut text, value, None);
    text
}

fn write_value(out: &mut String, value: &Value, note: Note<'_>) {
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
            write_elements(out, elements, note);
            out.push(']');
        }
        ValueKind::Tuple(elements) => {
            out.push('(');
            write_elements(out, elements, note);
            if elements.len() == 1 {
                out.push(',');
            }
            out.push(')');
        }
        ValueKind::UnitName(name) => out.push_str(name),
        ValueKind::NamedTuple(name, elements) => {
            out.push_str(name);
            out.push('(');
            write_elements(out, elements, note);
            out.push(')');
        }
        ValueKind::Keyed(keyed) => {
            if let Some(name) = &keyed.name {
                out.push_str(name);
                out.push(' ');
            }
            out.push('{');
            // Whether the next entry needs a `, ` before it.
            let mut after_entry = false;
            let separator = |out: &mut String, after_entry: &mut bool| {
                if std::mem::replace(after_entry, true) {
                    out.push_str(", ");
                }
            };
            for (key, value) in &keyed.entries {
                separator(out, &mut after_entry);
                write_key(out, key);
                out.push_str(": ");
                write_value(out, value, note);
                // A note is a comment after the entry, which breaks the block.
                if let Some(text) = note.and_then(|note| note(value)) {
                    let _ = writeln!(out, ", // {text}");
                    after_entry = false;
                }
            }
            // A defaulted field is one removed with `-field`; it must exist
            // to be removed.
            for key in &keyed.defaulted {
                separator(out, &mut after_entry);
                write_key(out, key);
                out.push_str(": (), -");
                write_key(out, key);
            }
            if keyed.defaults {
                separator(out, &mut after_entry);
                out.push_str("..");
            }
            out.push('}');
        }
    }
}

fn write_key(out: &mut String, key: &Key) {
    let _ = write!(out, "{key}");
}

fn write_elements(out: &mut String, elements: &[Value], note: Note<'_>) {
    for (i, element) in elements.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        write_value(out, element, note);
    }
}
