//! Lowering the syntax tree to the IR, with the checks `SPEC.md` 13 calls
//! reading errors: attributes, binds and their scopes, and the forms of
//! variant selectors.

use std::collections::HashMap;
use std::rc::Rc;

use crol_syntax::SyntaxKind;
use crol_syntax::ast::{self, AstNode};

use crate::diagnostic::Diagnostic;
use crate::ir::*;
use crate::literal;
use crate::source::{FileId, Span};
use crate::value::Key;

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
    diagnostics: Vec<Diagnostic>,
    scopes: Vec<Scope>,
    /// `use(path as Alias)`: alias to the type name it stands for.
    aliases: HashMap<String, String>,
}

/// Lowers a value-mode document (`SPEC.md` 4).
pub(crate) fn lower_value_document(
    document: &ast::Document,
    file: FileId,
) -> (Lowered, Vec<Diagnostic>) {
    let mut lowerer = Lowerer {
        file,
        ir: Ir::default(),
        diagnostics: Vec::new(),
        scopes: Vec::new(),
        aliases: HashMap::new(),
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

    fn resolve_type_name(&self, name: &str) -> Rc<str> {
        Rc::from(self.aliases.get(name).map_or(name, String::as_str))
    }

    // ---- Attributes (`SPEC.md` 4.1) ----

    /// Checks the attributes and records `use` aliases. Returns the name a
    /// `type` attribute gives.
    fn attributes(&mut self, document: &ast::Document) -> Option<(Rc<str>, Span)> {
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
                Some(ast::AttributeKind::Extends(_)) => {
                    self.error(
                        span,
                        "`extends` names layers below a document, and a value-mode document has no layers",
                    );
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
        name: Option<Rc<str>>,
        entries: impl Iterator<Item = ast::AnyEntry>,
    ) -> BlockId {
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
                                name: Rc::from(name.text()),
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

        let mut spreads = Vec::new();
        let mut lowered = Vec::new();
        let mut defaults = false;
        for entry in &entries {
            match entry {
                ast::AnyEntry::Entry(entry) => {
                    let Some(target) = entry.target() else {
                        continue;
                    };
                    let Some((key, path)) = self.target(&target) else {
                        continue;
                    };
                    let op = match entry.op() {
                        ast::EntryOp::Set => SetOp::Set,
                        ast::EntryOp::Declare => SetOp::Declare,
                        ast::EntryOp::Ensure => SetOp::Ensure,
                    };
                    let value = entry.value().map(|value| self.expr(&value));
                    lowered.push(Entry {
                        op: Some(op),
                        key,
                        path,
                        value,
                        span: self.span(target.syntax()),
                    });
                }
                ast::AnyEntry::Remove(entry) => {
                    let Some(target) = entry.target() else {
                        continue;
                    };
                    if let Some((key, path)) = self.target(&target) {
                        lowered.push(Entry {
                            op: None,
                            key,
                            path,
                            value: None,
                            span: self.span(target.syntax()),
                        });
                    }
                }
                ast::AnyEntry::Spread(spread) => {
                    if let Some(operand) = spread.operand() {
                        spreads.push(self.expr(&operand));
                    }
                }
                ast::AnyEntry::Defaults(_) => defaults = true,
                ast::AnyEntry::ValueBind(_) => {}
                ast::AnyEntry::VariantBlock(block) => {
                    let span = self.span(block.syntax());
                    self.error(span, "variants are not implemented yet");
                }
            }
        }
        let bind_ids = value_binds.iter().map(|(id, _)| *id).collect();
        for (bind_id, bind) in value_binds {
            let expr = match bind.value() {
                Some(value) => self.expr(&value),
                None => self.ir.add_expr(ExprKind::Error, self.span(bind.syntax())),
            };
            self.ir.value_binds[bind_id].expr = expr;
        }
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

    /// A target's key and selectors.
    fn target(&mut self, target: &ast::Target) -> Option<(Key, Vec<Selector>)> {
        let key_node = target.key()?;
        let key = if let Some(accessor) = key_node.accessor() {
            self.accessor_key(&accessor)?
        } else {
            let token = key_node.token()?;
            match token.kind() {
                SyntaxKind::IDENT => Key::Str(token.text().to_owned()),
                SyntaxKind::STRING | SyntaxKind::RAW_STRING => {
                    Key::Str(literal::string(token.text()))
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
        let path = self.selectors(target.selectors())?;
        Some((key, path))
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
            _ => Some(Key::Str(literal::string(token.text()))),
        }
    }

    fn selectors(
        &mut self,
        selectors: impl Iterator<Item = ast::Selector>,
    ) -> Option<Vec<Selector>> {
        let mut out = Vec::new();
        for selector in selectors {
            let span = self.span(selector.syntax());
            let kind = match &selector {
                ast::Selector::Field(field) => SelectorKind::Field(Rc::from(field.name()?.text())),
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
                    Key::Str(s) => SelectorKind::Field(Rc::from(s)),
                    Key::Int(i) => SelectorKind::Accessor(i),
                },
                ast::Selector::Variant(_) => {
                    self.error(span, "variants are not implemented yet");
                    return None;
                }
                // The parser has reported it.
                ast::Selector::Parent(_) => return None,
            };
            out.push(Selector { kind, span });
        }
        Some(out)
    }

    // ---- Expressions (`SPEC.md` 6) ----

    fn expr(&mut self, expr: &ast::Expr) -> ExprId {
        let span = self.span(expr.syntax());
        match expr {
            ast::Expr::Literal(literal) => self.literal(literal, false, span),
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
                    let operand = self.expr(&operand);
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
                self.ir.add_expr(ExprKind::Id(Rc::from(text)), span)
            }
            ast::Expr::Reference(reference) => self.reference(reference, span),
            ast::Expr::Block(block) => {
                let name = block
                    .type_name()
                    .map(|name| self.resolve_type_name(name.text()));
                let id = self.block(name, block.entries());
                self.ir.add_expr(ExprKind::Block(id), span)
            }
            ast::Expr::List(list) => {
                let elements = list.elements().map(|e| self.expr(&e)).collect();
                self.ir.add_expr(ExprKind::List(elements), span)
            }
            ast::Expr::Tuple(tuple) => {
                let elements: Vec<_> = tuple.elements().map(|e| self.expr(&e)).collect();
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
                    .map_or_else(|| Rc::from(""), |n| self.resolve_type_name(n.text()));
                let elements = named.elements().map(|e| self.expr(&e)).collect();
                self.ir.add_expr(ExprKind::Named(name, elements), span)
            }
            ast::Expr::UnitName(unit) => {
                let name = unit
                    .name()
                    .map_or_else(|| Rc::from(""), |n| self.resolve_type_name(n.text()));
                self.ir.add_expr(ExprKind::UnitName(name), span)
            }
            ast::Expr::Paren(paren) => match paren.inner() {
                Some(inner) => self.expr(&inner),
                None => self.ir.add_expr(ExprKind::Error, span),
            },
            ast::Expr::Path(path) => {
                let Some(base) = path.base() else {
                    return self.ir.add_expr(ExprKind::Error, span);
                };
                if let Some(error) = self.check_reference_variants(&base, path) {
                    return self.error_expr(span, error);
                }
                let base = self.expr(&base);
                match self.selectors(path.selectors()) {
                    Some(selectors) => self.ir.add_expr(ExprKind::Path(base, selectors), span),
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
                    Some(ast::BinaryOp::Fallback) => BinOp::Fallback,
                    None => return self.ir.add_expr(ExprKind::Error, span),
                };
                let (Some(lhs), Some(rhs)) = (binary.lhs(), binary.rhs()) else {
                    return self.ir.add_expr(ExprKind::Error, span);
                };
                let lhs = self.expr(&lhs);
                let rhs = self.expr(&rhs);
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
                Ok(value) => ExprKind::Int(value),
                Err(message) => return self.error_expr(span, message),
            },
            SyntaxKind::FLOAT => match literal::float(token.text(), negative) {
                Ok(value) => ExprKind::Float(value),
                Err(message) => return self.error_expr(span, message),
            },
            SyntaxKind::STRING | SyntaxKind::RAW_STRING => {
                ExprKind::Str(Rc::from(literal::string(token.text())))
            }
            SyntaxKind::CHAR => ExprKind::Char(literal::char(token.text())),
            SyntaxKind::TRUE_KW => ExprKind::Bool(true),
            SyntaxKind::FALSE_KW => ExprKind::Bool(false),
            _ => ExprKind::Error,
        };
        self.ir.add_expr(kind, span)
    }

    fn reference(&mut self, reference: &ast::Reference, span: Span) -> ExprId {
        let kind = match reference.kind() {
            ast::ReferenceKind::Below => RefKind::Below,
            ast::ReferenceKind::SelfValue => RefKind::SelfValue,
            ast::ReferenceKind::Parent(n) => RefKind::Parent(n),
            ast::ReferenceKind::Root => RefKind::Root,
            ast::ReferenceKind::Default => RefKind::Default,
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
                                    Key::Str(s) => SelectorKind::Field(Rc::from(s.as_str())),
                                    Key::Int(i) => SelectorKind::Accessor(*i),
                                },
                                span,
                            }];
                            path.extend(target.iter().cloned());
                            Some(RefKind::Bind {
                                up: innermost - 1 - depth,
                                target: path,
                            })
                        } else {
                            scope
                                .value_binds
                                .get(name)
                                .map(|(id, _)| RefKind::ValueBind(*id))
                        }
                    });
                match found {
                    Some(kind) => kind,
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
