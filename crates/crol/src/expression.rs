//! `Expression`: CROL text written where a value goes, for what a value
//! cannot say, such as a reference (`SPEC.md` 10.8).

use crol_eval::{Span, Value, ValueKind};
use crol_syntax::SyntaxKind;
use crol_syntax::ast::{self, AstNode};
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

    /// The expression's text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The expression as a [`Value`] that prints as its text.
    pub fn to_value(&self) -> Value {
        text_value(&self.0)
    }
}

/// A value that prints as `text`, as printing a name writes it as it is.
pub(crate) fn text_value(text: &str) -> Value {
    Value::new(ValueKind::UnitName(text.into()), Span::detached())
}

/// Whether `text` is one expression, and nothing else.
pub(crate) fn is_expression(text: &str) -> bool {
    let parse = crol_syntax::parse(text);
    if !parse.errors().is_empty() {
        return false;
    }
    let tree = parse.syntax();
    let comment = tree.descendants_with_tokens().any(|e| {
        matches!(
            e.kind(),
            SyntaxKind::LINE_COMMENT | SyntaxKind::BLOCK_COMMENT
        )
    });
    let Some(document) = ast::Document::cast(tree) else {
        return false;
    };
    !comment
        && document.expr().is_some()
        && document.entries().next().is_none()
        && document.attributes().next().is_none()
}

impl Serialize for Expression {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_newtype_struct(EXPRESSION_TOKEN, &self.0)
    }
}
