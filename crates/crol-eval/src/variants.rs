//! Variants and contexts (`SPEC.md` 8.1).

use std::collections::BTreeSet;
use std::fmt;

/// Identifies a declared variant.
pub type VariantId = usize;

/// The declared variants: `Discrete` flags and `Grouped` alternatives, and
/// their parents (`SPEC.md` 8.1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Variants {
    names: Vec<String>,
    /// The group of each variant, or `None` for a `Discrete` one.
    group_of: Vec<Option<usize>>,
    groups: Vec<Vec<VariantId>>,
    parent: Vec<Option<VariantId>>,
}

impl Variants {
    /// No variants.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether no variant is declared.
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    fn add(&mut self, name: &str, group: Option<usize>) -> Result<VariantId, String> {
        if !crate::value::is_identifier(name) {
            return Err(format!(
                "`{name}` is not an identifier, so it cannot be a variant"
            ));
        }
        if self.id(name).is_some() {
            return Err(format!("the variant `{name}` is declared twice"));
        }
        self.names.push(name.to_owned());
        self.group_of.push(group);
        self.parent.push(None);
        Ok(self.names.len() - 1)
    }

    /// Declares a flag, on or off independently of the rest.
    pub fn add_discrete(&mut self, name: &str) -> Result<(), String> {
        self.add(name, None).map(|_| ())
    }

    /// Declares alternatives: at most one of them is active.
    pub fn add_grouped<S: AsRef<str>>(
        &mut self,
        names: impl IntoIterator<Item = S>,
    ) -> Result<(), String> {
        let group = self.groups.len();
        let mut members = Vec::new();
        for name in names {
            members.push(self.add(name.as_ref(), Some(group))?);
        }
        self.groups.push(members);
        Ok(())
    }

    /// Gives a variant a parent: the child being active makes the parent
    /// active. Both must be `Discrete`, or in the same group.
    pub fn add_parent(&mut self, child: &str, parent: &str) -> Result<(), String> {
        let unknown = |name: &str| format!("the variant `{name}` is not declared");
        let c = self.id(child).ok_or_else(|| unknown(child))?;
        let p = self.id(parent).ok_or_else(|| unknown(parent))?;
        if self.group_of[c] != self.group_of[p] {
            return Err(format!(
                "`{parent}` cannot be the parent of `{child}`: a variant and its parent must both be discrete, or in the same group"
            ));
        }
        if self.parent[c].is_some() {
            return Err(format!("`{child}` already has a parent"));
        }
        let mut ancestor = Some(p);
        while let Some(a) = ancestor {
            if a == c {
                return Err(format!(
                    "making `{parent}` the parent of `{child}` forms a cycle"
                ));
            }
            ancestor = self.parent[a];
        }
        self.parent[c] = Some(p);
        Ok(())
    }

    /// The id of a declared variant.
    pub fn id(&self, name: &str) -> Option<VariantId> {
        self.names.iter().position(|n| n == name)
    }

    /// A variant's name.
    pub fn name(&self, id: VariantId) -> &str {
        &self.names[id]
    }

    /// Every variant, in the order declared.
    pub fn ids(&self) -> std::ops::Range<VariantId> {
        0..self.names.len()
    }

    /// A variant's parent, if any.
    pub fn parent(&self, id: VariantId) -> Option<VariantId> {
        self.parent[id]
    }

    /// A variant's group, if it is grouped.
    pub fn group(&self, id: VariantId) -> Option<usize> {
        self.group_of[id]
    }

    /// The number of ancestors of a variant.
    pub(crate) fn depth(&self, id: VariantId) -> usize {
        std::iter::successors(self.parent[id], |&p| self.parent[p]).count()
    }

    fn is_ancestor(&self, ancestor: VariantId, of: VariantId) -> bool {
        std::iter::successors(self.parent[of], |&p| self.parent[p]).any(|a| a == ancestor)
    }

    /// The context in which `chosen` and their ancestors are active. Fails
    /// for two choices in one group that are not ancestor and descendant.
    pub fn context(&self, chosen: &[VariantId]) -> Result<Context, String> {
        for (i, &a) in chosen.iter().enumerate() {
            for &b in &chosen[i + 1..] {
                if a != b
                    && self.group_of[a].is_some()
                    && self.group_of[a] == self.group_of[b]
                    && !self.is_ancestor(a, b)
                    && !self.is_ancestor(b, a)
                {
                    return Err(format!(
                        "`{}` and `{}` are alternatives in one group; at most one of them can be active",
                        self.names[a], self.names[b]
                    ));
                }
            }
        }
        let mut active = BTreeSet::new();
        for &id in chosen {
            active.insert(id);
            active.extend(std::iter::successors(self.parent[id], |&p| self.parent[p]));
        }
        Ok(Context(active.into_iter().collect()))
    }

    /// `context` with `changes` applied (`SPEC.md` 8.4): making a variant
    /// active replaces its group's choice; making one inactive also makes
    /// its descendants inactive.
    pub(crate) fn switch(&self, context: &Context, changes: &[(VariantId, bool)]) -> Context {
        let mut chosen: Vec<VariantId> = self.chosen(context);
        for &(id, active) in changes {
            if active {
                if let Some(group) = self.group_of[id] {
                    chosen.retain(|&c| self.group_of[c] != Some(group));
                }
                chosen.push(id);
            } else {
                chosen.retain(|&c| c != id && !self.is_ancestor(id, c));
                // An ancestor made inactive turns off its active descendants,
                // whose ancestors would otherwise turn it back on.
            }
        }
        self.context(&chosen).unwrap_or_else(|_| context.clone())
    }

    /// The chosen variants of a context: its active variants that are not
    /// ancestors of other active ones.
    pub fn chosen(&self, context: &Context) -> Vec<VariantId> {
        context
            .0
            .iter()
            .copied()
            .filter(|&a| !context.0.iter().any(|&b| self.is_ancestor(a, b)))
            .collect()
    }

    /// The number of valid contexts: each group's choice or none, times each
    /// discrete variant on or off. With parents, some of these are the same
    /// context; this is an upper bound.
    pub fn context_count(&self) -> u128 {
        let mut count: u128 = 1;
        for group in &self.groups {
            count = count.saturating_mul(group.len() as u128 + 1);
        }
        for id in self.ids() {
            if self.group_of[id].is_none() {
                count = count.saturating_mul(2);
            }
        }
        count
    }

    /// Every valid context, the base first, in a fixed order.
    pub fn all_contexts(&self) -> Vec<Context> {
        let discrete: Vec<VariantId> = self.ids().filter(|&i| self.group_of[i].is_none()).collect();
        let mut contexts = vec![Vec::new()];
        for group in &self.groups {
            let mut next = Vec::with_capacity(contexts.len() * (group.len() + 1));
            for chosen in &contexts {
                next.push(chosen.clone());
                for &member in group {
                    let mut with = chosen.clone();
                    with.push(member);
                    next.push(with);
                }
            }
            contexts = next;
        }
        for &id in &discrete {
            let mut next = Vec::with_capacity(contexts.len() * 2);
            for chosen in &contexts {
                next.push(chosen.clone());
                let mut with = chosen.clone();
                with.push(id);
                next.push(with);
            }
            contexts = next;
        }
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for chosen in contexts {
            if let Ok(context) = self.context(&chosen)
                && seen.insert(context.clone())
            {
                out.push(context);
            }
        }
        out
    }

    /// Displays a context by its chosen variants: `{prod_eu, cache}`.
    pub fn display<'a>(&'a self, context: &'a Context) -> impl fmt::Display + 'a {
        DisplayContext {
            variants: self,
            context,
        }
    }

    /// The sorted names of a context's chosen variants, as the fingerprint
    /// orders contexts (`SPEC.md` appendix B).
    pub fn chosen_names(&self, context: &Context) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .chosen(context)
            .into_iter()
            .map(|id| self.name(id))
            .collect();
        names.sort_unstable();
        names
    }
}

struct DisplayContext<'a> {
    variants: &'a Variants,
    context: &'a Context,
}

impl fmt::Display for DisplayContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self
            .variants
            .chosen(self.context)
            .into_iter()
            .map(|id| self.variants.name(id))
            .collect();
        write!(f, "{{{}}}", names.join(", "))
    }
}

/// A context: the set of active variants, ancestors included (`SPEC.md`
/// 8.1). The base context is empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Context(Vec<VariantId>);

impl Context {
    /// The base context, in which no variant is active.
    pub fn base() -> Self {
        Self::default()
    }

    /// Whether this is the base context.
    pub fn is_base(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether a variant is active.
    pub fn is_active(&self, id: VariantId) -> bool {
        self.0.binary_search(&id).is_ok()
    }

    /// The active variants, ancestors included, in id order.
    pub fn active(&self) -> &[VariantId] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_variants() -> Variants {
        let mut v = Variants::new();
        v.add_grouped(["dev", "prod", "prod_eu"]).unwrap();
        v.add_discrete("cache").unwrap();
        v.add_discrete("debug").unwrap();
        v.add_parent("prod_eu", "prod").unwrap();
        v
    }

    #[test]
    fn contexts_of_the_specification_example() {
        let v = spec_variants();
        // 4 × 2 × 2 (SPEC.md 8.1).
        assert_eq!(v.context_count(), 16);
        assert_eq!(v.all_contexts().len(), 16);
        let id = |n| v.id(n).unwrap();
        let context = v.context(&[id("prod_eu"), id("cache")]).unwrap();
        assert!(context.is_active(id("prod")));
        assert_eq!(v.display(&context).to_string(), "{prod_eu, cache}");
        assert!(v.context(&[id("dev"), id("prod")]).is_err());
        assert!(v.context(&[id("prod"), id("prod_eu")]).is_ok());
    }

    #[test]
    fn switching_contexts() {
        let v = spec_variants();
        let id = |n| v.id(n).unwrap();
        let context = v.context(&[id("prod_eu"), id("cache")]).unwrap();
        let dev = v.switch(&context, &[(id("dev"), true)]);
        assert_eq!(v.display(&dev).to_string(), "{dev, cache}");
        let no_cache = v.switch(&context, &[(id("cache"), false)]);
        assert_eq!(v.display(&no_cache).to_string(), "{prod_eu}");
        let no_prod = v.switch(&context, &[(id("prod"), false)]);
        assert_eq!(v.display(&no_prod).to_string(), "{cache}");
    }

    #[test]
    fn parents_must_match_kinds() {
        let mut v = spec_variants();
        assert!(v.add_parent("cache", "prod").is_err());
        assert!(v.add_parent("debug", "cache").is_ok());
        assert!(
            v.add_parent("cache", "debug")
                .unwrap_err()
                .contains("cycle")
        );
        assert!(v.add_discrete("cache").unwrap_err().contains("twice"));
    }
}
