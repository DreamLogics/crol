//! `Expression`: CROL text written where a value goes, for what a value
//! cannot say, such as a reference (`SPEC.md` 10.8).

use crol_eval::{Span, Value, ValueKind};
use crol_syntax::is_expression;
use serde::ser::{Serialize, Serializer};

use crate::error::Error;

/// The newtype name through which `Expression` and the CROL serializer
/// recognize each other. Other formats see the text, as a string.
pub(crate) const EXPRESSION_TOKEN: &str = "$crol::Expression";

/// One expression, written as it is in place of a value:
///
/// ```
/// # fn main() -> Result<(), crol::Error> {
/// let mut writer = crol::DataWriter::new();
/// writer.set("weapon", &crol::Expression::new("&root.rifle")?)?;
/// assert_eq!(writer.finish()?, "weapon: &root.rifle\n");
/// # Ok(())
/// # }
/// ```
///
/// In a value from [`crate::to_value`], put one with [`Expression::to_value`]
/// and write the value as a [`crate::RawValue`]. Such a value is for writing:
/// nothing has evaluated the expression.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Expression(String);

impl Expression {
    /// An expression from its text, which must be one expression, without
    /// comments.
    pub fn new(text: impl Into<String>) -> Result<Self, Error> {
        let text = text.into();
        if is_expression(&text) {
            Ok(Self(text))
        } else {
            Err(Error::new(format!("`{text}` is not one expression")))
        }
    }

    /// An expression `croxp!` checked when the program was compiled.
    #[doc(hidden)]
    pub fn __checked(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// The expression's text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The expression as a [`Value`] that prints as its text.
    pub fn to_value(&self) -> Value {
        text_value(&self.0)
    }
}

/// What `set_expr` takes: an [`Expression`], or a `Result` of one, which is
/// what `croxp!` gives when it holds Rust values.
pub trait IntoExpression {
    /// The expression, or why there is none.
    fn into_expression(self) -> Result<Expression, Error>;
}

impl IntoExpression for Expression {
    fn into_expression(self) -> Result<Expression, Error> {
        Ok(self)
    }
}

impl IntoExpression for Result<Expression, Error> {
    fn into_expression(self) -> Result<Expression, Error> {
        self
    }
}

/// The text of a Rust value in an expression, for `croxp!`.
#[doc(hidden)]
pub fn __value_text<T: Serialize + ?Sized>(value: &T) -> Result<String, Error> {
    let value = crate::ser::to_value(value, &crate::PrettyConfig::default())?;
    Ok(crol_eval::print::to_text(&value))
}

/// A value that prints as `text`, as printing a name writes it as it is.
pub(crate) fn text_value(text: &str) -> Value {
    Value::new(ValueKind::UnitName(text.into()), Span::detached())
}

impl Serialize for Expression {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_newtype_struct(EXPRESSION_TOKEN, &self.0)
    }
}
