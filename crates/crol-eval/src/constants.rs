//! Constants the program gives (`SPEC.md` 6.1.4): `$name` in the data.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::value::{Value, ValueKind};

/// The constants an evaluation reads through `$name` (`SPEC.md` 6.1.4).
///
/// A constant is the same value in every layer and every context. A name
/// that is not given is *missing* (`SPEC.md` 6.5), unless it is
/// [failed](Constants::fail), or tools ask for a
/// [warning](Constants::with_absent_warning).
#[derive(Debug, Clone, Default)]
pub struct Constants {
    values: BTreeMap<Arc<str>, Value>,
    /// Names whose value is an error reported elsewhere, as a constant the
    /// schema declares and the program did not give: reading one fails
    /// quietly, and `??` does not catch it.
    failed: BTreeSet<Arc<str>>,
    /// Whether a name that is not given is a warning on the use and fails
    /// quietly, rather than missing: what tools do with stand-in values.
    absent_warning: bool,
}

impl PartialEq for Constants {
    fn eq(&self, other: &Self) -> bool {
        self.failed == other.failed
            && self.absent_warning == other.absent_warning
            && self.values.len() == other.values.len()
            && self
                .values
                .iter()
                .zip(&other.values)
                .all(|((a, v), (b, w))| a == b && v.identical(w))
    }
}

impl Eq for Constants {}

impl Constants {
    /// No constants.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether there are no constants, and no names are failed.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty() && self.failed.is_empty()
    }

    /// Gives a constant its value, and returns the one it had. Values from
    /// a program carry no seals, defaults flag or scope flag, so those of
    /// `value` are cleared.
    pub fn insert(&mut self, name: impl Into<Arc<str>>, mut value: Value) -> Option<Value> {
        let name = name.into();
        clear_flags(&mut value);
        self.failed.remove(&name);
        self.values.insert(name, value)
    }

    /// Takes a constant away: it is missing again.
    pub fn remove(&mut self, name: &str) -> Option<Value> {
        self.failed.remove(name);
        self.values.remove(name)
    }

    /// The value of a constant.
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.values.get(name)
    }

    /// The constants given, by name.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.values.iter().map(|(name, value)| (&**name, value))
    }

    /// The names of the constants given.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.values.keys().map(|name| &**name)
    }

    /// Makes reading `name` fail quietly: its error is reported elsewhere,
    /// as for a constant the schema declares that was not given. `??` does
    /// not catch it.
    pub fn fail(&mut self, name: impl Into<Arc<str>>) {
        let name = name.into();
        self.values.remove(&name);
        self.failed.insert(name);
    }

    /// Whether reading `name` fails quietly.
    pub fn is_failed(&self, name: &str) -> bool {
        self.failed.contains(name)
    }

    /// Makes a name that is not given a warning on each use, which fails
    /// quietly, rather than missing: for tools, where a constant without a
    /// stand-in value should not flood a file with errors (`SPEC.md` 12.5).
    pub fn with_absent_warning(mut self, warn: bool) -> Self {
        self.absent_warning = warn;
        self
    }

    /// Whether a name that is not given is a warning.
    pub fn absent_warning(&self) -> bool {
        self.absent_warning
    }

    /// The names given that differ between `self` and `other`: given in
    /// one and not the other, given different values, or failed in one.
    pub fn changed<'a>(&'a self, other: &'a Constants) -> BTreeSet<Arc<str>> {
        let mut changed = BTreeSet::new();
        for (name, value) in &self.values {
            if other.values.get(name).is_none_or(|w| !value.identical(w)) {
                changed.insert(name.clone());
            }
        }
        for name in other.values.keys() {
            if !self.values.contains_key(name) {
                changed.insert(name.clone());
            }
        }
        changed.extend(self.failed.symmetric_difference(&other.failed).cloned());
        changed
    }

    /// The message for reading `name` where it is not given, with the
    /// names given that look like it.
    pub(crate) fn absent_message(&self, name: &str) -> String {
        let names: Vec<&str> = self.names().collect();
        match closest(name, &names).as_slice() {
            [] => format!("no constant `{name}` was given"),
            [one] => format!("no constant `{name}` was given; did you mean `${one}`?"),
            [first @ .., last] => {
                let first: Vec<String> = first.iter().map(|n| format!("`${n}`")).collect();
                format!(
                    "no constant `{name}` was given; did you mean {} or `${last}`?",
                    first.join(", ")
                )
            }
        }
    }
}

fn clear_flags(value: &mut Value) {
    match &mut value.kind {
        ValueKind::List(items) | ValueKind::Tuple(items) | ValueKind::NamedTuple(_, items) => {
            items.iter_mut().for_each(clear_flags);
        }
        ValueKind::Keyed(keyed) => {
            keyed.defaults = false;
            keyed.scope = false;
            keyed.defaulted.clear();
            keyed.entries.values_mut().for_each(clear_flags);
        }
        _ => {}
    }
}

/// The names in `candidates` closest to `name` by edit distance, if close
/// enough to be a misspelling, best first, at most three.
pub fn closest<'a>(name: &str, candidates: &[&'a str]) -> Vec<&'a str> {
    let limit = (name.chars().count() / 3).max(1);
    let mut found: Vec<(usize, &'a str)> = candidates
        .iter()
        .filter(|c| **c != name)
        .map(|c| (distance(name, c), *c))
        .filter(|(d, _)| *d <= limit)
        .collect();
    found.sort();
    found.into_iter().take(3).map(|(_, c)| c).collect()
}

/// The edit distance between two names, with swapping two neighbouring
/// characters as one edit (optimal string alignment).
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[a.len()][b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_close_names() {
        let mut constants = Constants::new();
        let unit = Value::new(ValueKind::Unit, crate::Span::detached());
        constants.insert("foo_count", unit.clone());
        constants.insert("bar", unit);
        assert_eq!(
            constants.absent_message("foo_cuont"),
            "no constant `foo_cuont` was given; did you mean `$foo_count`?"
        );
        assert_eq!(
            constants.absent_message("zzz"),
            "no constant `zzz` was given"
        );
    }
}
