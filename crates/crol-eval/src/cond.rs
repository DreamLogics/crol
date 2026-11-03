//! Variant conditions (`SPEC.md` 8.2, 8.3).

use std::rc::Rc;

use crate::source::Span;
use crate::variants::{Context, VariantId, Variants};

/// A condition as written, with variant names.
#[derive(Debug, Clone)]
pub(crate) enum Cond {
    Name(Rc<str>, Span),
    Not(Box<Cond>),
    And(Box<Cond>, Box<Cond>),
    Or(Box<Cond>, Box<Cond>),
    Xor(Box<Cond>, Box<Cond>),
}

impl Cond {
    /// `a & b`, where `None` is the base condition that always holds.
    pub(crate) fn and(a: Option<&Cond>, b: Option<&Cond>) -> Option<Cond> {
        match (a, b) {
            (None, None) => None,
            (Some(c), None) | (None, Some(c)) => Some(c.clone()),
            (Some(a), Some(b)) => Some(Cond::And(Box::new(a.clone()), Box::new(b.clone()))),
        }
    }

    /// Every variant name in the condition, with its span.
    pub(crate) fn names(&self, out: &mut Vec<(Rc<str>, Span)>) {
        match self {
            Cond::Name(name, span) => out.push((name.clone(), *span)),
            Cond::Not(c) => c.names(out),
            Cond::And(a, b) | Cond::Or(a, b) | Cond::Xor(a, b) => {
                a.names(out);
                b.names(out);
            }
        }
    }

    /// Resolves names to variants; every name must be declared.
    pub(crate) fn resolve(&self, variants: &Variants) -> Option<Resolved> {
        Some(match self {
            Cond::Name(name, _) => Resolved::Name(variants.id(name)?),
            Cond::Not(c) => Resolved::Not(Box::new(c.resolve(variants)?)),
            Cond::And(a, b) => Resolved::And(
                Box::new(a.resolve(variants)?),
                Box::new(b.resolve(variants)?),
            ),
            Cond::Or(a, b) => Resolved::Or(
                Box::new(a.resolve(variants)?),
                Box::new(b.resolve(variants)?),
            ),
            Cond::Xor(a, b) => Resolved::Xor(
                Box::new(a.resolve(variants)?),
                Box::new(b.resolve(variants)?),
            ),
        })
    }
}

impl std::fmt::Display for Cond {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let operand = |c: &Cond, f: &mut std::fmt::Formatter<'_>| match c {
            Cond::Name(..) | Cond::Not(_) => write!(f, "{c}"),
            _ => write!(f, "({c})"),
        };
        match self {
            Cond::Name(name, _) => f.write_str(name),
            Cond::Not(c) => {
                f.write_str("!")?;
                operand(c, f)
            }
            Cond::And(a, b) | Cond::Or(a, b) | Cond::Xor(a, b) => {
                let op = match self {
                    Cond::And(..) => "&",
                    Cond::Or(..) => "|",
                    _ => "^",
                };
                operand(a, f)?;
                write!(f, " {op} ")?;
                operand(b, f)
            }
        }
    }
}

/// A condition with names resolved to variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Resolved {
    Name(VariantId),
    Not(Box<Resolved>),
    And(Box<Resolved>, Box<Resolved>),
    Or(Box<Resolved>, Box<Resolved>),
    Xor(Box<Resolved>, Box<Resolved>),
}

impl Resolved {
    /// Whether the condition holds in a context.
    pub(crate) fn holds(&self, context: &Context) -> bool {
        match self {
            Resolved::Name(id) => context.is_active(*id),
            Resolved::Not(c) => !c.holds(context),
            Resolved::And(a, b) => a.holds(context) && b.holds(context),
            Resolved::Or(a, b) => a.holds(context) || b.holds(context),
            Resolved::Xor(a, b) => a.holds(context) != b.holds(context),
        }
    }

    /// Specificity, like CSS selectors (`SPEC.md` 8.3).
    pub(crate) fn specificity(&self, variants: &Variants) -> usize {
        match self {
            Resolved::Name(id) => 1 + variants.depth(*id),
            Resolved::Not(_) => 0,
            Resolved::And(a, b) => a.specificity(variants) + b.specificity(variants),
            Resolved::Or(a, b) | Resolved::Xor(a, b) => {
                a.specificity(variants).min(b.specificity(variants))
            }
        }
    }

    /// A canonical text for the condition, the same for conditions that
    /// differ only in the order of operands and in parentheses.
    pub(crate) fn normalized(&self) -> String {
        fn flatten<'a>(c: &'a Resolved, op: u8, out: &mut Vec<&'a Resolved>) {
            match (c, op) {
                (Resolved::And(a, b), 0) | (Resolved::Or(a, b), 1) | (Resolved::Xor(a, b), 2) => {
                    flatten(a, op, out);
                    flatten(b, op, out);
                }
                _ => out.push(c),
            }
        }
        let (op, symbol) = match self {
            Resolved::Name(id) => return id.to_string(),
            Resolved::Not(c) => return format!("!({})", c.normalized()),
            Resolved::And(..) => (0, "&"),
            Resolved::Or(..) => (1, "|"),
            Resolved::Xor(..) => (2, "^"),
        };
        let mut operands = Vec::new();
        flatten(self, op, &mut operands);
        let mut parts: Vec<String> = operands.iter().map(|c| c.normalized()).collect();
        parts.sort();
        format!("({})", parts.join(symbol))
    }
}
