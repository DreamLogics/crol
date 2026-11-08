//! The requests the server answers (`SPEC.md` 12.5).

use crol_eval::{Key, PathStep, StepKind, Value, ValueKind, print};
use crol_syntax::ast::{self, AstNode};
use crol_syntax::{LineIndex, SyntaxKind, SyntaxNode, SyntaxToken, TextRange, TextSize};
use lsp_types::{
    DocumentSymbol, Hover, HoverContents, Location, MarkupContent, MarkupKind, Position,
    SymbolKind, TextEdit, Uri,
};

use crate::Settings;
use crate::convert::{self, Encoding};
use crate::place::{self, Scope};
use crate::workspace::Snapshot;

// ---- Document symbols ----

/// The outline: root entries with their kind and type name, and nested
/// blocks as children (`SPEC.md` 12.5).
pub(crate) fn symbols(text: &str, encoding: Encoding) -> Vec<DocumentSymbol> {
    let parse = crol_syntax::parse(text);
    let index = LineIndex::new(text);
    let document = parse.document();
    let entries: Vec<ast::AnyEntry> = match document.expr() {
        Some(ast::Expr::Block(block)) => block.entries().collect(),
        _ => document.entries().collect(),
    };
    entries
        .iter()
        .filter_map(|e| entry_symbol(e, "", text, &index, encoding))
        .collect()
}

fn target_text(target: &ast::Target) -> String {
    target
        .syntax()
        .text()
        .to_string()
        .split_whitespace()
        .collect()
}

#[allow(deprecated)]
fn entry_symbol(
    entry: &ast::AnyEntry,
    prefix: &str,
    text: &str,
    index: &LineIndex,
    encoding: Encoding,
) -> Option<DocumentSymbol> {
    let range = convert::range(text, index, entry.syntax().text_range(), encoding);
    let (name, detail, kind, selection, children) = match entry {
        ast::AnyEntry::Entry(e) => {
            let target = e.target()?;
            let op = e.op_token().map_or(String::new(), |t| t.text().to_owned());
            let (detail, children) = match e.value() {
                Some(ast::Expr::Block(block)) => (
                    block.type_name().map(|t| t.text().to_owned()),
                    block_children(&block, text, index, encoding),
                ),
                _ => (None, Vec::new()),
            };
            let kind = if children.is_empty() {
                SymbolKind::FIELD
            } else {
                SymbolKind::STRUCT
            };
            (
                format!("{prefix}{op}{}", target_text(&target)),
                detail,
                kind,
                target.syntax().text_range(),
                children,
            )
        }
        ast::AnyEntry::Remove(e) => {
            let target = e.target()?;
            let op = if e.if_set() { "-?" } else { "-" };
            (
                format!("{prefix}{op}{}", target_text(&target)),
                None,
                SymbolKind::FIELD,
                target.syntax().text_range(),
                Vec::new(),
            )
        }
        ast::AnyEntry::ValueBind(bind) => {
            let name = bind.name()?;
            (
                format!("{} :=", name.text()),
                None,
                SymbolKind::CONSTANT,
                name.text_range(),
                Vec::new(),
            )
        }
        ast::AnyEntry::VariantBlock(block) => {
            let variant = block.variant()?;
            let condition: String = variant.syntax().text().to_string();
            let inner = format!("{prefix}{condition} › ");
            let children = block
                .entries()
                .filter_map(|e| entry_symbol(&e, &inner, text, index, encoding))
                .collect();
            (
                condition,
                None,
                SymbolKind::NAMESPACE,
                variant.syntax().text_range(),
                children,
            )
        }
        ast::AnyEntry::Spread(_) | ast::AnyEntry::Defaults(_) => return None,
    };
    Some(DocumentSymbol {
        name,
        detail,
        kind,
        tags: None,
        deprecated: None,
        range,
        selection_range: convert::range(text, index, selection, encoding),
        children: (!children.is_empty()).then_some(children),
    })
}

fn block_children(
    block: &ast::Block,
    text: &str,
    index: &LineIndex,
    encoding: Encoding,
) -> Vec<DocumentSymbol> {
    block
        .entries()
        .filter_map(|e| entry_symbol(&e, "", text, index, encoding))
        .collect()
}

// ---- Formatting ----

/// The whole document, formatted (`SPEC.md` 12.2); nothing for a document
/// with syntax errors.
pub(crate) fn format(text: &str, encoding: Encoding) -> Option<Vec<TextEdit>> {
    let formatted = crol_syntax::format::format(text, &Default::default()).ok()?;
    if formatted == text {
        return Some(Vec::new());
    }
    let index = LineIndex::new(text);
    let whole = TextRange::up_to(crol_syntax::TextSize::of(text));
    Some(vec![TextEdit::new(
        convert::range(text, &index, whole, encoding),
        formatted,
    )])
}

// ---- Paths at the cursor ----

pub(crate) fn token_at(
    text: &str,
    position: Position,
    encoding: Encoding,
) -> Option<(SyntaxNode, SyntaxToken)> {
    let parse = crol_syntax::parse(text);
    let root = parse.syntax();
    let offset = convert::offset(text, position, encoding);
    // Between two tokens, the one to the right: the name in `@web` rather
    // than the `@` before it.
    let mut token = root.token_at_offset(offset).right_biased()?;
    if token.kind().is_trivia() {
        token = root.token_at_offset(offset).left_biased()?;
    }
    if token.kind() == SyntaxKind::AT {
        token = token.next_token()?;
    }
    Some((root, token))
}

/// The steps a target's selectors take, ignoring conditions.
pub(crate) fn selector_steps(selectors: impl Iterator<Item = ast::Selector>) -> Vec<PathStep> {
    selectors
        .filter_map(|s| match s {
            ast::Selector::Field(f) => Some(PathStep::Field(f.name()?.text().to_owned())),
            ast::Selector::Index(i) => i.index()?.text().parse().ok().map(PathStep::Element),
            ast::Selector::Accessor(a) => {
                let token = a.key()?;
                match token.kind() {
                    SyntaxKind::INT => token
                        .text()
                        .parse::<i128>()
                        .ok()
                        .map(|i| PathStep::Accessor(if a.is_negative() { -i } else { i })),
                    _ => Some(PathStep::Field(token.text().trim_matches('"').to_owned())),
                }
            }
            _ => None,
        })
        .collect()
}

/// The text a key or string token stands for.
pub(crate) fn string_text(text: &str) -> String {
    match text.strip_prefix('r').unwrap_or(text).trim_matches('#') {
        quoted if quoted.starts_with('"') => quoted.trim_matches('"').to_owned(),
        _ => text.to_owned(),
    }
}

pub(crate) fn key_text(key: &ast::Key) -> Option<String> {
    let token = key
        .accessor()
        .map_or_else(|| key.token(), |accessor| accessor.key())?;
    Some(string_text(token.text()))
}

/// The steps of a target: its key and selectors.
fn target_steps(target: &ast::Target) -> Option<Vec<PathStep>> {
    let mut steps = key_steps(target)?;
    steps.extend(selector_steps(target.selectors()));
    Some(steps)
}

/// The step of a target's key. An integer key is a key of that integer, not
/// of the text.
fn key_steps(target: &ast::Target) -> Option<Vec<PathStep>> {
    let key = target.key()?;
    let steps = match key.accessor() {
        Some(accessor) => selector_steps(std::iter::once(ast::Selector::Accessor(accessor))),
        None => {
            let token = key.token()?;
            let integer = (token.kind() == SyntaxKind::INT)
                .then(|| token.text().parse().ok())
                .flatten();
            vec![integer.map_or_else(
                || PathStep::Field(string_text(token.text())),
                PathStep::Accessor,
            )]
        }
    };
    Some(steps)
}

/// The path of the place `node` is at (`SPEC.md` 6.1): through the targets
/// of the entries around it, list elements at `[i]`, and tuple and
/// named-tuple elements at `.i`. None for what has no such place, as a
/// value bind's expression, or a list that is an operand.
fn place_path(node: &SyntaxNode) -> Option<Vec<PathStep>> {
    let position = |parent: &SyntaxNode, node: &SyntaxNode| {
        parent
            .children()
            .filter_map(ast::Expr::cast)
            .position(|e| e.syntax() == node)
    };
    // Innermost first.
    let mut parts: Vec<Vec<PathStep>> = Vec::new();
    let mut node = node.clone();
    while let Some(parent) = node.parent() {
        match parent.kind() {
            SyntaxKind::DOCUMENT => break,
            SyntaxKind::BLOCK
            | SyntaxKind::VARIANT_BLOCK
            | SyntaxKind::BINARY_EXPR
            | SyntaxKind::PAREN_EXPR
            | SyntaxKind::NAMED_TUPLE
            | SyntaxKind::MACRO_CALL
            | SyntaxKind::SPREAD => {}
            SyntaxKind::ENTRY => {
                parts.push(target_steps(&ast::Entry::cast(parent.clone())?.target()?)?);
            }
            SyntaxKind::LIST_EXPR => {
                let operand = parent
                    .parent()
                    .is_some_and(|p| p.kind() == SyntaxKind::BINARY_EXPR);
                if operand {
                    return None;
                }
                let index = position(&parent, &node)?;
                parts.push(vec![PathStep::Accessor(index as i128)]);
            }
            SyntaxKind::ARG_LIST | SyntaxKind::TUPLE_EXPR => {
                parts.push(vec![PathStep::Element(position(&parent, &node)?)]);
            }
            _ => return None,
        }
        node = parent;
    }
    Some(parts.into_iter().rev().flatten().collect())
}

/// A path from the root as a root key and the path in its value.
fn from_root(mut path: Vec<PathStep>) -> Option<(String, Vec<PathStep>)> {
    if path.is_empty() {
        return None;
    }
    let key = match path.remove(0) {
        PathStep::Field(key) => key,
        PathStep::Accessor(key) => key.to_string(),
        PathStep::Element(_) => return None,
    };
    Some((key, path))
}

/// The root key and path of the target at a token: `web.limits.cpu` for
/// `cpu` in `+web: Service { limits: { cpu: 2.0 } }`.
pub(crate) fn target_path(token: &SyntaxToken) -> Option<(String, Vec<PathStep>)> {
    let target = token.parent_ancestors().find_map(ast::Target::cast)?;
    let mut path = place_path(&target.syntax().parent()?)?;
    path.extend(target_steps(&target)?);
    from_root(path)
}

/// [`target_path`] up to the segment of the target a token is on, with where
/// that segment ends: `web.limits` on `limits` in `web.limits.cpu: 2`.
fn segment_path(token: &SyntaxToken) -> Option<(String, Vec<PathStep>, TextSize)> {
    let target = token.parent_ancestors().find_map(ast::Target::cast)?;
    let mut path = place_path(&target.syntax().parent()?)?;
    path.extend(key_steps(&target)?);
    let mut end = target.key()?.syntax().text_range().end();
    let reached: Vec<ast::Selector> = target
        .selectors()
        .take_while(|s| s.syntax().text_range().start() <= token.text_range().start())
        .collect();
    if let Some(last) = reached.last() {
        end = last.syntax().text_range().end();
    }
    path.extend(selector_steps(reached.into_iter()));
    let (key, steps) = from_root(path)?;
    Some((key, steps, end))
}

/// For a segment of a target in the value of a `#[type(...)]` bind, the end
/// of the target up to it (`SPEC.md` 6.1.2).
fn typed_segment_end(token: &SyntaxToken) -> Option<TextSize> {
    let target = token.parent_ancestors().find_map(ast::Target::cast)?;
    let bind = target.syntax().ancestors().find_map(ast::ValueBind::cast)?;
    bind.attr()?;
    let mut end = target.key()?.syntax().text_range().end();
    if let Some(last) = target
        .selectors()
        .take_while(|s| s.syntax().text_range().start() <= token.text_range().start())
        .last()
    {
        end = last.syntax().text_range().end();
    }
    Some(end)
}

/// The root key and path of an element of a list, tuple or `Name(...)`.
pub(crate) fn element_path(node: &SyntaxNode) -> Option<(String, Vec<PathStep>)> {
    from_root(place_path(node)?)
}

/// The root key a token names: the key of a root entry's target, an id
/// or a `&root` reference.
pub(crate) fn root_key_at(token: &SyntaxToken) -> Option<String> {
    if token
        .parent()
        .is_some_and(|p| p.kind() == SyntaxKind::ID_EXPR)
    {
        return Some(token.text().trim_matches('"').to_owned());
    }
    if let Some((key, _)) = reference_path(token) {
        return Some(key);
    }
    // The key of a target, at the top level (or in an untyped outer
    // block) or in a top-level variant block.
    let key = token.parent().and_then(ast::Key::cast)?;
    let target = key.syntax().parent().and_then(ast::Target::cast)?;
    let mut node = target.syntax().parent()?;
    while let Some(parent) = node.parent() {
        match parent.kind() {
            SyntaxKind::DOCUMENT => return key_text(&key),
            SyntaxKind::VARIANT_BLOCK => node = parent,
            SyntaxKind::BLOCK
                if parent
                    .parent()
                    .is_some_and(|p| p.kind() == SyntaxKind::DOCUMENT)
                    && ast::Block::cast(parent.clone())?.type_name().is_none() =>
            {
                return key_text(&key);
            }
            _ => return None,
        }
    }
    None
}

/// The root key and path of the place a block is at.
pub(crate) fn block_path(block: &ast::Block) -> Option<(String, Vec<PathStep>)> {
    from_root(place_path(block.syntax())?)
}

/// The root key and path that `&self`, `&parent` or an entry bind at a
/// token names, with the selectors after it: `&self.port` in
/// `+web: { url: &self.port }` names `web.port`.
fn relative_reference_path(token: &SyntaxToken) -> Option<(String, Vec<PathStep>)> {
    let reference = token.parent_ancestors().find_map(|n| {
        if let Some(path) = ast::PathExpr::cast(n.clone())
            && let Some(ast::Expr::Reference(r)) = path.base()
        {
            return Some(r);
        }
        ast::Reference::cast(n)
    })?;
    let blocks = || reference.syntax().ancestors().filter_map(ast::Block::cast);
    let (key, mut path) = match reference.kind() {
        ast::ReferenceKind::SelfValue => block_path(&blocks().next()?)?,
        ast::ReferenceKind::Parent(n) => block_path(&blocks().nth(n)?)?,
        ast::ReferenceKind::Bind(name) => {
            let declared = crate::navigate::local_declaration(&name)?;
            let entry = declared.parent()?.parent().and_then(ast::Entry::cast)?;
            target_path(&entry.target()?.key()?.token()?)?
        }
        _ => return None,
    };
    if let Some(expr) = reference.syntax().parent().and_then(ast::PathExpr::cast) {
        path.extend(selector_steps(expr.selectors()));
    }
    Some((key, path))
}

/// The root key and path a reference names: `&root.db.host`.
fn reference_path(token: &SyntaxToken) -> Option<(String, Vec<PathStep>)> {
    let path = token.parent_ancestors().find_map(ast::PathExpr::cast)?;
    let ast::Expr::Reference(reference) = path.base()? else {
        return None;
    };
    if reference.kind() != ast::ReferenceKind::Root {
        return None;
    }
    let mut steps = selector_steps(path.selectors());
    if steps.is_empty() {
        return None;
    }
    let PathStep::Field(key) = steps.remove(0) else {
        return None;
    };
    Some((key, steps))
}

pub(crate) fn describe_path(key: &str, path: &[PathStep]) -> String {
    let mut out = Key::Str(key.into()).to_string();
    for step in path {
        match step {
            PathStep::Field(name) if crol_eval::is_identifier(name) => {
                out.push_str(&format!(".{name}"))
            }
            PathStep::Field(name) => out.push_str(&format!("[{}]", crol_eval::quote(name))),
            PathStep::Element(i) => out.push_str(&format!(".{i}")),
            PathStep::Accessor(i) => out.push_str(&format!("[{i}]")),
        }
    }
    out
}

/// The value at `key` + `path` in the evaluation `file` belongs to, in the
/// context `context`.
pub(crate) fn value_at(
    snapshot: &Snapshot,
    file: crol_eval::FileId,
    key: &str,
    path: &[PathStep],
    context: &[String],
) -> Option<Value> {
    value_in(snapshot.evaluation_of(file)?, key, path, context)
}

/// The evaluation a document belongs to, else the project's; while that
/// has no value, the latest of the same that had one.
pub(crate) fn evaluation_for(
    snapshot: &Snapshot,
    file: Option<crol_eval::FileId>,
) -> &crol_eval::Evaluation {
    let evaluation = file
        .and_then(|f| snapshot.evaluation_of(f))
        .unwrap_or(&snapshot.evaluation);
    match &evaluation.value {
        None => snapshot.last_good_of(file).unwrap_or(evaluation),
        Some(_) => evaluation,
    }
}

/// The value at `key` + `path` in an evaluation, in the context `context`.
pub(crate) fn value_in(
    evaluation: &crol_eval::Evaluation,
    key: &str,
    path: &[PathStep],
    context: &[String],
) -> Option<Value> {
    let base = evaluation.value.as_ref()?;
    let ValueKind::Keyed(keyed) = &base.kind else {
        return None;
    };
    let key = Key::Str(key.into());
    let data = if context.is_empty() {
        None
    } else {
        let ids: Option<Vec<_>> = context.iter().map(|n| evaluation.variants.id(n)).collect();
        let wanted = evaluation.variants.context(&ids?).ok()?;
        evaluation.contexts.iter().find(|d| d.context == wanted)
    };
    let value = match data {
        Some(data) => data.get(base, &key),
        None => keyed.entries.get(&key),
    };
    crol_eval::value_at_path(value?, path).cloned()
}

/// A value on one line, in code: nothing for what does not fit on one.
fn one_line(value: &Value) -> Option<String> {
    let text = print::to_text(value);
    (text.len() <= MAX_VALUE).then(|| code(&text))
}

/// What fits on a line of a hover.
const MAX_VALUE: usize = 80;

/// `text` as Markdown inline code, whatever backticks it holds.
fn code(text: &str) -> String {
    let longest = text
        .split(|c| c != '`')
        .map(str::len)
        .max()
        .unwrap_or_default();
    if longest == 0 {
        return format!("`{text}`");
    }
    let fence = "`".repeat(longest + 1);
    format!("{fence} {text} {fence}")
}

// ---- Hover ----

/// Whether a token can be a value, or the start of a part of one. The `:`
/// goes with these: it is in the entry, and past the key.
fn in_value(token: &SyntaxToken) -> bool {
    use SyntaxKind::*;
    // A value bind is no place in the data, unless `#[type(...)]` gives it
    // a type (`SPEC.md` 6.1.2).
    let outside = |n: &SyntaxNode| match n.kind() {
        ATTRIBUTE | ENTRY_ATTR | BIND | COND_NAME | VARIANT => true,
        VALUE_BIND => !n.children().any(|c| c.kind() == ENTRY_ATTR),
        _ => false,
    };
    matches!(
        token.kind(),
        IDENT
            | COLON
            | L_BRACE
            | L_BRACK
            | L_PAREN
            | INT
            | FLOAT
            | STRING
            | RAW_STRING
            | CHAR
            | TRUE_KW
            | FALSE_KW
            | AMP
            | SELF_KW
            | PARENT_KW
            | ROOT_KW
            | DEFAULT_KW
    ) && !token.parent_ancestors().any(|n| outside(&n))
}

/// The path of a place, as a root key and the path in its value.
fn split_path(path: &[PathStep]) -> Option<(String, Vec<PathStep>)> {
    from_root(path.to_vec())
}

/// The doc, then the type, of what is at the cursor, with one line for what
/// a reference or an id names, and the operations of a key in data that more
/// than one has touched (`SPEC.md` 12.5). No hover where none of it is
/// known.
pub(crate) fn hover(
    snapshot: &Snapshot,
    uri: &Uri,
    text: &str,
    position: Position,
    encoding: Encoding,
    settings: &Settings,
) -> Option<Hover> {
    let (_, token) = token_at(text, position, encoding)?;
    if let Some(hover) = macro_hover(&token, text, encoding) {
        return Some(hover);
    }
    let path = convert::uri_to_path(uri)?;
    let file = snapshot.file_of(&path)?;
    if let Some(type_ref) = token.parent_ancestors().find_map(ast::TypeRef::cast) {
        return type_hover(snapshot, file, &type_ref, &token, text, encoding);
    }
    let scope = Scope::new(snapshot, Some(file));
    let schema = snapshot.schema_for(Some(file));
    let target = token.parent_ancestors().find_map(ast::Target::cast);
    // The place, what the schema says of it, and the path of the entry.
    let mut about = None;
    let mut entry_path = None;
    if target.is_some() {
        if let Some((key, steps, end)) = segment_path(&token) {
            let cursor = place::locate(&scope, text, end);
            about = schema
                .as_ref()
                .zip(cursor.current())
                .and_then(|(schema, place)| crate::schema::about(schema, place));
            entry_path = Some((key, steps));
        } else if let Some(end) = typed_segment_end(&token) {
            // In the value of a `#[type(...)]` bind, which has no path.
            let cursor = place::locate(&scope, text, end);
            about = schema
                .as_ref()
                .zip(cursor.current())
                .and_then(|(schema, place)| crate::schema::about(schema, place));
        }
    } else if in_value(&token) {
        let cursor = place::locate(&scope, text, token.text_range().start());
        let place = match token.kind() {
            SyntaxKind::COLON => cursor.current(),
            _ => cursor.value_or_root(),
        };
        if let Some(place) = place {
            let name = (token.kind() == SyntaxKind::IDENT)
                .then(|| token.text())
                .filter(|_| !is_reference(&token));
            about = schema.as_ref().and_then(|schema| {
                name.and_then(|name| crate::schema::about_name(schema, place, name))
                    .or_else(|| crate::schema::about(schema, place))
            });
            entry_path = place.path.as_deref().and_then(split_path);
        }
    }
    let mut parts = Vec::new();
    let documented = named_path(&token)
        .or_else(|| entry_path.clone())
        .map(|(key, steps)| describe_path(&key, &steps));
    parts.extend(documented.and_then(|path| crate::docs::of_path(snapshot, file, &path, settings)));
    if let Some(about) = about {
        if !about.doc.trim().is_empty() {
            parts.push(about.doc.trim().to_owned());
        }
        parts.extend(about.ty.map(|ty| code(&ty)));
    }
    parts.extend(named_value(snapshot, file, &token, settings));
    let mut markdown = parts.join("\n\n");
    if let Some((key, steps)) = entry_path
        && let Some(history) = history(snapshot, file, &describe_path(&key, &steps), settings)
    {
        if !markdown.is_empty() {
            markdown.push_str("\n\n---\n\n");
        }
        markdown.push_str(&history);
    }
    if markdown.is_empty() {
        return None;
    }
    let index = LineIndex::new(text);
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: markdown,
        }),
        range: Some(convert::range(text, &index, token.text_range(), encoding)),
    })
}

/// In a `#[type(...)]`: the type at the segment or step under the cursor,
/// and its doc (`SPEC.md` 12.5).
fn type_hover(
    snapshot: &Snapshot,
    file: crol_eval::FileId,
    type_ref: &ast::TypeRef,
    token: &SyntaxToken,
    text: &str,
    encoding: Encoding,
) -> Option<Hover> {
    use crol::typed::TypeAt;
    let schema = snapshot.schema_for(Some(file))?.file;
    let step = token.parent_ancestors().find_map(ast::TypeStep::cast);
    let (steps, range) = match &step {
        Some(step) => {
            let index = type_ref.steps().position(|s| s == *step)?;
            (index + 1, step.syntax().text_range())
        }
        None => (0, type_ref.ty()?.syntax().text_range()),
    };
    let document = crol_syntax::parse(text).document();
    let uses = crol::typed::TypeUses::of(&document, schema);
    let at = crol::typed::resolve_steps(schema, &uses, type_ref, steps).ok()?;
    let (doc, ty) = match &at {
        TypeAt::Type { ty, doc } => {
            let own = crate::schema::named_def(schema, ty).map(|d| d.doc().trim().to_owned());
            let doc = doc.clone().or(own).unwrap_or_default();
            (doc, crate::schema::rust_type(schema, ty))
        }
        TypeAt::Variant { path, name } => {
            let doc = match schema.get(path) {
                Some(crol::schema::TypeDef::Enum { variants, .. }) => {
                    variants.get(name).map(|v| v.doc.trim().to_owned())
                }
                _ => None,
            };
            let own = schema.get(path).map_or(path.as_str(), |d| d.name());
            (doc.unwrap_or_default(), format!("{own}::{name}"))
        }
    };
    let mut parts = Vec::new();
    if !doc.is_empty() {
        parts.push(doc);
    }
    parts.push(code(&ty));
    let index = LineIndex::new(text);
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: parts.join("\n\n"),
        }),
        range: Some(convert::range(text, &index, range, encoding)),
    })
}

/// On the name of a built-in macro or its `!`: how a call is written, and
/// what it does. The arguments are hovered as any value is.
fn macro_hover(token: &SyntaxToken, text: &str, encoding: Encoding) -> Option<Hover> {
    if !matches!(token.kind(), SyntaxKind::IDENT | SyntaxKind::BANG) {
        return None;
    }
    let call = token.parent().and_then(ast::MacroCall::cast)?;
    let name = call.name()?;
    let help = crol_eval::macro_help(name.text())?;
    let index = LineIndex::new(text);
    let range = name.text_range().cover(token.text_range());
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: format!("```crol\n{}\n```\n\n{}", help.signature, help.doc),
        }),
        range: Some(convert::range(text, &index, range, encoding)),
    })
}

/// Whether a token is a part of a reference or an id, rather than a value
/// of its own.
fn is_reference(token: &SyntaxToken) -> bool {
    token.parent_ancestors().any(|n| {
        matches!(
            n.kind(),
            SyntaxKind::REFERENCE | SyntaxKind::PATH_EXPR | SyntaxKind::ID_EXPR
        )
    })
}

/// On a reference or an id: the root key and path it names.
fn named_path(token: &SyntaxToken) -> Option<(String, Vec<PathStep>)> {
    if token
        .parent()
        .is_some_and(|p| p.kind() == SyntaxKind::ID_EXPR)
    {
        let id = token
            .text()
            .trim_start_matches('@')
            .trim_matches('"')
            .to_owned();
        return Some((id, Vec::new()));
    }
    reference_path(token).or_else(|| relative_reference_path(token))
}

/// On a reference or an id, what it names: its path and value on one line,
/// which is not to be read off the document as a written value is.
fn named_value(
    snapshot: &Snapshot,
    file: crol_eval::FileId,
    token: &SyntaxToken,
    settings: &Settings,
) -> Option<String> {
    let (key, steps) = named_path(token)?;
    let evaluation = evaluation_for(snapshot, Some(file));
    let stale = snapshot
        .last_good_of(Some(file))
        .is_some_and(|good| std::ptr::eq(good, evaluation));
    let value = value_in(evaluation, &key, &steps, &settings.context)?;
    let shown = one_line(&value)?;
    let described = describe_path(&key, &steps);
    let in_context = if settings.context.is_empty() {
        String::new()
    } else {
        format!(" in {{{}}}", settings.context.join(", "))
    };
    let mut line = format!("**{described}**{in_context}: {shown}");
    if stale {
        line.push_str(" (at the last evaluation that had a value)");
    }
    Some(line)
}

/// The operations on `path` in the layers of a data document, one to a line,
/// if there is more than one: what one operation does is there to read.
fn history(
    snapshot: &Snapshot,
    file: crol_eval::FileId,
    path: &str,
    settings: &Settings,
) -> Option<String> {
    let (layers, options) = snapshot.chain_of(file)?;
    let steps = snapshot.explain(layers, options, path, &settings.context);
    let steps = steps.ok().filter(|steps| steps.len() > 1)?;
    let lines: Vec<String> = steps
        .into_iter()
        .map(|step| {
            let location = snapshot.sources.location(step.span);
            let kind = match step.kind {
                StepKind::Definition => "definition",
                StepKind::Patch => "patch",
                StepKind::Remove => "remove",
            };
            let value = step.value.as_ref().map_or_else(
                || "(missing)".to_owned(),
                |value| one_line(value).unwrap_or_else(|| "(a long value)".to_owned()),
            );
            format!(
                "- {} ({kind}, {}:{}, layer `{}`): {value}",
                code(&step.entry),
                file_name(location.name),
                location.line_col.line,
                step.layer
            )
        })
        .collect();
    Some(lines.join("\n"))
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

// ---- Go to definition ----

/// On a root key, an id or a reference into the root: the entry that
/// defines the key; on a template: its definition.
pub(crate) fn definition(
    snapshot: &Snapshot,
    uri: &Uri,
    text: &str,
    position: Position,
    encoding: Encoding,
) -> Option<Location> {
    let (_, token) = token_at(text, position, encoding)?;
    if let Some(location) = value_below(snapshot, uri, &token, encoding) {
        return Some(location);
    }
    // A bind: its declaration in scope, or the template.
    if let Some(name) = crate::navigate::bind_name(&token) {
        return match crate::navigate::local_declaration(&name) {
            Some(declared) => Some(Location::new(
                uri.clone(),
                convert::range(text, &LineIndex::new(text), declared.text_range(), encoding),
            )),
            None => template_definition(snapshot, name.text(), encoding),
        };
    }
    let key = if token
        .parent()
        .is_some_and(|p| p.kind() == SyntaxKind::ID_EXPR)
    {
        token
            .text()
            .trim_start_matches('@')
            .trim_matches('"')
            .to_owned()
    } else if let Some((key, _)) = reference_path(&token) {
        key
    } else {
        target_path(&token)?.0
    };
    // The first definition of the key, in the base chain.
    let file = convert::uri_to_path(uri).and_then(|path| snapshot.file_of(&path));
    let (layers, options) = file
        .and_then(|file| snapshot.chain_of(file))
        .unwrap_or((&snapshot.layers, &snapshot.options));
    let steps = snapshot
        .explain(layers, options, &Key::Str(key.into()).to_string(), &[])
        .ok()?;
    let step = steps
        .iter()
        .find(|s| s.kind == StepKind::Definition)
        .or_else(|| steps.first())?;
    location(snapshot, step.span, encoding)
}

/// "Go to value below" (`SPEC.md` 12.5): on `&`, the operation that
/// produced the value below, in the base context: the last one before the
/// operation the `&` is in that changed the value at its path.
fn value_below(
    snapshot: &Snapshot,
    uri: &Uri,
    token: &SyntaxToken,
    encoding: Encoding,
) -> Option<Location> {
    let reference = token
        .parent_ancestors()
        .find_map(ast::Reference::cast)
        .filter(|r| r.kind() == ast::ReferenceKind::Below)?;
    let file = snapshot.file_of(&convert::uri_to_path(uri)?)?;
    let entry = reference.syntax().ancestors().find_map(ast::Entry::cast)?;
    let (key, mut path) = target_path(&entry.target()?.key()?.token()?)?;
    // `&.port`: the field `port` of the value below.
    if let Some(selected) = reference.syntax().parent().and_then(ast::PathExpr::cast) {
        path.extend(selector_steps(selected.selectors()));
    }
    let (layers, options) = snapshot.chain_of(file)?;
    let steps = snapshot
        .explain(layers, options, &describe_path(&key, &path), &[])
        .ok()?;
    // The operation the `&` is in: the innermost entry around it whose
    // target an operation in this file starts at.
    let at = reference
        .syntax()
        .ancestors()
        .filter_map(ast::Entry::cast)
        .find_map(|e| {
            let start = e.target()?.syntax().text_range().start();
            steps
                .iter()
                .position(|s| s.span.file == file && s.span.range.start() == start)
        })?;
    let below = &steps[..at];
    let producer = (0..below.len()).rev().find(|&i| {
        i == 0
            || match (&below[i].value, &below[i - 1].value) {
                (Some(a), Some(b)) => !a.identical(b),
                (a, b) => a.is_some() != b.is_some(),
            }
    })?;
    location(snapshot, below[producer].span, encoding)
}

fn location(snapshot: &Snapshot, span: crol_eval::Span, encoding: Encoding) -> Option<Location> {
    let file = snapshot.sources.file(span.file);
    let index = LineIndex::new(file.text());
    Some(Location::new(
        convert::path_to_uri(snapshot.path(span.file)?)?,
        convert::range(file.text(), &index, span.range, encoding),
    ))
}

fn template_definition(snapshot: &Snapshot, name: &str, encoding: Encoding) -> Option<Location> {
    for (id, file) in snapshot.sources.files() {
        let parse = crol_syntax::parse(file.text());
        for entry in parse.document().entries() {
            if let ast::AnyEntry::ValueBind(bind) = entry
                && let Some(token) = bind.name()
                && token.text() == name
            {
                return location(
                    snapshot,
                    crol_eval::Span::new(id, token.text_range()),
                    encoding,
                );
            }
        }
    }
    None
}
