//! Lowering the syntax tree to the IR, with the checks `SPEC.md` 13 calls
//! reading errors: attributes, binds and their scopes, and the forms of
//! variant selectors.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crol_syntax::SyntaxKind;
use crol_syntax::ast::{self, AstNode};

use crate::binary::Part;
use crate::cond::Cond;
use crate::diagnostic::Diagnostic;
use crate::ir::*;
use crate::literal;
use crate::source::{FileId, Span};
use crate::value::Key;

/// A data-mode document to lower, in its layer.
pub(crate) struct DataDocument<'a> {
    pub(crate) file: FileId,
    pub(crate) document: &'a ast::Document,
    pub(crate) layer: usize,
    /// The document's position in its layer.
    pub(crate) index: usize,
}

/// Lowered data-mode documents: every root operation, and the templates.
#[derive(Debug)]
pub(crate) struct LoweredData {
    pub(crate) ir: Ir,
    pub(crate) ops: Vec<RootOp>,
    pub(crate) templates: Vec<BindId>,
    /// Every `..` at a root.
    pub(crate) root_defaults: Vec<RootDefaults>,
    /// Every `#![variant(group: name)]`.
    pub(crate) declared: Vec<DeclaredVariant>,
    /// Where what is wrong with the root as a whole is reported: the start
    /// of the top document (`SPEC.md` 4.1).
    pub(crate) root_span: Span,
}

/// The root entries of a data-mode document: its entries, or those of its
/// untyped outer block (`SPEC.md` 4).
fn root_entries(lowerer: &mut Lowerer, document: &ast::Document) -> Vec<ast::AnyEntry> {
    match document.expr() {
        None => document.entries().collect(),
        Some(ast::Expr::Block(block)) => {
            if let Some(name) = block.type_name() {
                let span = lowerer.token_span(&name);
                lowerer.error(
                    span,
                    "the outer braces of a data-mode document cannot have a type name",
                );
            }
            block.entries().collect()
        }
        Some(expr) => {
            let span = lowerer.span(expr.syntax());
            lowerer.error(
                span,
                "a data-mode document holds entries (`key: value,`), not one value",
            );
            Vec::new()
        }
    }
}

/// Lowers the documents of every layer (`SPEC.md` 4, 7.2).
pub(crate) fn lower_data(documents: &[DataDocument<'_>]) -> (LoweredData, Vec<Diagnostic>) {
    lower_data_over(documents, Ir::default(), Vec::new())
}

/// [`lower_data`], with layers lowered ahead of time already in `ir`: their
/// templates are visible to the documents (`SPEC.md` 7.8).
pub(crate) fn lower_data_over(
    documents: &[DataDocument<'_>],
    ir: Ir,
    parts: Vec<Part>,
) -> (LoweredData, Vec<Diagnostic>) {
    let mut lowerer = Lowerer {
        file: documents.first().map_or(FileId::default(), |d| d.file),
        ir,
        names: HashSet::new(),
        diagnostics: Vec::new(),
        scopes: Vec::new(),
        aliases: HashMap::new(),
        reach: Reach::default(),
        used_below: false,
        data_mode: true,
        fixed: None,
        lifted: Vec::new(),
        lift_floor: None,
        position: (0, 0),
        default_target: DefaultTarget::default(),
        block_target: DefaultTarget::default(),
        root_defaults: Vec::new(),
        declared: Vec::new(),
    };

    // Templates are visible in every document of every layer, so they are
    // all collected first (`SPEC.md` 6.1.2).
    let mut scope = Scope::default();
    let mut templates = Vec::new();
    let mut ops = Vec::new();
    let mut root_defaults = Vec::new();
    let mut declared = Vec::new();
    for part in parts {
        for id in part.templates {
            let bind = &lowerer.ir.value_binds[id];
            let (name, span) = (bind.name.clone(), lowerer.ir.exprs[bind.expr].span);
            if let Some((_, other)) = scope.value_binds.get(&*name) {
                let diagnostic =
                    Diagnostic::error(span, format!("the template `{name}` is defined twice"))
                        .with_related(*other, "the other definition");
                lowerer.diagnostics.push(diagnostic);
                continue;
            }
            scope.value_binds.insert(name.to_string(), (id, span));
            templates.push(id);
        }
        ops.extend(part.ops);
        root_defaults.extend(part.root_defaults);
        declared.extend(part.declared);
    }
    lowerer.root_defaults = root_defaults;
    lowerer.declared = declared;
    let mut entries_of = Vec::with_capacity(documents.len());
    for document in documents {
        lowerer.file = document.file;
        let entries = root_entries(&mut lowerer, document.document);
        for entry in &entries {
            let ast::AnyEntry::ValueBind(bind) = entry else {
                continue;
            };
            let Some(name) = bind.name() else { continue };
            let span = lowerer.token_span(&name);
            if let Some((_, other)) = scope.value_binds.get(name.text()) {
                let diagnostic = Diagnostic::error(
                    span,
                    format!("the template `{}` is defined twice", name.text()),
                )
                .with_related(*other, "the other definition");
                lowerer.diagnostics.push(diagnostic);
                continue;
            }
            if RESERVED_BIND_NAMES.contains(&name.text()) {
                lowerer.error(span, format!("`{}` cannot be a template name", name.text()));
                continue;
            }
            let id = lowerer.ir.value_binds.len();
            lowerer.ir.value_binds.push(ValueBind {
                name: Arc::from(name.text()),
                expr: usize::MAX,
            });
            scope.value_binds.insert(name.text().to_owned(), (id, span));
            templates.push(id);
        }
        entries_of.push(entries);
    }
    lowerer.scopes.push(scope);

    let root_span = documents.last().map_or(Span::detached(), |document| {
        Span::new(document.file, Default::default())
    });
    for (document, entries) in documents.iter().zip(&entries_of) {
        lowerer.file = document.file;
        lowerer.position = (document.layer, document.index);
        lowerer.aliases.clear();
        if let Some((_, span)) = lowerer.attributes(document.document) {
            lowerer.error(
                span,
                "`type` names the type of a value-mode document; a data-mode document's root has no type",
            );
        }
        for entry in entries {
            lowerer.root_entry(entry, &[None], &mut ops);
        }
    }

    let Lowerer {
        ir,
        mut diagnostics,
        root_defaults,
        declared,
        ..
    } = lowerer;
    diagnostics.sort_by_key(|d| (d.span.file, d.span.range.start()));
    // Layers lowered ahead of time come first; a chain is in layer order.
    ops.sort_by_key(|op| op.layer);
    (
        LoweredData {
            ir,
            ops,
            templates,
            root_defaults,
            declared,
            root_span,
        },
        diagnostics,
    )
}

/// A lowered value-mode document.
pub(crate) struct Lowered {
    pub(crate) ir: Ir,
    pub(crate) root: ExprId,
}

const RESERVED_BIND_NAMES: [&str; 4] = ["self", "parent", "root", "default"];

/// Names visible in one block (`SPEC.md` 6.1.1).
#[derive(Default)]
struct Scope {
    entry_binds: HashMap<String, (Vec<Selector>, Key, Span)>,
    value_binds: HashMap<String, (BindId, Span)>,
}

struct Lowerer {
    file: FileId,
    ir: Ir,
    /// Every name so far, so that each is held once.
    names: HashSet<Arc<str>>,
    diagnostics: Vec<Diagnostic>,
    scopes: Vec<Scope>,
    /// `use(path as Alias)`: alias to the type name it stands for.
    aliases: HashMap<String, String>,
    /// Whether a `&` here, and in a whole-value block here, would reach the
    /// value below the root entry being lowered.
    reach: Reach,
    /// Whether the root entry being lowered uses its value below.
    used_below: bool,
    /// Lowering data-mode documents rather than a value-mode one.
    data_mode: bool,
    /// The fixed location of the block about to be lowered, if it has one:
    /// variant entries in it are lifted to root operations (`SPEC.md` 8.2).
    fixed: Option<FixedBlock>,
    /// Operations lifted out of the root entry being lowered.
    lifted: Vec<RootOp>,
    /// While lowering a lifted entry: the scope depth of its block. Value
    /// binds of that block and those around it, other than templates, do
    /// not exist where the lifted operation is evaluated.
    lift_floor: Option<usize>,
    /// The layer and document position of the document being lowered.
    position: (usize, usize),
    /// Every `..` at a root so far.
    root_defaults: Vec<RootDefaults>,
    /// Every `#![variant(group: name)]` so far.
    declared: Vec<DeclaredVariant>,
    /// Whose default a `&default` here is.
    default_target: DefaultTarget,
    /// The `&default` target of the block being lowered, for its value binds
    /// and lifted entries.
    block_target: DefaultTarget,
}

/// A block whose location is fixed: an entry's value, the right side of
/// `~` there, or a field value nested in such a block (`SPEC.md` 6.1).
#[derive(Debug, Clone)]
struct FixedBlock {
    key: Arc<str>,
    /// The block's path from the root key.
    path: Vec<Selector>,
    /// The paths of the blocks around it, innermost first.
    outer: Vec<Vec<Selector>>,
    /// The conditions it is under, as alternatives; `None` is the base.
    conditions: Vec<Option<Cond>>,
}

impl FixedBlock {
    /// The block that is the value of the entry `key` + `path` in this one.
    fn child(
        &self,
        key: &Key,
        path: &[Selector],
        span: Span,
        conditions: Vec<Option<Cond>>,
    ) -> FixedBlock {
        let mut child_path = self.path.clone();
        child_path.push(key_selector(key, span));
        child_path.extend(path.iter().cloned());
        let mut outer = vec![self.path.clone()];
        outer.extend(self.outer.iter().cloned());
        FixedBlock {
            key: self.key.clone(),
            path: child_path,
            outer,
            conditions,
        }
    }
}

/// The field names of a path, for a `&default` target.
/// What an entry's operation does to its target; `^` is a set that never
/// merges, which the entry records apart.
fn set_op(op: ast::EntryOp) -> SetOp {
    match op {
        ast::EntryOp::Set | ast::EntryOp::Replace => SetOp::Set,
        ast::EntryOp::Declare => SetOp::Declare,
        ast::EntryOp::Ensure => SetOp::Ensure,
    }
}

fn field_names(path: &[Selector]) -> Vec<Arc<str>> {
    path.iter()
        .filter_map(|s| match &s.kind {
            SelectorKind::Field(name) => Some(name.clone()),
            _ => None,
        })
        .collect()
}

/// Whether a target has a `?` after its key (`SPEC.md` 5.1).
fn key_is_optional(target: &ast::Target) -> bool {
    target.key().is_some_and(|key| key.is_optional())
}

fn key_selector(key: &Key, span: Span) -> Selector {
    let kind = match key {
        Key::Str(s) => SelectorKind::Field(s.clone()),
        Key::Int(i) => SelectorKind::Accessor(*i),
    };
    Selector {
        kind,
        span,
        optional: false,
    }
}

/// How many conditional operations one entry may stand for, through `+`
/// lists multiplied by nesting (`SPEC.md` 7.7).
const MAX_ALTERNATIVES: usize = 256;

/// Each alternative of `alternatives` with each condition of `list`: the
/// conjunction that nesting means, distributed over `+` lists (`SPEC.md`
/// 8.2). `None` if there would be more than [`MAX_ALTERNATIVES`].
fn distribute(alternatives: &[Option<Cond>], list: &[Cond]) -> Option<Vec<Option<Cond>>> {
    if alternatives.len() * list.len() > MAX_ALTERNATIVES {
        return None;
    }
    let mut out = Vec::with_capacity(alternatives.len() * list.len());
    for alternative in alternatives {
        for condition in list {
            out.push(Cond::and(alternative.as_ref(), Some(condition)));
        }
    }
    Some(out)
}

const TOO_MANY_ALTERNATIVES: &str = "`+` lists multiply through nested conditions, and this stands for more than 256 conditional operations";

/// Whether a target has a variant selector, which makes its entry a variant
/// entry.
fn has_variant(target: ast::Target) -> bool {
    target
        .selectors()
        .any(|s| matches!(s, ast::Selector::Variant(_)))
}

/// The changes a reference's variant selector makes to the context: names
/// made active, and with `!` inactive. Lowering has checked that it uses
/// only names, `!` and `&` (`SPEC.md` 8.4).
fn context_changes(condition: &Cond) -> Vec<(Arc<str>, bool, Span)> {
    fn walk(c: &Cond, active: bool, out: &mut Vec<(Arc<str>, bool, Span)>) {
        match c {
            Cond::Name(name, span) => out.push((name.clone(), active, *span)),
            Cond::Not(inner) => walk(inner, !active, out),
            Cond::And(a, b) | Cond::Or(a, b) | Cond::Xor(a, b) => {
                walk(a, active, out);
                walk(b, active, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(condition, true, &mut out);
    out
}

/// Where selectors are written, which decides what a variant selector means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectorUse {
    /// In a target: a condition.
    Target,
    /// After a late reference: a change of context.
    Reference,
    /// After anything else: an error.
    Other,
}

/// Mirrors the evaluator's `below` and `outer_below` (`SPEC.md` 6.1).
#[derive(Debug, Clone, Copy, Default)]
struct Reach {
    below: bool,
    outer: bool,
}

/// Lowers a value-mode document (`SPEC.md` 4).
pub(crate) fn lower_value_document(
    document: &ast::Document,
    file: FileId,
) -> (Lowered, Vec<Diagnostic>) {
    let mut lowerer = Lowerer {
        file,
        ir: Ir::default(),
        names: HashSet::new(),
        diagnostics: Vec::new(),
        scopes: Vec::new(),
        aliases: HashMap::new(),
        reach: Reach::default(),
        used_below: false,
        data_mode: false,
        fixed: None,
        lifted: Vec::new(),
        lift_floor: None,
        position: (0, 0),
        default_target: DefaultTarget::default(),
        block_target: DefaultTarget::default(),
        root_defaults: Vec::new(),
        declared: Vec::new(),
    };
    let type_name = lowerer.attributes(document);
    let root = match document.expr() {
        Some(expr) => {
            if let Some((_, span)) = type_name {
                lowerer.error(
                    span,
                    "`type` names the type of a document written as entries; this document is one expression",
                );
            }
            lowerer.expr(&expr)
        }
        None => {
            let span = lowerer.span(document.syntax());
            let name = type_name.map(|(name, _)| name);
            let block = lowerer.block(name, document.entries());
            lowerer.ir.add_expr(ExprKind::Block(block), span)
        }
    };
    let Lowerer {
        ir,
        mut diagnostics,
        ..
    } = lowerer;
    diagnostics.sort_by_key(|d| d.span.range.start());
    (Lowered { ir, root }, diagnostics)
}

impl Lowerer {
    /// Lowers one root entry of a data-mode document to an operation.
    /// Lowers one root entry of a data-mode document to operations, one
    /// for each of the `conditions` it is under (`SPEC.md` 7.2, 8.2).
    fn root_entry(
        &mut self,
        entry: &ast::AnyEntry,
        conditions: &[Option<Cond>],
        ops: &mut Vec<RootOp>,
    ) {
        let conditional = conditions.iter().any(Option::is_some);
        let mut replace = false;
        let (target, op, value) = match entry {
            ast::AnyEntry::Entry(entry) => {
                if let Some(bind) = entry.bind() {
                    let span = self.span(bind.syntax());
                    self.error(
                        span,
                        "a bind at the top level of a data-mode document is not needed: write `&root.key`",
                    );
                }
                let op = set_op(entry.op());
                replace = entry.op() == ast::EntryOp::Replace;
                (entry.target(), Some(op), entry.value())
            }
            ast::AnyEntry::Remove(entry) => (entry.target(), None, None),
            ast::AnyEntry::ValueBind(bind) => {
                if conditional {
                    let span = self.span(bind.syntax());
                    self.error(
                        span,
                        "a value bind cannot be inside a variant block; vary the keys that use it instead",
                    );
                    return;
                }
                // A template: its id was assigned when templates were collected.
                let Some(name) = bind.name() else { return };
                let Some(&(id, span)) = self.scopes[0].value_binds.get(name.text()) else {
                    return;
                };
                if span != self.token_span(&name) {
                    return; // A second definition, already reported.
                }
                self.reach = Reach::default();
                let expr = match bind.value() {
                    Some(value) => self.expr(&value),
                    None => self.ir.add_expr(ExprKind::Error, span),
                };
                self.ir.value_binds[id].expr = expr;
                return;
            }
            ast::AnyEntry::Spread(_) => {
                let span = self.span(entry.syntax());
                self.error(
                    span,
                    "a spread cannot be at the root of a data-mode document; the root is not a value",
                );
                return;
            }
            ast::AnyEntry::Defaults(_) => {
                let span = self.span(entry.syntax());
                for condition in conditions {
                    self.root_defaults.push(RootDefaults {
                        condition: condition.clone(),
                        span,
                    });
                }
                return;
            }
            ast::AnyEntry::VariantBlock(block) => {
                if let Some(bind) = block.bind() {
                    let span = self.span(bind.syntax());
                    self.error(span, "a bind cannot name a variant block");
                }
                let Some(variant) = block.variant() else {
                    return;
                };
                let Some(list) = self.variant_list(&variant) else {
                    return;
                };
                let Some(inner) = distribute(conditions, &list) else {
                    let span = self.span(variant.syntax());
                    self.error(span, TOO_MANY_ALTERNATIVES);
                    return;
                };
                for entry in block.entries() {
                    self.root_entry(&entry, &inner, ops);
                }
                return;
            }
        };
        let Some(target) = target else { return };
        let span = self.span(target.syntax());
        let mut lists = Vec::new();
        let Some((key, path)) = self.target_with_conditions(&target, &mut lists) else {
            return;
        };
        let target_optional = key_is_optional(&target);
        let key: Arc<str> = match key {
            Key::Str(key) => key,
            Key::Int(i) => {
                self.error(
                    span,
                    format!(
                        "root keys are ids, which are strings; write `\"{i}\"` for the key {i}"
                    ),
                );
                return;
            }
        };
        let mut alternatives = conditions.to_vec();
        for list in &lists {
            let Some(more) = distribute(&alternatives, list) else {
                self.error(span, TOO_MANY_ALTERNATIVES);
                return;
            };
            alternatives = more;
        }
        if let Some(bind) = match entry {
            ast::AnyEntry::Entry(e) => e.bind(),
            _ => None,
        } && alternatives.iter().any(Option::is_some)
        {
            let span = self.span(bind.syntax());
            self.error(span, "a bind cannot name a variant entry");
        }
        self.reach = Reach {
            below: true,
            outer: true,
        };
        self.used_below = false;
        self.fixed = Some(FixedBlock {
            key: key.clone(),
            path: path.clone(),
            outer: Vec::new(),
            conditions: alternatives.clone(),
        });
        // A root value's type is the schema's root type, or the key is a
        // field of it (`SPEC.md` 4.1).
        self.default_target = DefaultTarget {
            type_name: None,
            root_key: Some(key.clone()),
            path: field_names(&path),
        };
        let value = value.map(|value| self.expr(&value));
        self.default_target = DefaultTarget::default();
        self.fixed = None;
        self.reach = Reach::default();
        for condition in alternatives {
            ops.push(RootOp {
                key: key.clone(),
                op,
                replace,
                key_optional: target_optional,
                path: path.clone(),
                value,
                span,
                uses_below: self.used_below,
                layer: self.position.0,
                document: self.position.1,
                condition,
                blocks: Vec::new(),
            });
        }
        // Variant entries in the value apply after the entry (`SPEC.md` 8.2).
        ops.append(&mut self.lifted);
    }

    /// Lifts a variant entry in a fixed block to root operations: `field<c>:
    /// e` in the block at `web` is `web<c>.field: e` (`SPEC.md` 8.2).
    fn lift(&mut self, entry: &ast::AnyEntry, fixed: &FixedBlock, conditions: &[Option<Cond>]) {
        let mut replace = false;
        let (target, op, value) = match entry {
            ast::AnyEntry::Entry(e) => {
                if let Some(bind) = e.bind() {
                    let span = self.span(bind.syntax());
                    self.error(span, "a bind cannot name a variant entry");
                }
                let op = set_op(e.op());
                replace = e.op() == ast::EntryOp::Replace;
                (e.target(), Some(op), e.value())
            }
            ast::AnyEntry::Remove(e) => (e.target(), None, None),
            ast::AnyEntry::VariantBlock(block) => {
                let Some(variant) = block.variant() else {
                    return;
                };
                let Some(list) = self.variant_list(&variant) else {
                    return;
                };
                let Some(inner) = distribute(conditions, &list) else {
                    let span = self.span(variant.syntax());
                    self.error(span, TOO_MANY_ALTERNATIVES);
                    return;
                };
                for entry in block.entries() {
                    self.lift(&entry, fixed, &inner);
                }
                return;
            }
            ast::AnyEntry::ValueBind(_) => {
                let span = self.span(entry.syntax());
                self.error(
                    span,
                    "a value bind cannot be inside a variant block; vary the keys that use it instead",
                );
                return;
            }
            ast::AnyEntry::Spread(_) | ast::AnyEntry::Defaults(_) => {
                let span = self.span(entry.syntax());
                self.error(
                    span,
                    "a spread or `..` cannot be conditional; write the fields it would set",
                );
                return;
            }
        };
        let Some(target) = target else { return };
        let span = self.span(target.syntax());
        let mut lists = Vec::new();
        let Some((key, path)) = self.target_with_conditions(&target, &mut lists) else {
            return;
        };
        let mut alternatives = conditions.to_vec();
        for list in &lists {
            let Some(more) = distribute(&alternatives, list) else {
                self.error(span, TOO_MANY_ALTERNATIVES);
                return;
            };
            alternatives = more;
        }
        let mut full_path = fixed.path.clone();
        full_path.push(Selector {
            optional: key_is_optional(&target),
            ..key_selector(&key, span)
        });
        full_path.extend(path.iter().cloned());
        let mut blocks = vec![fixed.path.clone()];
        blocks.extend(fixed.outer.iter().cloned());

        let saved_floor = self.lift_floor.replace(self.scopes.len());
        let saved_reach = std::mem::take(&mut self.reach);
        let mut entry_target = self.block_target.clone();
        if let Key::Str(key) = &key {
            entry_target.path.push(key.clone());
        }
        entry_target.path.extend(field_names(&path));
        let saved_target = std::mem::replace(&mut self.default_target, entry_target);
        self.fixed = Some(fixed.child(&key, &path, span, alternatives.clone()));
        let value = value.map(|value| self.expr(&value));
        self.fixed = None;
        self.reach = saved_reach;
        self.lift_floor = saved_floor;
        self.default_target = saved_target;
        for condition in alternatives {
            self.lifted.push(RootOp {
                key: fixed.key.clone(),
                op,
                replace,
                key_optional: false,
                path: full_path.clone(),
                value,
                span,
                uses_below: true,
                layer: self.position.0,
                document: self.position.1,
                condition,
                blocks: blocks.clone(),
            });
        }
    }

    /// A variant selector's `+` list of conditions.
    fn variant_list(&mut self, variant: &ast::Variant) -> Option<Vec<Cond>> {
        if !self.data_mode {
            let span = self.span(variant.syntax());
            self.error(
                span,
                "variants need layered data: a value-mode document is evaluated in no context",
            );
            return None;
        }
        variant.conditions().map(|c| self.condition(&c)).collect()
    }

    fn condition(&mut self, condition: &ast::Condition) -> Option<Cond> {
        Some(match condition {
            ast::Condition::Name(name) => {
                let token = name.name()?;
                Cond::Name(Arc::from(token.text()), self.token_span(&token))
            }
            ast::Condition::Not(not) => Cond::Not(Box::new(self.condition(&not.operand()?)?)),
            ast::Condition::Paren(paren) => self.condition(&paren.inner()?)?,
            ast::Condition::Binary(binary) => {
                let lhs = Box::new(self.condition(&binary.lhs()?)?);
                let rhs = Box::new(self.condition(&binary.rhs()?)?);
                match binary.op()? {
                    ast::CondOp::And => Cond::And(lhs, rhs),
                    ast::CondOp::Or => Cond::Or(lhs, rhs),
                    ast::CondOp::Xor => Cond::Xor(lhs, rhs),
                }
            }
        })
    }

    /// A target's key and selectors, with its variant selectors taken out
    /// as conditions (`SPEC.md` 8.2: they apply to the whole root key).
    fn target_with_conditions(
        &mut self,
        target: &ast::Target,
        conditions: &mut Vec<Vec<Cond>>,
    ) -> Option<(Key, Vec<Selector>)> {
        let key = self.target_key(target)?;
        let path = self.selectors(target.selectors(), SelectorUse::Target, conditions)?;
        Some((key, path))
    }

    /// The one copy of a name.
    fn name(&mut self, name: &str) -> Arc<str> {
        if let Some(name) = self.names.get(name) {
            return name.clone();
        }
        let name: Arc<str> = Arc::from(name);
        self.names.insert(name.clone());
        name
    }

    fn span(&self, node: &crol_syntax::SyntaxNode) -> Span {
        Span::new(self.file, node.text_range())
    }

    fn token_span(&self, token: &crol_syntax::SyntaxToken) -> Span {
        Span::new(self.file, token.text_range())
    }

    fn error(&mut self, span: Span, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic::error(span, message));
    }

    fn error_expr(&mut self, span: Span, message: impl Into<String>) -> ExprId {
        self.error(span, message);
        self.ir.add_expr(ExprKind::Error, span)
    }

    fn resolve_type_name(&mut self, name: &str) -> Arc<str> {
        match self.aliases.get(name).cloned() {
            Some(alias) => self.name(&alias),
            None => self.name(name),
        }
    }

    // ---- Attributes (`SPEC.md` 4.1) ----

    /// Checks the attributes and records `use` aliases. Returns the name a
    /// `type` attribute gives.
    fn attributes(&mut self, document: &ast::Document) -> Option<(Arc<str>, Span)> {
        let mut schema = None;
        let mut type_name = None;
        // Type name (alias or last segment) to the full path it stands for.
        let mut names: HashMap<String, (String, Span)> = HashMap::new();
        for attribute in document.attributes() {
            let span = self.span(attribute.syntax());
            match attribute.kind() {
                Some(ast::AttributeKind::Use(attr)) => {
                    for path in attr.paths() {
                        let path_span = self.span(path.syntax());
                        let segments: Vec<String> =
                            path.segments().map(|t| t.text().to_owned()).collect();
                        let Some(last) = segments.last().cloned() else {
                            continue;
                        };
                        let mut full = segments.join("::");
                        if let Some(args) = path.type_args() {
                            let args = args.syntax().text().to_string();
                            full.extend(args.chars().filter(|c| !c.is_whitespace()));
                        }
                        let name = match path.alias() {
                            Some(alias) => {
                                let alias_text = alias.text().to_owned();
                                if matches!(alias_text.as_str(), "true" | "false")
                                    || RESERVED_BIND_NAMES.contains(&alias_text.as_str())
                                {
                                    self.error(
                                        self.token_span(&alias),
                                        format!("use: `{alias_text}` is a reserved word and cannot be an alias"),
                                    );
                                }
                                self.aliases.insert(alias_text.clone(), last);
                                alias_text
                            }
                            None => last,
                        };
                        if let Some((other, other_span)) = names.get(&name)
                            && *other != full
                        {
                            let diagnostic = Diagnostic::error(
                                path_span,
                                format!("use: `{name}` already names `{other}`"),
                            )
                            .with_related(*other_span, "the other `use`");
                            self.diagnostics.push(diagnostic);
                        }
                        names.insert(name, (full, path_span));
                    }
                }
                Some(ast::AttributeKind::Schema(attr)) => {
                    if schema.replace(span).is_some() {
                        self.error(span, "a document has at most one `schema` attribute");
                    }
                    if let Some(string) = attr.string() {
                        let text = literal::string(string.text());
                        if !text.starts_with("https://") && !text.starts_with("crate:") {
                            self.error(
                                self.token_span(&string),
                                "schema: a schema string is a URL (`https://...`) or a crate (`crate:name@version`)",
                            );
                        }
                    }
                }
                Some(ast::AttributeKind::Type(attr)) => {
                    if type_name.is_some() {
                        self.error(span, "a document has at most one `type` attribute");
                    } else if let Some(name) = attr.name() {
                        type_name = Some((name.text().to_owned(), span));
                    }
                }
                Some(ast::AttributeKind::Extends(_)) if !self.data_mode => {
                    self.error(
                        span,
                        "`extends` names layers below a document, and a value-mode document has no layers",
                    );
                }
                Some(ast::AttributeKind::Extends(_)) => {}
                Some(ast::AttributeKind::Variant(_)) if !self.data_mode => {
                    self.error(
                        span,
                        "`variant` declares a variant of data, and a value-mode document has no variants",
                    );
                }
                Some(ast::AttributeKind::Variant(attr)) => {
                    if let (Some(group), Some(name)) = (attr.group(), attr.name()) {
                        let (group, name) = (self.name(group.text()), self.name(name.text()));
                        self.declared.push(DeclaredVariant { group, name, span });
                    }
                }
                None => {}
            }
        }
        if let Some(expr) = document.expr()
            && expr.syntax().kind() == SyntaxKind::BLOCK
            && let Some((_, span)) = &type_name
            && let ast::Expr::Block(block) = &expr
            && block.type_name().is_some()
        {
            self.error(
                *span,
                "`type` on a document whose block already has a type name",
            );
            return None;
        }
        type_name.map(|(name, span)| (self.resolve_type_name(&name), span))
    }

    // ---- Blocks and entries (`SPEC.md` 5) ----

    fn block(
        &mut self,
        name: Option<Arc<str>>,
        entries: impl Iterator<Item = ast::AnyEntry>,
    ) -> BlockId {
        let fixed = self.fixed.take();
        // The block's own `&default` target: its type name, or the field
        // path it is at from the nearest enclosing one.
        let outer_target = std::mem::take(&mut self.default_target);
        let block_target = match &name {
            Some(name) => DefaultTarget {
                type_name: Some(name.clone()),
                root_key: None,
                path: Vec::new(),
            },
            None => outer_target.clone(),
        };
        let outer_block_target = std::mem::replace(&mut self.block_target, block_target.clone());
        let entries: Vec<_> = entries.collect();
        let id = self.ir.blocks.len();
        self.ir.blocks.push(Block {
            name,
            spreads: Vec::new(),
            entries: Vec::new(),
            defaults: false,
            value_binds: Vec::new(),
        });

        // Binds are visible anywhere in their block, before or after them.
        let mut scope = Scope::default();
        let mut value_binds = Vec::new();
        for entry in &entries {
            match entry {
                ast::AnyEntry::Entry(entry) => {
                    if let Some(bind) = entry.bind()
                        && let Some(name) = bind.name()
                        && let Some(target) = entry.target()
                    {
                        let span = self.token_span(&name);
                        if self.check_bind_name(name.text(), span, &scope)
                            && let Some((key, path)) = self.target(&target)
                        {
                            scope
                                .entry_binds
                                .insert(name.text().to_owned(), (path, key, span));
                        }
                    }
                }
                ast::AnyEntry::ValueBind(bind) => {
                    if let Some(name) = bind.name() {
                        let span = self.token_span(&name);
                        if self.check_bind_name(name.text(), span, &scope) {
                            let bind_id = self.ir.value_binds.len();
                            self.ir.value_binds.push(ValueBind {
                                name: Arc::from(name.text()),
                                expr: usize::MAX,
                            });
                            scope
                                .value_binds
                                .insert(name.text().to_owned(), (bind_id, span));
                            value_binds.push((bind_id, bind.clone()));
                        }
                    }
                }
                _ => {}
            }
        }
        self.scopes.push(scope);

        // In a whole-value block, an entry's `&` sees through to the value
        // below the entry the block is the value of; in a patch block, the
        // caller has cleared `outer`.
        let outer = self.reach;
        let entry_reach = Reach {
            below: outer.outer,
            outer: outer.outer,
        };
        let mut spreads = Vec::new();
        let mut lowered = Vec::new();
        let mut defaults = false;
        for entry in &entries {
            self.reach = entry_reach;
            let is_variant = match entry {
                ast::AnyEntry::VariantBlock(_) => true,
                ast::AnyEntry::Entry(e) => e.target().is_some_and(has_variant),
                ast::AnyEntry::Remove(e) => e.target().is_some_and(has_variant),
                _ => false,
            };
            if is_variant {
                match &fixed {
                    Some(fixed) => {
                        let conditions = fixed.conditions.clone();
                        self.lift(entry, fixed, &conditions);
                    }
                    None if !self.data_mode => {
                        let span = self.span(entry.syntax());
                        self.error(
                            span,
                            "variants need layered data: a value-mode document is evaluated in no context",
                        );
                    }
                    None => {
                        let span = self.span(entry.syntax());
                        self.error(
                            span,
                            "a variant entry is only allowed in a block whose location is fixed: an entry's value, the right side of `~` there, or a field value in such a block",
                        );
                    }
                }
                continue;
            }
            match entry {
                ast::AnyEntry::Entry(entry) => {
                    let Some(target) = entry.target() else {
                        continue;
                    };
                    let Some((key, path)) = self.target(&target) else {
                        continue;
                    };
                    let target_span = self.span(target.syntax());
                    let value = entry.value();
                    // Most values are literals, which read neither of these.
                    if !matches!(value, Some(ast::Expr::Literal(_))) {
                        let mut entry_target = block_target.clone();
                        if let Key::Str(key) = &key {
                            entry_target.path.push(key.clone());
                        }
                        entry_target.path.extend(field_names(&path));
                        self.default_target = entry_target;
                        self.fixed = fixed.as_ref().map(|f| {
                            let conditions = f.conditions.clone();
                            f.child(&key, &path, target_span, conditions)
                        });
                    }
                    let value = value.map(|value| self.expr(&value));
                    let op = entry.op();
                    lowered.push(Entry {
                        op: Some(set_op(op)),
                        replace: op == ast::EntryOp::Replace,
                        key,
                        key_optional: key_is_optional(&target),
                        path,
                        value,
                        span: target_span,
                    });
                }
                ast::AnyEntry::Remove(entry) => {
                    let Some(target) = entry.target() else {
                        continue;
                    };
                    if let Some((key, path)) = self.target(&target) {
                        lowered.push(Entry {
                            op: None,
                            replace: false,
                            key,
                            key_optional: key_is_optional(&target),
                            path,
                            value: None,
                            span: self.span(target.syntax()),
                        });
                    }
                }
                ast::AnyEntry::Spread(spread) => {
                    if let Some(operand) = spread.operand() {
                        self.reach = Reach {
                            below: outer.below,
                            outer: false,
                        };
                        // `..&default` is the block's own default.
                        self.default_target = block_target.clone();
                        spreads.push(self.expr(&operand));
                    }
                }
                ast::AnyEntry::Defaults(_) => defaults = true,
                ast::AnyEntry::ValueBind(_) | ast::AnyEntry::VariantBlock(_) => {}
            }
            self.fixed = None;
        }
        let bind_ids = value_binds.iter().map(|(id, _)| *id).collect();
        self.reach = Reach::default();
        self.default_target = block_target.clone();
        for (bind_id, bind) in value_binds {
            let expr = match bind.value() {
                Some(value) => self.expr(&value),
                None => self.ir.add_expr(ExprKind::Error, self.span(bind.syntax())),
            };
            self.ir.value_binds[bind_id].expr = expr;
        }
        self.reach = outer;
        self.default_target = outer_target;
        self.block_target = outer_block_target;
        self.scopes.pop();

        let block = &mut self.ir.blocks[id];
        block.spreads = spreads;
        block.entries = lowered;
        block.defaults = defaults;
        block.value_binds = bind_ids;
        id
    }

    /// Checks a new bind's name against reserved words and the binds already
    /// visible (`SPEC.md` 6.1.1: there is no shadowing).
    fn check_bind_name(&mut self, name: &str, span: Span, scope: &Scope) -> bool {
        if RESERVED_BIND_NAMES.contains(&name) {
            self.error(span, format!("`{name}` cannot be a bind name"));
            return false;
        }
        let visible = std::iter::once(scope)
            .chain(self.scopes.iter().rev())
            .find_map(|s| {
                s.entry_binds
                    .get(name)
                    .map(|b| b.2)
                    .or_else(|| s.value_binds.get(name).map(|b| b.1))
            });
        if let Some(other) = visible {
            let diagnostic = Diagnostic::error(span, format!("`{name}` is already bound"))
                .with_related(other, "the other bind");
            self.diagnostics.push(diagnostic);
            return false;
        }
        true
    }

    /// A target's key and selectors, for an entry without variants.
    fn target(&mut self, target: &ast::Target) -> Option<(Key, Vec<Selector>)> {
        let key = self.target_key(target)?;
        let mut conditions = Vec::new();
        let path = self.selectors(target.selectors(), SelectorUse::Target, &mut conditions)?;
        Some((key, path))
    }

    fn target_key(&mut self, target: &ast::Target) -> Option<Key> {
        let key_node = target.key()?;
        let key = if let Some(accessor) = key_node.accessor() {
            self.accessor_key(&accessor)?
        } else {
            let token = key_node.token()?;
            match token.kind() {
                SyntaxKind::IDENT => Key::Str(self.name(token.text())),
                SyntaxKind::STRING | SyntaxKind::RAW_STRING => {
                    Key::Str(self.name(&literal::string(token.text())))
                }
                SyntaxKind::INT => match literal::int(token.text(), false) {
                    Ok(value) => Key::Int(value),
                    Err(message) => {
                        self.error(self.token_span(&token), message);
                        return None;
                    }
                },
                _ => return None,
            }
        };
        Some(key)
    }

    fn accessor_key(&mut self, accessor: &ast::Accessor) -> Option<Key> {
        let token = accessor.key()?;
        match token.kind() {
            SyntaxKind::INT => match literal::int(token.text(), accessor.is_negative()) {
                Ok(value) => Some(Key::Int(value)),
                Err(message) => {
                    self.error(self.token_span(&token), message);
                    None
                }
            },
            _ => Some(Key::Str(self.name(&literal::string(token.text())))),
        }
    }

    /// Lowers selectors. In a target, variant selectors are taken out into
    /// `conditions`; after a reference, they change the context.
    fn selectors(
        &mut self,
        selectors: impl Iterator<Item = ast::Selector>,
        usage: SelectorUse,
        conditions: &mut Vec<Vec<Cond>>,
    ) -> Option<Vec<Selector>> {
        let mut out = Vec::new();
        for selector in selectors {
            let span = self.span(selector.syntax());
            let kind = match &selector {
                ast::Selector::Field(field) => SelectorKind::Field(self.name(field.name()?.text())),
                ast::Selector::Index(index) => {
                    let token = index.index()?;
                    match token.text().parse() {
                        Ok(i) => SelectorKind::Element(i),
                        Err(_) => {
                            self.error(span, "tuple index out of range");
                            return None;
                        }
                    }
                }
                ast::Selector::Accessor(accessor) => match self.accessor_key(accessor)? {
                    Key::Str(s) => SelectorKind::Field(s),
                    Key::Int(i) => SelectorKind::Accessor(i),
                },
                ast::Selector::Variant(variant) => match usage {
                    SelectorUse::Target => {
                        conditions.push(self.variant_list(variant)?);
                        continue;
                    }
                    SelectorUse::Reference => {
                        let list = self.variant_list(variant)?;
                        SelectorKind::Context(context_changes(list.first()?))
                    }
                    SelectorUse::Other => {
                        self.error(
                            span,
                            "a variant selector reads a value in another context, so it must follow a late reference (`&root`, `&self`, `&parent` or a bind)",
                        );
                        return None;
                    }
                },
                // The parser has reported it.
                ast::Selector::Parent(_) => return None,
            };
            out.push(Selector {
                kind,
                span,
                optional: selector.is_optional(),
            });
        }
        Some(out)
    }

    // ---- Expressions (`SPEC.md` 6) ----

    fn expr(&mut self, expr: &ast::Expr) -> ExprId {
        let span = self.span(expr.syntax());
        // Only a block here, or on the right of `~`, keeps a fixed location.
        let fixed = self.fixed.take();
        match expr {
            ast::Expr::Literal(literal) => self.literal(literal, false, span),
            ast::Expr::Prefix(prefix) if prefix.is_not() => match prefix.operand() {
                Some(operand) => {
                    let operand = self.detached(|this| this.expr(&operand));
                    self.ir.add_expr(ExprKind::Not(operand), span)
                }
                None => self.ir.add_expr(ExprKind::Error, span),
            },
            ast::Expr::Prefix(prefix) => match prefix.operand() {
                // `-` directly before a number is part of it (`SPEC.md` 6.2).
                Some(ast::Expr::Literal(literal))
                    if literal.token().is_some_and(|t| {
                        matches!(t.kind(), SyntaxKind::INT | SyntaxKind::FLOAT)
                    }) =>
                {
                    self.literal(&literal, true, span)
                }
                Some(operand) => {
                    let operand = self.detached(|this| this.expr(&operand));
                    self.ir.add_expr(ExprKind::Neg(operand), span)
                }
                None => self.ir.add_expr(ExprKind::Error, span),
            },
            ast::Expr::Id(id) => {
                let Some(name) = id.name() else {
                    return self.ir.add_expr(ExprKind::Error, span);
                };
                let text = match name.kind() {
                    SyntaxKind::IDENT => name.text().to_owned(),
                    _ => literal::string(name.text()),
                };
                let text = self.name(&text);
                self.ir.add_expr(ExprKind::Id(text), span)
            }
            ast::Expr::Reference(reference) => self.reference(reference, span),
            ast::Expr::Block(block) => {
                let name = block
                    .type_name()
                    .map(|name| self.resolve_type_name(name.text()));
                self.fixed = fixed;
                let id = self.block(name, block.entries());
                self.ir.add_expr(ExprKind::Block(id), span)
            }
            ast::Expr::List(list) => {
                let elements =
                    self.detached(|this| list.elements().map(|e| this.expr(&e)).collect());
                self.ir.add_expr(ExprKind::List(elements), span)
            }
            ast::Expr::Tuple(tuple) => {
                let elements: Vec<_> =
                    self.detached(|this| tuple.elements().map(|e| this.expr(&e)).collect());
                let kind = if elements.is_empty() {
                    ExprKind::Unit
                } else {
                    ExprKind::Tuple(elements)
                };
                self.ir.add_expr(kind, span)
            }
            ast::Expr::NamedTuple(named) => {
                let name = named
                    .name()
                    .map_or_else(|| Arc::from(""), |n| self.resolve_type_name(n.text()));
                let elements =
                    self.detached(|this| named.elements().map(|e| this.expr(&e)).collect());
                self.ir
                    .add_expr(ExprKind::Named(Box::new((name, elements))), span)
            }
            ast::Expr::MacroCall(call) => self.macro_call(call, span),
            ast::Expr::UnitName(unit) => {
                let name = unit
                    .name()
                    .map_or_else(|| Arc::from(""), |n| self.resolve_type_name(n.text()));
                self.ir.add_expr(ExprKind::UnitName(name), span)
            }
            ast::Expr::Paren(paren) => match paren.inner() {
                Some(inner) => {
                    self.fixed = fixed;
                    self.expr(&inner)
                }
                None => self.ir.add_expr(ExprKind::Error, span),
            },
            ast::Expr::Path(path) => {
                let Some(base) = path.base() else {
                    return self.ir.add_expr(ExprKind::Error, span);
                };
                if let Some(error) = self.check_reference_variants(&base, path) {
                    return self.error_expr(span, error);
                }
                let usage = match &base {
                    ast::Expr::Reference(r)
                        if !matches!(
                            r.kind(),
                            ast::ReferenceKind::Below | ast::ReferenceKind::Default
                        ) =>
                    {
                        SelectorUse::Reference
                    }
                    _ => SelectorUse::Other,
                };
                let base = self.detached(|this| this.expr(&base));
                match self.selectors(path.selectors(), usage, &mut Vec::new()) {
                    Some(selectors) => self
                        .ir
                        .add_expr(ExprKind::Path(base, selectors.into()), span),
                    None => self.ir.add_expr(ExprKind::Error, span),
                }
            }
            ast::Expr::Binary(binary) => {
                let op = match binary.op() {
                    Some(ast::BinaryOp::Add) => BinOp::Add,
                    Some(ast::BinaryOp::Sub) => BinOp::Sub,
                    Some(ast::BinaryOp::Mul) => BinOp::Mul,
                    Some(ast::BinaryOp::Div) => BinOp::Div,
                    Some(ast::BinaryOp::Rem) => BinOp::Rem,
                    Some(ast::BinaryOp::Merge) => BinOp::Merge,
                    Some(ast::BinaryOp::MergeShallow) => BinOp::MergeShallow,
                    Some(ast::BinaryOp::Fallback) => BinOp::Fallback,
                    None => return self.ir.add_expr(ExprKind::Error, span),
                };
                let (Some(lhs), Some(rhs)) = (binary.lhs(), binary.rhs()) else {
                    return self.ir.add_expr(ExprKind::Error, span);
                };
                let (lhs, rhs) = match op {
                    // Operands at the entry's position.
                    BinOp::Fallback => (self.expr(&lhs), self.expr(&rhs)),
                    // A block on the right is a patch block: its entries see
                    // the left operand, not the value below.
                    BinOp::Merge | BinOp::MergeShallow => {
                        let lhs = self.expr(&lhs);
                        self.fixed = fixed;
                        let rhs = if matches!(rhs, ast::Expr::Block(_)) {
                            let saved = self.reach;
                            self.reach.outer = false;
                            let rhs = self.expr(&rhs);
                            self.reach = saved;
                            rhs
                        } else {
                            self.expr(&rhs)
                        };
                        (lhs, rhs)
                    }
                    _ => self.detached(|this| (this.expr(&lhs), this.expr(&rhs))),
                };
                self.ir.add_expr(ExprKind::Binary(op, lhs, rhs), span)
            }
        }
    }

    /// Checks the variant selectors in a reference's path (`SPEC.md` 8.4):
    /// only names, `!` and `&`, and none on `&` or `&default`.
    fn check_reference_variants(&self, base: &ast::Expr, path: &ast::PathExpr) -> Option<String> {
        let variants: Vec<_> = path
            .selectors()
            .filter_map(|s| match s {
                ast::Selector::Variant(v) => Some(v),
                _ => None,
            })
            .collect();
        if variants.is_empty() {
            return None;
        }
        if let ast::Expr::Reference(reference) = base
            && matches!(
                reference.kind(),
                ast::ReferenceKind::Below | ast::ReferenceKind::Default
            )
        {
            return Some(
                "a variant selector cannot follow `&` or `&default`, which do not depend on the context"
                    .to_owned(),
            );
        }
        for variant in variants {
            if variant.conditions().count() > 1 {
                return Some(
                    "a `+` list does not name one context, so it cannot select one".to_owned(),
                );
            }
            let bad = variant
                .syntax()
                .descendants()
                .any(|node| match node.kind() {
                    SyntaxKind::COND_PAREN => true,
                    SyntaxKind::COND_BINARY => ast::CondBinary::cast(node)
                        .and_then(|b| b.op())
                        .is_some_and(|op| op != ast::CondOp::And),
                    _ => false,
                });
            if bad {
                return Some(
                    "a variant selector in a reference may only use names, `!` and `&`, since it must name one context"
                        .to_owned(),
                );
            }
        }
        None
    }

    fn literal(&mut self, literal: &ast::Literal, negative: bool, span: Span) -> ExprId {
        let Some(token) = literal.token() else {
            return self.ir.add_expr(ExprKind::Error, span);
        };
        let kind = match token.kind() {
            SyntaxKind::INT => match literal::int(token.text(), negative) {
                Ok(value) => ExprKind::Int(Wide(value)),
                Err(message) => return self.error_expr(span, message),
            },
            SyntaxKind::FLOAT => match literal::float(token.text(), negative) {
                Ok(value) => ExprKind::Float(value),
                Err(message) => return self.error_expr(span, message),
            },
            SyntaxKind::STRING | SyntaxKind::RAW_STRING => {
                ExprKind::Str(Arc::from(literal::string(token.text())))
            }
            SyntaxKind::CHAR => ExprKind::Char(literal::char(token.text())),
            SyntaxKind::TRUE_KW => ExprKind::Bool(true),
            SyntaxKind::FALSE_KW => ExprKind::Bool(false),
            _ => ExprKind::Error,
        };
        self.ir.add_expr(kind, span)
    }

    /// Lowers an operand that is not at the entry's position: a block in it
    /// does not see through to the value below.
    fn detached<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let saved = self.reach;
        self.reach.outer = false;
        let result = f(self);
        self.reach = saved;
        result
    }

    /// A built-in macro (`SPEC.md` 6.7).
    fn macro_call(&mut self, call: &ast::MacroCall, span: Span) -> ExprId {
        let name = call
            .name()
            .map_or_else(String::new, |n| n.text().to_owned());
        let args: Vec<ExprId> = self.detached(|this| call.args().map(|e| this.expr(&e)).collect());
        let Some(kind) = MacroKind::from_name(&name) else {
            self.error(
                span,
                format!("there is no macro `{name}!`; the macros are `round!`, `floor!`, `ceil!` and `clamp!`"),
            );
            return self.ir.add_expr(ExprKind::Error, span);
        };
        if args.len() != kind.arity() {
            let (name, arity, found) = (kind.name(), kind.arity(), args.len());
            let s = if arity == 1 { "" } else { "s" };
            self.error(
                span,
                format!("{name} takes {arity} argument{s}, not {found}"),
            );
            return self.ir.add_expr(ExprKind::Error, span);
        }
        self.ir.add_expr(ExprKind::Macro(kind, args.into()), span)
    }

    fn reference(&mut self, reference: &ast::Reference, span: Span) -> ExprId {
        let kind = match reference.kind() {
            ast::ReferenceKind::Below => {
                if self.reach.below {
                    self.used_below = true;
                }
                RefKind::Below
            }
            ast::ReferenceKind::SelfValue => RefKind::SelfValue,
            ast::ReferenceKind::Parent(n) => RefKind::Parent(n),
            ast::ReferenceKind::Root => RefKind::Root,
            ast::ReferenceKind::Default => RefKind::Default(Box::new(self.default_target.clone())),
            ast::ReferenceKind::Bind(name) => {
                let name = name.text();
                let innermost = self.scopes.len();
                let found = self
                    .scopes
                    .iter()
                    .enumerate()
                    .rev()
                    .find_map(|(depth, scope)| {
                        if let Some((target, key, _)) = scope.entry_binds.get(name) {
                            let mut path = vec![Selector {
                                kind: match key {
                                    Key::Str(s) => SelectorKind::Field(s.clone()),
                                    Key::Int(i) => SelectorKind::Accessor(*i),
                                },
                                span,
                                optional: false,
                            }];
                            path.extend(target.iter().cloned());
                            Some((
                                depth,
                                RefKind::Bind {
                                    up: innermost - 1 - depth,
                                    target: path.into(),
                                },
                            ))
                        } else {
                            scope
                                .value_binds
                                .get(name)
                                .map(|(id, _)| (depth, RefKind::ValueBind(*id)))
                        }
                    });
                match found {
                    // A value bind of a block around a lifted variant entry
                    // is not where the entry is evaluated; templates are.
                    Some((depth, RefKind::ValueBind(_)))
                        if depth > 0 && self.lift_floor.is_some_and(|floor| depth < floor) =>
                    {
                        return self.error_expr(
                            span,
                            format!("a variant entry cannot use `&{name}`, a value bind of a block around it; make it a template"),
                        );
                    }
                    Some((_, kind)) => kind,
                    None => {
                        return self.error_expr(
                            span,
                            format!("no bind `{name}` is visible; did you mean `&self.{name}`?"),
                        );
                    }
                }
            }
        };
        self.ir.add_expr(ExprKind::Ref(kind), span)
    }
}
