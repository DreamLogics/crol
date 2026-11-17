//! Constants the program gives (`SPEC.md` 6.1.4): `$name` in the data.

use crol_eval::{Constants, Diagnostic, Span, Value};
use serde::Serialize;

use crate::check::{CheckOptions, check_constant};
use crate::compose::{CrolData, Reloaded, VariantName};
use crate::error::Error;
use crate::schema::SchemaFile;

/// Converts what the program gives for the constant `name` to a value, and
/// checks it: `name` must be an identifier, the value must serialize (with
/// finite floats), and fit the type `schema` declares for it, if any.
pub(crate) fn constant_value(
    name: &str,
    value: &(impl Serialize + ?Sized),
    schema: Option<&SchemaFile>,
) -> Result<Value, Error> {
    if !crol_eval::is_identifier(name) {
        return Err(Error::new(format!(
            "constant: `{name}` is not an identifier, so data cannot write `$` before it"
        )));
    }
    let value = crate::to_value(value)
        .map_err(|error| Error::new(format!("constant `{name}`: {error}")))?;
    if let Some(schema) = schema {
        check_type(name, &value, schema)?;
    }
    Ok(value)
}

/// Checks a constant's value against the type `schema` declares for it.
pub(crate) fn check_type(name: &str, value: &Value, schema: &SchemaFile) -> Result<(), Error> {
    let Some(declared) = schema.constants.get(name) else {
        return Ok(());
    };
    let found = check_constant(value, schema, &declared.ty, &CheckOptions::default());
    match found.iter().find(|d| d.is_error()) {
        Some(diagnostic) => Err(Error::new(format!(
            "constant `{name}` does not fit the schema: {}",
            diagnostic.message
        ))),
        None => Ok(()),
    }
}

/// Checks constants against what a schema declares (`SPEC.md` 6.1.4), as
/// the composer does, for tools that evaluate with `crol-eval` directly: a
/// declared constant that is not given is an error, once, and is
/// [failed](Constants::fail) so that reading it is quiet; a value that does
/// not fit its type is an error.
pub fn check_constants(constants: &mut Constants, schema: Option<&SchemaFile>) -> Vec<Diagnostic> {
    check_declared(constants, schema)
}

/// What is wrong with `constants` against what `schema` declares: each
/// declared constant that was not given is an error, reported once, and
/// reading it fails quietly from then on, so that `??` does not catch it.
/// Values given before the schema are checked against it here.
pub(crate) fn check_declared(
    constants: &mut Constants,
    schema: Option<&SchemaFile>,
) -> Vec<Diagnostic> {
    let Some(schema) = schema else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for name in schema.constants.keys() {
        match constants.get(name) {
            Some(value) => {
                if let Err(error) = check_type(name, value, schema) {
                    found.push(Diagnostic::error(Span::detached(), error.to_string()));
                }
            }
            None => {
                found.push(Diagnostic::error(
                    Span::detached(),
                    format!("the schema declares constant `{name}`, which was not given"),
                ));
                constants.fail(name.as_str());
            }
        }
    }
    found
}

/// Changes to the constants of composed data, from
/// [`CrolData::update_constants`]: [`apply`](Self::apply) evaluates again
/// only what reads a constant that changed (`SPEC.md` 6.1.4, 7.9).
///
/// ```
/// use crol::{CrolComposer, CrolLayer};
///
/// let layer = CrolLayer::from_sources("base", [("a.crod", "count: $count + 1, other: 2")]);
/// let mut data = CrolComposer::new()
///     .load_layer(layer)
///     .with_constant("count", 1)?
///     .compose_data()?;
/// let reloaded = data.update_constants().set("count", 41)?.apply()?;
/// assert_eq!(data.get::<i64>("count")?, 42);
/// assert_eq!(reloaded.evaluated, Some(1));
/// # Ok::<(), crol::Error>(())
/// ```
#[must_use = "the constants change only when `apply` is called"]
pub struct ConstantsUpdate<'d, T: VariantName = String> {
    data: &'d mut CrolData<T>,
    constants: Constants,
}

impl<'d, T: VariantName> ConstantsUpdate<'d, T> {
    pub(crate) fn new(data: &'d mut CrolData<T>) -> Self {
        let constants = data.constants().clone();
        Self { data, constants }
    }

    /// Gives a constant a value, or another one. It fails as
    /// [`CrolComposer::with_constant`](crate::CrolComposer::with_constant)
    /// does, but for giving a name twice.
    pub fn set(mut self, name: &str, value: impl Serialize) -> Result<Self, Error> {
        let value = constant_value(name, &value, self.data.schema_file())?;
        self.constants.insert(name, value);
        Ok(self)
    }

    /// Takes a constant away: `$name` is missing again (`SPEC.md` 6.5). A
    /// constant the schema declares must be given, so removing one is an
    /// error.
    pub fn remove(mut self, name: &str) -> Result<Self, Error> {
        if self
            .data
            .schema_file()
            .is_some_and(|schema| schema.constants.contains_key(name))
        {
            return Err(Error::new(format!(
                "constant `{name}` is declared in the schema, so it cannot be removed"
            )));
        }
        self.constants.remove(name);
        Ok(self)
    }

    /// Evaluates again what reads a constant that changed, in the contexts
    /// evaluated so far. The result, order and fingerprint included, is what
    /// composing again with these constants gives. If it does not load, the
    /// error is returned and the data stays as it was.
    pub fn apply(self) -> Result<Reloaded, Error> {
        self.data.reload_constants(self.constants)
    }
}
