//! The fingerprint: a BLAKE3-256 hash of the canonical encoding of all
//! evaluated data (`SPEC.md` 7.6, appendix B).

use std::collections::{BTreeMap, BTreeSet};

use crate::eval::ContextData;
use crate::value::{Key, Value, ValueKind};
use crate::variants::Variants;

/// The fingerprint of evaluated data: the base root, and each other
/// evaluated context's root. Two data sets with the same values have the
/// same fingerprint, however they were written.
pub fn fingerprint(base: &Value, contexts: &[ContextData], variants: &Variants) -> [u8; 32] {
    let mut out = Vec::new();
    encode_data(&mut out, base, contexts, variants);
    *blake3::hash(&out).as_bytes()
}

/// A fingerprint for each root key, over its value in every evaluated
/// context (`SPEC.md` 7.6), to say which keys differ where the
/// [`fingerprint`]s of two data sets do. Keys are in byte order.
pub fn key_fingerprints(
    base: &Value,
    contexts: &[ContextData],
    variants: &Variants,
) -> BTreeMap<String, [u8; 32]> {
    let mut others: Vec<(Vec<&str>, &ContextData)> = contexts
        .iter()
        .filter(|data| !data.context.is_base())
        .map(|data| (variants.chosen_names(&data.context), data))
        .collect();
    others.sort_by(|a, b| a.0.cmp(&b.0));
    let mut keys: BTreeSet<&Key> = BTreeSet::new();
    if let ValueKind::Keyed(keyed) = &base.kind {
        keys.extend(keyed.entries.keys());
    }
    for (_, data) in &others {
        keys.extend(data.changes.keys());
    }
    let mut found = BTreeMap::new();
    let mut out = Vec::new();
    for key in keys {
        out.clear();
        out.extend_from_slice(b"CROLK1");
        match &base.kind {
            ValueKind::Keyed(keyed) => encode_entry(&mut out, keyed.entries.get(key)),
            _ => encode_entry(&mut out, None),
        }
        for (names, data) in &others {
            // A context only counts where it differs from the base.
            if let Some(change) = data.changes.get(key) {
                leb128(&mut out, names.len() as u128);
                for name in names {
                    encode_name(&mut out, name);
                }
                encode_entry(&mut out, change.as_ref());
            }
        }
        let name = match key {
            Key::Str(name) => name.clone(),
            Key::Int(i) => i.to_string(),
        };
        found.insert(name, *blake3::hash(&out).as_bytes());
    }
    found
}

fn encode_entry(out: &mut Vec<u8>, value: Option<&Value>) {
    match value {
        Some(value) => {
            out.push(1);
            encode(out, value);
        }
        None => out.push(0),
    }
}

/// The fingerprint of a value-mode document's value: it has no root keys or
/// contexts, so the value is encoded as it is, after a header of its own
/// (appendix B).
pub fn fingerprint_value(value: &Value) -> [u8; 32] {
    let mut out = b"CROLV1".to_vec();
    encode(&mut out, value);
    *blake3::hash(&out).as_bytes()
}

/// The canonical encoding of the data (appendix B).
pub fn encode_data(out: &mut Vec<u8>, base: &Value, contexts: &[ContextData], variants: &Variants) {
    // A `..` at the root is the one thing a root has besides its keys; data
    // without one keeps the encoding it always had.
    let base_defaults = matches!(&base.kind, ValueKind::Keyed(keyed) if keyed.defaults);
    let root_defaults = base_defaults || contexts.iter().any(|data| data.defaults);
    if root_defaults {
        out.extend_from_slice(b"CROLR1");
        out.push(u8::from(base_defaults));
    } else {
        out.extend_from_slice(b"CROL1");
    }
    match &base.kind {
        ValueKind::Keyed(keyed) => {
            leb128(out, keyed.entries.len() as u128);
            for (key, value) in &keyed.entries {
                encode_key(out, key);
                encode(out, value);
            }
        }
        _ => leb128(out, 0),
    }
    let mut others: Vec<(Vec<&str>, &ContextData)> = contexts
        .iter()
        .filter(|data| !data.context.is_base())
        .map(|data| (variants.chosen_names(&data.context), data))
        .collect();
    others.sort_by(|a, b| a.0.cmp(&b.0));
    leb128(out, others.len() as u128);
    for (names, data) in others {
        leb128(out, names.len() as u128);
        for name in &names {
            encode_name(out, name);
        }
        // The changes are in root order, with context-only keys after.
        leb128(out, data.changes.len() as u128);
        for (key, change) in &data.changes {
            encode_key(out, key);
            match change {
                None => out.push(0x00),
                Some(value) => {
                    out.push(0x01);
                    encode(out, value);
                }
            }
        }
        if root_defaults {
            out.push(u8::from(data.defaults));
        }
    }
}

fn leb128(out: &mut Vec<u8>, mut n: u128) {
    loop {
        let byte = (n & 0x7F) as u8;
        n >>= 7;
        if n == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn encode_name(out: &mut Vec<u8>, name: &str) {
    leb128(out, name.len() as u128);
    out.extend_from_slice(name.as_bytes());
}

/// A key, encoded as the value it is (tag `0x02` or `0x05`).
fn encode_key(out: &mut Vec<u8>, key: &Key) {
    match key {
        Key::Int(i) => {
            out.push(0x02);
            out.extend_from_slice(&i.to_le_bytes());
        }
        Key::Str(s) => {
            out.push(0x05);
            encode_name(out, s);
        }
    }
}

/// A value: a tag byte and a payload (appendix B).
pub fn encode(out: &mut Vec<u8>, value: &Value) {
    match &value.kind {
        ValueKind::Unit => out.push(0x00),
        ValueKind::Bool(b) => {
            out.push(0x01);
            out.push(u8::from(*b));
        }
        ValueKind::Int(i) => {
            out.push(0x02);
            out.extend_from_slice(&i.to_le_bytes());
        }
        ValueKind::Float(f) => {
            out.push(0x03);
            out.extend_from_slice(&f.to_bits().to_le_bytes());
        }
        ValueKind::Char(c) => {
            out.push(0x04);
            out.extend_from_slice(&u32::from(*c).to_le_bytes());
        }
        ValueKind::String(s) => {
            out.push(0x05);
            encode_name(out, s);
        }
        ValueKind::Id(s) => {
            out.push(0x06);
            encode_name(out, s);
        }
        ValueKind::List(items) => {
            out.push(0x07);
            encode_all(out, items);
        }
        ValueKind::Tuple(items) => {
            out.push(0x08);
            encode_all(out, items);
        }
        ValueKind::UnitName(name) => {
            out.push(0x09);
            encode_name(out, name);
        }
        ValueKind::NamedTuple(name, items) => {
            out.push(0x0A);
            encode_name(out, name);
            encode_all(out, items);
        }
        ValueKind::Keyed(keyed) => {
            out.push(0x0B);
            encode_name(out, keyed.name.as_deref().unwrap_or(""));
            out.push(u8::from(keyed.defaults));
            // `defaulted` is a sorted set: integers ascending, then strings
            // in byte order.
            leb128(out, keyed.defaulted.len() as u128);
            for key in &keyed.defaulted {
                encode_key(out, key);
            }
            leb128(out, keyed.entries.len() as u128);
            for (key, value) in &keyed.entries {
                encode_key(out, key);
                encode(out, value);
            }
        }
    }
}

fn encode_all(out: &mut Vec<u8>, items: &[Value]) {
    leb128(out, items.len() as u128);
    for item in items {
        encode(out, item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Span;

    fn v(kind: ValueKind) -> Value {
        Value::new(kind, Span::detached())
    }

    #[test]
    fn encodings_follow_appendix_b() {
        let mut out = Vec::new();
        leb128(&mut out, 300);
        assert_eq!(out, [0xAC, 0x02]);

        let mut out = Vec::new();
        encode(&mut out, &v(ValueKind::Int(-1)));
        assert_eq!(out[0], 0x02);
        assert_eq!(&out[1..], &[0xFF; 16]);

        let mut out = Vec::new();
        encode(&mut out, &v(ValueKind::String("ab".into())));
        assert_eq!(out, [0x05, 2, b'a', b'b']);

        let mut out = Vec::new();
        encode(&mut out, &v(ValueKind::Char('é')));
        assert_eq!(out, [0x04, 0xE9, 0, 0, 0]);
    }

    #[test]
    fn the_same_values_give_the_same_fingerprint() {
        let a = v(ValueKind::Keyed(crate::value::Keyed {
            entries: [(Key::Str("x".into()), v(ValueKind::Int(1)))]
                .into_iter()
                .collect(),
            ..Default::default()
        }));
        let variants = Variants::new();
        assert_eq!(
            fingerprint(&a, &[], &variants),
            fingerprint(&a.clone(), &[], &variants)
        );
        let b = v(ValueKind::Keyed(crate::value::Keyed {
            entries: [(Key::Str("x".into()), v(ValueKind::Int(2)))]
                .into_iter()
                .collect(),
            ..Default::default()
        }));
        assert_ne!(
            fingerprint(&a, &[], &variants),
            fingerprint(&b, &[], &variants)
        );
    }
}
