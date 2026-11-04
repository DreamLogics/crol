//! Views: reading parts of evaluated data into small types, across contexts
//! (`SPEC.md` 10.11).

use std::cmp::Ordering;

use crol_eval::{Defaults, Sources, Value, ValueKind};
use serde::{Deserialize, Serialize};

use crate::compose::{CrolContext, CrolData, VariantName};
use crate::de;
use crate::error::Error;
use crate::ser::{self, PrettyConfig};

/// A type that reads fields from anywhere in a value, by path, unlike a
/// `Deserialize` type, which has to mirror it. `#[derive(crol::View)]`
/// implements this: a field reads the value's field of its own name, or
/// the path in `#[crol(path = "combat.range.max")]`.
pub trait View: Sized {
    /// Reads the view from a value.
    fn read(source: &ViewSource<'_>) -> Result<Self, Error>;
}

/// The value a [`View`] reads from.
pub struct ViewSource<'a> {
    key: &'a str,
    value: &'a Value,
    sources: &'a Sources,
    options: &'a de::Options,
    defaults: Option<&'a Defaults>,
}

impl ViewSource<'_> {
    /// The root key the value is at.
    pub fn key(&self) -> &str {
        self.key
    }

    /// The value at `path`, or if it was left out, its default in the
    /// schema, if there is one.
    pub fn optional<T: for<'de> Deserialize<'de>>(
        &self,
        path: &[&str],
    ) -> Result<Option<T>, Error> {
        if let Some(value) = at_path(self.value, path) {
            return self.read(value).map(Some);
        }
        let name = match &self.value.kind {
            ValueKind::Keyed(keyed) => keyed.name.as_deref(),
            _ => None,
        };
        let steps: Vec<String> = path.iter().map(|step| (*step).to_owned()).collect();
        let default = self
            .defaults
            .filter(|_| name.is_some())
            .and_then(|defaults| defaults.0.default_of(name, &steps));
        default.map(|value| self.read(&value)).transpose()
    }

    /// The value at `path`, or its default; an error if it has neither.
    pub fn field<T: for<'de> Deserialize<'de>>(&self, path: &[&str]) -> Result<T, Error> {
        self.optional(path)?.ok_or_else(|| {
            Error::new(format!(
                "`{}` has no `{}`, and no default for it",
                self.key,
                path.join(".")
            ))
        })
    }

    fn read<T: for<'de> Deserialize<'de>>(&self, value: &Value) -> Result<T, Error> {
        crate::deserialize(value, self.sources, self.options.clone(), Some(self.key))
            .map(|(value, _)| value)
    }
}

/// The value at a path of field names, through `Some(...)`.
fn at_path<'v>(value: &'v Value, path: &[&str]) -> Option<&'v Value> {
    let mut value = value;
    for step in path {
        let keyed = match &unwrap_some(value).kind {
            ValueKind::Keyed(keyed) => keyed,
            _ => return None,
        };
        value = keyed.entries.get(&crol_eval::Key::Str((*step).into()))?;
    }
    Some(unwrap_some(value))
}

fn unwrap_some(value: &Value) -> &Value {
    match &value.kind {
        ValueKind::NamedTuple(name, items) if name == "Some" && items.len() == 1 => &items[0],
        _ => value,
    }
}

/// Whether what `path` reads is `wanted`, or a list that holds it. An id
/// counts as its text.
fn holds(value: &Value, path: &[&str], wanted: &Value) -> bool {
    let Some(found) = at_path(value, path) else {
        return false;
    };
    let equal = |value: &Value| match (&value.kind, &wanted.kind) {
        (ValueKind::Id(a) | ValueKind::String(a), ValueKind::Id(b) | ValueKind::String(b)) => {
            a == b
        }
        _ => value.equals(wanted),
    };
    match &found.kind {
        ValueKind::List(items) => items.iter().any(equal) || equal(found),
        _ => equal(found),
    }
}

/// One value of a root key, and the contexts it has that value in.
#[derive(Debug, Clone, PartialEq)]
pub struct Variation<V> {
    /// The contexts, the base first if it is among them.
    pub contexts: Vec<CrolContext>,
    /// The value.
    pub value: V,
}

impl<T: VariantName> CrolData<T> {
    /// The distinct values of a root key across the evaluated contexts, each
    /// with the contexts it has that value in. A key that no context changes
    /// has one, for every context. The order is the same whatever order the
    /// contexts were evaluated in: the base's value first, then by the names
    /// of the first context of each.
    pub fn distinct(&self, key: &str) -> Vec<Variation<&Value>> {
        let mut found: Vec<Variation<&Value>> = Vec::new();
        for context in self.sorted_contexts() {
            let Some(value) = self.value(key, &context) else {
                continue;
            };
            match found.iter_mut().find(|v| v.value.equals(value)) {
                Some(variation) => variation.contexts.push(context),
                None => found.push(Variation {
                    contexts: vec![context],
                    value,
                }),
            }
        }
        found
    }

    /// [`distinct`](Self::distinct), each value read into a [`View`].
    pub fn variations<V: View>(&self, key: &str) -> Result<Vec<Variation<V>>, Error> {
        self.distinct(key)
            .into_iter()
            .map(|variation| {
                Ok(Variation {
                    value: self.read_view(key, variation.value)?,
                    contexts: variation.contexts,
                })
            })
            .collect()
    }

    /// The root keys that match conditions, for reading them in one
    /// context: the base, unless [`Query::in_context`] says otherwise.
    pub fn query(&self) -> Query<'_, T> {
        Query {
            data: self,
            context: CrolContext::base(),
            type_name: None,
            holds: Vec::new(),
            error: None,
        }
    }

    /// The evaluated contexts, the base first, then by their names.
    fn sorted_contexts(&self) -> Vec<CrolContext> {
        let mut contexts: Vec<CrolContext> = self.contexts().collect();
        contexts.sort_by(|a, b| match (a.is_base(), b.is_base()) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => self.context_names(a).cmp(&self.context_names(b)),
        });
        contexts
    }

    fn read_view<V: View>(&self, key: &str, value: &Value) -> Result<V, Error> {
        V::read(&ViewSource {
            key,
            value,
            sources: self.sources(),
            options: self.deserialize_options(),
            defaults: self.defaults(),
        })
    }
}

/// Root keys chosen by what their values hold, from [`CrolData::query`].
/// The results are in the order of their keys, compared byte by byte, so
/// they are the same on every machine.
pub struct Query<'a, T> {
    data: &'a CrolData<T>,
    context: CrolContext,
    type_name: Option<String>,
    holds: Vec<(Vec<String>, Value)>,
    error: Option<Error>,
}

impl<T: VariantName> Query<'_, T> {
    /// Reads the values in this context.
    pub fn in_context(mut self, context: &CrolContext) -> Self {
        self.context = context.clone();
        self
    }

    /// Only values of this type name.
    pub fn of_type(mut self, name: &str) -> Self {
        self.type_name = Some(name.to_owned());
        self
    }

    /// Only values that hold `value` at `path`, a field path such as
    /// `"behaviour.tags"`: that value, or a list with that value in it.
    pub fn holding<S: Serialize + ?Sized>(mut self, path: &str, value: &S) -> Self {
        match ser::to_value(value, &PrettyConfig::default()) {
            Ok(value) => self
                .holds
                .push((path.split('.').map(str::to_owned).collect(), value)),
            Err(error) => self.error = Some(error),
        }
        self
    }

    fn matches(&self, value: &Value) -> bool {
        let type_name = match &value.kind {
            ValueKind::Keyed(keyed) => keyed.name.as_deref(),
            ValueKind::NamedTuple(name, _) | ValueKind::UnitName(name) => Some(name.as_str()),
            _ => None,
        };
        self.type_name
            .as_deref()
            .is_none_or(|wanted| type_name == Some(wanted))
            && self.holds.iter().all(|(path, wanted)| {
                let path: Vec<&str> = path.iter().map(String::as_str).collect();
                holds(value, &path, wanted)
            })
    }

    /// The keys that match.
    pub fn keys(&self) -> Result<Vec<String>, Error> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        let mut keys = self.data.keys_in(&self.context);
        keys.retain(|key| {
            self.data
                .value(key, &self.context)
                .is_some_and(|value| self.matches(value))
        });
        keys.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        Ok(keys)
    }

    /// What matches, each read into a [`View`].
    pub fn views<V: View>(&self) -> Result<Vec<(String, V)>, Error> {
        self.keys()?
            .into_iter()
            .map(|key| {
                let value = self.data.value(&key, &self.context).ok_or_else(|| {
                    Error::new(format!("there is no root key `{key}` in this context"))
                })?;
                let view = self.data.read_view(&key, value)?;
                Ok((key, view))
            })
            .collect()
    }
}
