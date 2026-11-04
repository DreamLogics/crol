//! The requests the server answers (`SPEC.md` 12.5).

use crol_eval::{Key, PathStep, StepKind, Value, ValueKind, print};
use crol_syntax::ast::{self, AstNode};
use crol_syntax::{LineIndex, SyntaxKind, SyntaxNode, SyntaxToken, TextRange};
use lsp_types::{
    DocumentSymbol, Hover, HoverContents, Location, MarkupContent, MarkupKind, Position,
    SymbolKind, TextEdit, Uri,
};

use crate::Settings;
use crate::convert::{self, Encoding};
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
            (
                format!("{prefix}-{}", target_text(&target)),
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

pub(crate) fn key_text(key: &ast::Key) -> Option<String> {
    let token = key.token()?;
    Some(match token.kind() {
        SyntaxKind::STRING | SyntaxKind::RAW_STRING => token
            .text()
            .trim_start_matches('r')
            .trim_matches('#')
            .trim_matches('"')
            .to_owned(),
        _ => token.text().to_owned(),
    })
}

/// The root key and path of the target at a token: `web.limits.cpu` for
/// `cpu` in `+web: Service { limits: { cpu: 2.0 } }`.
pub(crate) fn target_path(token: &SyntaxToken) -> Option<(String, Vec<PathStep>)> {
    let target = token.parent_ancestors().find_map(ast::Target::cast)?;
    let mut segments: Vec<PathStep> = Vec::new();
    let key = key_text(&target.key()?)?;
    let mut path = vec![PathStep::Field(key)];
    path.extend(selector_steps(target.selectors()));
    let mut node = target.syntax().parent()?; // the entry
    loop {
        let parent = node.parent()?;
        match parent.kind() {
            SyntaxKind::DOCUMENT => break,
            SyntaxKind::VARIANT_BLOCK => {
                node = parent;
                continue;
            }
            SyntaxKind::BLOCK => {
                // A block that is the value of an entry, or the right side
                // of `~` there.
                let mut up = parent.parent()?;
                while up.kind() == SyntaxKind::BINARY_EXPR || up.kind() == SyntaxKind::PAREN_EXPR {
                    up = up.parent()?;
                }
                if up.kind() == SyntaxKind::DOCUMENT {
                    break;
                }
                let entry = ast::Entry::cast(up.clone())?;
                let outer = entry.target()?;
                let mut outer_path = vec![PathStep::Field(key_text(&outer.key()?)?)];
                outer_path.extend(selector_steps(outer.selectors()));
                outer_path.append(&mut path);
                path = outer_path;
                node = up;
            }
            _ => return None,
        }
    }
    segments.append(&mut path);
    let PathStep::Field(key) = segments.remove(0) else {
        return None;
    };
    Some((key, segments))
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

/// The root key and path of the entry a block is the value of.
pub(crate) fn block_path(block: &ast::Block) -> Option<(String, Vec<PathStep>)> {
    let entry = block.syntax().ancestors().find_map(ast::Entry::cast)?;
    target_path(&entry.target()?.key()?.token()?)
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
    let mut out = Key::Str(key.to_owned()).to_string();
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

/// The value at `key` + `path` in an evaluation, in the context `context`.
pub(crate) fn value_in(
    evaluation: &crol_eval::Evaluation,
    key: &str,
    path: &[PathStep],
    context: &[String],
) -> Option<Value> {
    let base = evaluation.value.as_ref()?;
    let root = if context.is_empty() {
        base.clone()
    } else {
        let ids: Option<Vec<_>> = context.iter().map(|n| evaluation.variants.id(n)).collect();
        let wanted = evaluation.variants.context(&ids?).ok()?;
        evaluation
            .contexts
            .iter()
            .find(|d| d.context == wanted)
            .map_or_else(|| base.clone(), |d| d.root(base))
    };
    let ValueKind::Keyed(keyed) = &root.kind else {
        return None;
    };
    crol_eval::value_at_path(keyed.entries.get(&Key::Str(key.to_owned()))?, path).cloned()
}

fn show(value: &Value) -> String {
    let one_line = print::to_text(value);
    if one_line.len() <= 80 {
        format!("`{one_line}`")
    } else {
        format!(
            "```crol\n{}```",
            print::to_document(value, &Default::default())
        )
    }
}

// ---- Hover ----

/// On a target, a reference into the root, or an id: the value, and on a
/// target in the project, the operations behind it (`SPEC.md` 12.3, 12.5).
pub(crate) fn hover(
    snapshot: &Snapshot,
    uri: &Uri,
    text: &str,
    position: Position,
    encoding: Encoding,
    settings: &Settings,
) -> Option<Hover> {
    let path = convert::uri_to_path(uri)?;
    let file = snapshot.file_of(&path)?;
    let (_, token) = token_at(text, position, encoding)?;
    let (key, steps, is_target) = if token
        .parent()
        .is_some_and(|p| p.kind() == SyntaxKind::ID_EXPR)
    {
        let id = token
            .text()
            .trim_start_matches('@')
            .trim_matches('"')
            .to_owned();
        (id, Vec::new(), false)
    } else if let Some((key, steps)) = reference_path(&token) {
        (key, steps, false)
    } else if let Some((key, steps)) = relative_reference_path(&token) {
        (key, steps, false)
    } else {
        let (key, steps) = target_path(&token)?;
        (key, steps, true)
    };
    let described = describe_path(&key, &steps);
    let in_context = if settings.context.is_empty() {
        String::new()
    } else {
        format!(" in {{{}}}", settings.context.join(", "))
    };
    let mut markdown = match value_at(snapshot, file, &key, &steps, &settings.context) {
        Some(value) => format!("**{described}**{in_context}: {}", show(&value)),
        None => format!("**{described}**{in_context}: no value"),
    };
    // The type and doc from the schema.
    if is_target
        && let Some(entry) = token
            .parent_ancestors()
            .find_map(ast::Target::cast)
            .and_then(|t| t.syntax().parent())
            .and_then(ast::Entry::cast)
        && let Some((ty, doc)) = crate::schema::entry_type(snapshot, &entry)
        && let Some(schema) = &snapshot.schema
    {
        markdown.push_str(&format!("\n\n`{}`", crate::schema::rust_type(schema, &ty)));
        if !doc.is_empty() {
            markdown.push_str(&format!(" — {doc}"));
        }
    }
    if is_target && snapshot.in_layers(file) {
        let path_text = described.clone();
        if let Ok(history) = crol_eval::explain(
            &snapshot.sources,
            &snapshot.layers,
            &snapshot.options,
            &path_text,
            &settings.context,
        ) && !history.is_empty()
        {
            markdown.push_str("\n\n");
            for step in history {
                let location = snapshot.sources.location(step.span);
                let kind = match step.kind {
                    StepKind::Definition => "definition",
                    StepKind::Patch => "patch",
                    StepKind::Remove => "remove",
                };
                let value = step
                    .value
                    .as_ref()
                    .map_or_else(|| "(missing)".to_owned(), print::to_text);
                markdown.push_str(&format!(
                    "- `{}` ({kind}, {}:{}, layer `{}`): `{value}`\n",
                    step.entry,
                    file_name(location.name),
                    location.line_col.line,
                    step.layer
                ));
            }
        }
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
    let steps = crol_eval::explain(
        &snapshot.sources,
        &snapshot.layers,
        &snapshot.options,
        &Key::Str(key).to_string(),
        &[],
    )
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
    let steps = crol_eval::explain(
        &snapshot.sources,
        &snapshot.layers,
        &snapshot.options,
        &describe_path(&key, &path),
        &[],
    )
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
