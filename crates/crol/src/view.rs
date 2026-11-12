//! Views: reading parts of evaluated data into small types, across contexts
//! (`SPEC.md` 10.11).

use std::cmp::Ordering;

use crol_eval::{Defaults, Key, Sources, Value, ValueKind};
use serde::{Deserialize, Serialize};

use crate::compose::{CrolContext, CrolData, VariantName};
use crate::de;
use crate::error::Error;
use crate::path::{self, IntoPath, Step};
use crate::ser::{self, PrettyConfig};

/// A type that reads fields from anywhere in a value, by path, unlike a
/// `Deserialize` type, which has to mirror it. `#[derive(crol::View)]`
/// implements this: a field reads the value's field of its own name, as serde
/// names it, or the path in `#[crol(path = "combat.range.max")]`.
///
/// [`CrolData::variations`] and [`Query::views`] read them, and so do
/// [`Select::view`](crate::Select::view) and
/// [`Transaction::view`](crate::Transaction::view), at a typed path.
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
        value = keyed.field(step)?;
    }
    Some(unwrap_some(value))
}

fn unwrap_some(value: &Value) -> &Value {
    match &value.kind {
        ValueKind::NamedTuple(name, items) if &**name == "Some" && items.len() == 1 => &items[0],
        _ => value,
    }
}

/// Whether what `steps` read is `wanted`, or a list that holds it. An id
/// counts as its text.
fn holds(value: &Value, steps: &[Step], wanted: &Value) -> bool {
    let Some(found) = path::value_at(value, steps).map(unwrap_some) else {
        return false;
    };
    let equal = |value: &Value| match (&value.kind, &wanted.kind) {
        (ValueKind::Id(a), ValueKind::Id(b)) => a == b,
        (ValueKind::Id(id), ValueKind::String(s)) | (ValueKind::String(s), ValueKind::Id(id)) => {
            crate::id::id_text(id) == **s
        }
        (ValueKind::String(a), ValueKind::String(b)) => a == b,
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

    pub(crate) fn read_view<V: View>(&self, key: &str, value: &Value) -> Result<V, Error> {
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
    holds: Vec<(Vec<Step>, Value)>,
    error: Option<Error>,
}

impl<'a, T: VariantName> Query<'a, T> {
    /// A query that gives this error when it is read.
    pub(crate) fn failed(mut self, error: Error) -> Self {
        self.error = Some(error);
        self
    }

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
    pub fn holding<S: Serialize + ?Sized>(self, path: &str, value: &S) -> Self {
        let steps = path.split('.').map(|step| Step::Key(Key::Str(step.into())));
        self.hold(steps.collect(), value)
    }

    /// [`holding`](Self::holding) at a typed path, which starts at the type of
    /// the values: `Unit::path().combat().range()`. It can go through a list
    /// or an `Option` as well.
    pub fn holding_at<B, S: Serialize + ?Sized>(self, path: impl IntoPath<B>, value: &S) -> Self {
        self.hold(path.into_path().into_steps(), value)
    }

    fn hold<S: Serialize + ?Sized>(mut self, steps: Vec<Step>, value: &S) -> Self {
        match ser::to_value(value, &PrettyConfig::default()) {
            Ok(value) => self.holds.push((steps, value)),
            Err(error) => {
                self.error.get_or_insert(error);
            }
        }
        self
    }

    fn matches(&self, value: &Value) -> bool {
        let type_name = match &value.kind {
            ValueKind::Keyed(keyed) => keyed.name.as_deref(),
            ValueKind::NamedTuple(name, _) | ValueKind::UnitName(name) => Some(&**name),
            _ => None,
        };
        self.type_name
            .as_deref()
            .is_none_or(|wanted| type_name == Some(wanted))
            && self
                .holds
                .iter()
                .all(|(steps, wanted)| holds(value, steps, wanted))
    }

    /// What matches, in the order of the keys.
    fn matching(&self) -> Result<Vec<(String, &'a Value)>, Error> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        let mut found: Vec<(String, &'a Value)> = self
            .data
            .keys_in(&self.context)
            .into_iter()
            .filter_map(|key| {
                let value = self.data.value(&key, &self.context)?;
                self.matches(value).then_some((key, value))
            })
            .collect();
        found.sort_by(|(a, _), (b, _)| a.as_bytes().cmp(b.as_bytes()));
        Ok(found)
    }

    /// The keys that match.
    pub fn keys(&self) -> Result<Vec<String>, Error> {
        let found = self.matching()?;
        Ok(found.into_iter().map(|(key, _)| key).collect())
    }

    /// What matches, each read into a [`View`].
    pub fn views<V: View>(&self) -> Result<Vec<(String, V)>, Error> {
        self.matching()?
            .into_iter()
            .map(|(key, value)| {
                let view = self.data.read_view(&key, value)?;
                Ok((key, view))
            })
            .collect()
    }
}
