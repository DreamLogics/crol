//! Writing data documents (`SPEC.md` 10.8): `.crod` text with operations,
//! for programs that generate a layer.

use std::fmt::Write as _;

use crol_eval::{Key, Keyed, Value, ValueKind, print};
use crol_syntax::format::{FormatConfig, format};
use serde::Serialize;

use crate::error::Error;
use crate::ser::{self, PrettyConfig};

/// Writes a data-mode document, entry by entry. Values are anything
/// `Serialize`, written as [`crate::to_string`] writes them, so a struct that
/// leaves fields out with `skip_serializing_if` gets `..`.
///
/// ```
/// # fn main() -> Result<(), crol::Error> {
/// let mut writer = crol::DataWriter::new();
/// writer.declare("port", &80)?;
/// writer.variant("prod", |w| w.set("port", &443))?;
/// assert_eq!(writer.finish()?, "+port: 80\n<prod> {\n    port: 443,\n}\n");
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default, Clone)]
pub struct DataWriter {
    config: PrettyConfig,
    text: String,
}

impl DataWriter {
    /// A writer with the default [`PrettyConfig`].
    pub fn new() -> Self {
        Self::default()
    }

    /// A writer with these options.
    pub fn with_config(config: PrettyConfig) -> Self {
        Self {
            config,
            text: String::new(),
        }
    }

    fn value<T: Serialize + ?Sized>(&self, value: &T) -> Result<Value, Error> {
        ser::to_value(value, &self.config)
    }

    fn entry(&mut self, prefix: &str, target: &str, value: &str) {
        let _ = writeln!(self.text, "{prefix}{target}: {value},");
    }

    /// `+key: value`.
    pub fn declare<T: Serialize + ?Sized>(&mut self, key: &str, value: &T) -> Result<(), Error> {
        let value = print::to_text(&self.value(value)?);
        self.entry("+", &root_key(key), &value);
        Ok(())
    }

    /// `key: value`.
    pub fn set<T: Serialize + ?Sized>(&mut self, key: &str, value: &T) -> Result<(), Error> {
        let value = print::to_text(&self.value(value)?);
        self.entry("", &root_key(key), &value);
        Ok(())
    }

    /// `?key: value`.
    pub fn ensure<T: Serialize + ?Sized>(&mut self, key: &str, value: &T) -> Result<(), Error> {
        let value = print::to_text(&self.value(value)?);
        self.entry("?", &root_key(key), &value);
        Ok(())
    }

    /// `name := value`, a template.
    pub fn template<T: Serialize + ?Sized>(&mut self, name: &str, value: &T) -> Result<(), Error> {
        let value = print::to_text(&self.value(value)?);
        let _ = writeln!(self.text, "{name} := {value},");
        Ok(())
    }

    /// `-key`.
    pub fn remove(&mut self, key: &str) {
        let _ = writeln!(self.text, "-{},", root_key(key));
    }

    /// `key: & ~ value`: a patch with what `value` holds, which may leave
    /// fields out. A field left out is not changed, so `value` gets no `..`.
    pub fn patch<T: Serialize + ?Sized>(&mut self, key: &str, value: &T) -> Result<(), Error> {
        let mut value = self.value(value)?;
        clear_defaults(&mut value);
        let text = print::to_text(&value);
        self.entry("", &root_key(key), &format!("& ~ {text}"));
        Ok(())
    }

    /// The smallest entry that makes `from` into `to` at `key`: a patch of
    /// what differs, or nothing if they are equal. Where `to` cannot be
    /// merged into `from`, the entry sets it.
    pub fn diff<T: Serialize + ?Sized>(
        &mut self,
        key: &str,
        from: &T,
        to: &T,
    ) -> Result<(), Error> {
        let (from, to) = (self.value(from)?, self.value(to)?);
        if from.equals(&to) && same_marks(&from, &to) {
            return Ok(());
        }
        let key = root_key(key);
        match (&from.kind, &to.kind) {
            (ValueKind::Keyed(a), ValueKind::Keyed(b)) if mergeable(a, b) => {
                let name = b.name.as_ref().map_or(String::new(), |n| format!("{n} "));
                self.entry("", &key, &format!("& ~ {name}{{ {} }}", diff_entries(a, b)));
            }
            _ => self.entry("", &key, &print::to_text(&to)),
        }
        Ok(())
    }

    /// `<condition> { ... }`, with the entries `body` writes. An empty block
    /// is left out.
    pub fn variant<F>(&mut self, condition: &str, body: F) -> Result<(), Error>
    where
        F: FnOnce(&mut DataWriter) -> Result<(), Error>,
    {
        let mut inner = Self::with_config(self.config.clone());
        body(&mut inner)?;
        if !inner.text.is_empty() {
            let _ = writeln!(self.text, "<{condition}> {{\n{}}}", inner.text);
        }
        Ok(())
    }

    /// `// text`, one comment for each line of `text`.
    pub fn comment(&mut self, text: &str) {
        for line in text.lines() {
            let _ = writeln!(self.text, "// {line}");
        }
    }

    /// Whether nothing is written yet.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The document, formatted.
    pub fn finish(self) -> Result<String, Error> {
        let config = FormatConfig::default().with_max_width(self.config.max_width);
        format(&self.text, &config).map_err(|errors| {
            let reasons: Vec<String> = errors.iter().map(|e| e.message().to_owned()).collect();
            Error::new(format!(
                "the document does not parse: {}",
                reasons.join("; ")
            ))
        })
    }
}

/// A root key as a target: a key that is no identifier is quoted, and one
/// that reads as an integer stays a string.
fn root_key(key: &str) -> String {
    Key::Str(key.into()).to_string()
}

/// Whether `~` merges `b` into `a`, rather than `b` replacing it.
fn mergeable(a: &Keyed, b: &Keyed) -> bool {
    match (&a.name, &b.name) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    }
}

/// Whether the marks that [`Value::equals`] ignores are the same, at every
/// depth of what the two have in common.
fn same_marks(a: &Value, b: &Value) -> bool {
    let (ValueKind::Keyed(a), ValueKind::Keyed(b)) = (&a.kind, &b.kind) else {
        return true;
    };
    a.defaults == b.defaults
        && a.entries
            .iter()
            .all(|(key, a)| b.entries.get(key).is_none_or(|b| same_marks(a, b)))
}

/// The entries of a patch block that make `from` into `to`.
fn diff_entries(from: &Keyed, to: &Keyed) -> String {
    let mut entries = Vec::new();
    for (key, new) in &to.entries {
        let Some(old) = from.entries.get(key) else {
            entries.push(format!("{key}: {}", print::to_text(new)));
            continue;
        };
        if old.equals(new) && same_marks(old, new) {
            continue;
        }
        match (&old.kind, &new.kind) {
            (ValueKind::Keyed(a), ValueKind::Keyed(b)) if mergeable(a, b) => {
                let name = b.name.as_ref().map_or(String::new(), |n| format!("{n} "));
                entries.push(format!("{key}: {name}{{ {} }}", diff_entries(a, b)));
            }
            _ => entries.push(format!("{key}: {}", print::to_text(new))),
        }
    }
    for key in from.entries.keys() {
        if !to.entries.contains_key(key) {
            entries.push(format!("-{key}"));
        }
    }
    if to.defaults && !from.defaults {
        entries.push("..".to_owned());
    }
    entries.join(", ")
}

/// Takes `..` out of a patch, at every depth.
fn clear_defaults(value: &mut Value) {
    match &mut value.kind {
        ValueKind::Keyed(keyed) => {
            keyed.defaults = false;
            keyed.entries.values_mut().for_each(clear_defaults);
        }
        ValueKind::NamedTuple(_, items) if items.len() == 1 => clear_defaults(&mut items[0]),
        _ => {}
    }
}
