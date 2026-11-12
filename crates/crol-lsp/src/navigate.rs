//! Binds, find references, rename and workspace symbols (`SPEC.md` 12.5).

use std::collections::HashMap;

use crol_syntax::ast::{self, AstNode};
use crol_syntax::{LineIndex, SyntaxKind, SyntaxNode, SyntaxToken, TextRange};
use lsp_types::{
    Location, Position, Range, SymbolInformation, SymbolKind, TextEdit, Uri, WorkspaceEdit,
};

use crate::convert::{self, Encoding};
use crate::workspace::Snapshot;

// ---- Binds ----

/// Where a bind name is declared.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Declaration {
    /// In a block: visible in that block and every block nested in it
    /// (`SPEC.md` 6.1.1). `scope` is the block, `name` the declared name.
    Local {
        scope: SyntaxNode,
        name: SyntaxToken,
    },
    /// A value bind at the top level of a data-mode document, visible in
    /// every document (`SPEC.md` 6.1.2), or a name no block declares.
    Template,
}

/// The bind name at a token: its declaration (`port =`, `base :=`) or a
/// use (`&port`).
pub(crate) fn bind_name(token: &SyntaxToken) -> Option<SyntaxToken> {
    if token.kind() != SyntaxKind::IDENT {
        return None;
    }
    let parent = token.parent()?;
    match parent.kind() {
        SyntaxKind::BIND | SyntaxKind::VALUE_BIND => Some(token.clone()),
        SyntaxKind::REFERENCE => match ast::Reference::cast(parent)?.kind() {
            ast::ReferenceKind::Bind(name) if name == *token => Some(name),
            _ => None,
        },
        _ => None,
    }
}

/// The name a block or document declares directly, as an entry bind or a
/// value bind.
pub(crate) fn declared_in(container: &SyntaxNode, name: &str) -> Option<SyntaxToken> {
    container.children().find_map(|entry| {
        let token = match entry.kind() {
            SyntaxKind::VALUE_BIND => ast::ValueBind::cast(entry)?.name(),
            _ => entry
                .children()
                .find_map(ast::Bind::cast)
                .and_then(|b| b.name()),
        }?;
        (token.text() == name).then_some(token)
    })
}

/// Where the bind name at `token` is declared. There is no shadowing, so
/// the nearest block that declares the name is the one.
fn declaration(token: &SyntaxToken) -> Declaration {
    let name = token.text();
    for container in token.parent_ancestors() {
        if !matches!(container.kind(), SyntaxKind::BLOCK | SyntaxKind::DOCUMENT) {
            continue;
        }
        if let Some(declared) = declared_in(&container, name) {
            let template = container.kind() == SyntaxKind::DOCUMENT
                && declared.parent().map(|p| p.kind()) == Some(SyntaxKind::VALUE_BIND);
            return if template {
                Declaration::Template
            } else {
                Declaration::Local {
                    scope: container,
                    name: declared,
                }
            };
        }
    }
    Declaration::Template
}

/// Where the bind at a name token (a use or the declaration) is declared:
/// the declared name in this document, or `None` for a template.
pub(crate) fn local_declaration(token: &SyntaxToken) -> Option<SyntaxToken> {
    match declaration(token) {
        Declaration::Local { name, .. } => Some(name),
        Declaration::Template => None,
    }
}

/// The declaration and uses of the bind `name` under `node` that resolve
/// to `target`, each with whether it is the declaration.
fn occurrences(node: &SyntaxNode, name: &str, target: &Declaration) -> Vec<(TextRange, bool)> {
    node.descendants_with_tokens()
        .filter_map(|it| it.into_token())
        .filter(|t| t.text() == name)
        .filter_map(|t| bind_name(&t))
        .filter(|t| declaration(t) == *target)
        .map(|t| {
            let declares = t
                .parent()
                .is_some_and(|p| matches!(p.kind(), SyntaxKind::BIND | SyntaxKind::VALUE_BIND));
            (t.text_range(), declares)
        })
        .collect()
}

/// The documents to look through: the one being edited, with its buffer's
/// text, and every other file the snapshot read.
fn documents(snapshot: Option<&Snapshot>, uri: &Uri, text: &str) -> Vec<(Uri, String)> {
    let mut out = vec![(uri.clone(), text.to_owned())];
    if let Some(snapshot) = snapshot {
        for (id, file) in snapshot.sources.files() {
            if let Some(other) = snapshot.path(id).and_then(convert::path_to_uri)
                && other != *uri
            {
                out.push((other, file.text().to_owned()));
            }
        }
    }
    out
}

/// A document's URI and text, and occurrences of a bind in it, each with
/// whether it is the declaration.
type Occurrences = (Uri, String, Vec<(TextRange, bool)>);

/// Every occurrence of the bind at the cursor, by document; `None` if the
/// cursor is not on a bind.
fn bind_occurrences(
    snapshot: Option<&Snapshot>,
    uri: &Uri,
    text: &str,
    position: Position,
    encoding: Encoding,
) -> Option<Vec<Occurrences>> {
    let (_, token) = crate::features::token_at(text, position, encoding)?;
    let name = bind_name(&token)?;
    let target = declaration(&name);
    let found = match &target {
        Declaration::Local { scope, .. } => {
            vec![(
                uri.clone(),
                text.to_owned(),
                occurrences(scope, name.text(), &target),
            )]
        }
        Declaration::Template => documents(snapshot, uri, text)
            .into_iter()
            .map(|(uri, text)| {
                let parse = crol_syntax::parse(&text);
                let ranges = occurrences(&parse.syntax(), name.text(), &target);
                (uri, text, ranges)
            })
            .filter(|(_, _, ranges)| !ranges.is_empty())
            .collect(),
    };
    Some(found)
}

/// The range of what can be renamed at the cursor: a bind's name, a string
/// key of a target or of a reference's path, or a key of an id.
pub(crate) fn prepare_rename(text: &str, position: Position, encoding: Encoding) -> Option<Range> {
    let (_, token) = crate::features::token_at(text, position, encoding)?;
    let range = match bind_name(&token) {
        Some(name) => name.text_range(),
        None => crate::rename::prepare(&token)?,
    };
    Some(convert::range(text, &LineIndex::new(text), range, encoding))
}

/// Renames what is at the cursor (`SPEC.md` 12.5). A bind, with its
/// declaration and every use: within its block for an entry bind or a value
/// bind in a block, everywhere for a template. A key, or the key an id
/// names, with everything that names it: see [`crate::rename`].
pub(crate) fn rename(
    snapshot: Option<&Snapshot>,
    uri: &Uri,
    text: &str,
    position: Position,
    new_name: &str,
    current: &dyn Fn(&Uri) -> Option<String>,
    encoding: Encoding,
) -> Result<Option<WorkspaceEdit>, String> {
    let Some(found) = bind_occurrences(snapshot, uri, text, position, encoding) else {
        let Some((_, token)) = crate::features::token_at(text, position, encoding) else {
            return Ok(None);
        };
        return crate::rename::rename(snapshot, uri, text, &token, new_name, current, encoding);
    };
    if !crol_eval::is_identifier(new_name)
        || matches!(
            new_name,
            "self" | "parent" | "super" | "top" | "root" | "default"
        )
    {
        return Err(format!("`{new_name}` cannot be the name of a bind"));
    }
    // `WorkspaceEdit` has `Uri` keys.
    #[allow(clippy::mutable_key_type)]
    let mut changes = HashMap::new();
    for (uri, text, ranges) in found {
        let index = LineIndex::new(&text);
        let edits = ranges
            .into_iter()
            .map(|(r, _)| {
                TextEdit::new(
                    convert::range(&text, &index, r, encoding),
                    new_name.to_owned(),
                )
            })
            .collect();
        changes.insert(uri, edits);
    }
    Ok(Some(WorkspaceEdit::new(changes)))
}

/// The first name of the id written at `span`: `web` in `@web`, `a` in
/// `@::a.b`.
fn id_name_span(snapshot: &Snapshot, span: crol_eval::Span) -> crol_eval::Span {
    let text = snapshot.sources.file(span.file).text();
    let parse = crol_syntax::parse(text);
    let name = parse
        .syntax()
        .descendants()
        .find(|node| node.text_range() == span.range)
        .and_then(|node| {
            node.descendants_with_tokens()
                .filter_map(|it| it.into_token())
                .find(|t| {
                    t.parent().is_some_and(|p| p.kind() == SyntaxKind::ID_EXPR)
                        && matches!(
                            t.kind(),
                            SyntaxKind::IDENT | SyntaxKind::STRING | SyntaxKind::RAW_STRING
                        )
                })
        });
    match name {
        Some(name) => crol_eval::Span::new(span.file, name.text_range()),
        None => span,
    }
}

/// Where a span is, as a location.
fn span_location(
    snapshot: &Snapshot,
    span: crol_eval::Span,
    encoding: Encoding,
) -> Option<Location> {
    let file = snapshot.sources.file(span.file);
    let index = LineIndex::new(file.text());
    Some(Location::new(
        convert::path_to_uri(snapshot.path(span.file)?)?,
        convert::range(file.text(), &index, span.range, encoding),
    ))
}

// ---- Find references ----

/// For a bind, its declaration (if asked for) and uses; for a root key,
/// every operation on it in every layer, every reference to it and every
/// id naming it (`SPEC.md` 12.5).
pub(crate) fn references(
    snapshot: Option<&Snapshot>,
    uri: &Uri,
    text: &str,
    position: Position,
    include_declaration: bool,
    encoding: Encoding,
) -> Option<Vec<Location>> {
    if let Some(found) = bind_occurrences(snapshot, uri, text, position, encoding) {
        let mut out = Vec::new();
        for (file, text, ranges) in found {
            let index = LineIndex::new(&text);
            for (range, declares) in ranges {
                if include_declaration || !declares {
                    out.push(Location::new(
                        file.clone(),
                        convert::range(&text, &index, range, encoding),
                    ));
                }
            }
        }
        return Some(out);
    }
    let (_, token) = crate::features::token_at(text, position, encoding)?;
    // What the token names in the data, and the ids that name it there
    // (`SPEC.md` 10.5).
    let file = snapshot.and_then(|s| Some((s, s.file_of(&convert::uri_to_path(uri)?)?)));
    let places = file.map_or_else(Vec::new, |(snapshot, file)| {
        crate::features::places_named(snapshot, file, &token)
    });
    let mut out = Vec::new();
    if let Some((snapshot, file)) = file
        && !places.is_empty()
    {
        for id in snapshot.resolved_ids(file) {
            if !id
                .target
                .as_ref()
                .is_some_and(|target| places.contains(target))
            {
                continue;
            }
            // A key has the span of its entry's value: the key is beside it.
            let span = if id.is_key {
                crate::features::id_key_span(snapshot, id.span, &id.id)
            } else {
                Some(id_name_span(snapshot, id.span))
            };
            if let Some(location) = span.and_then(|span| span_location(snapshot, span, encoding))
                && !out.contains(&location)
            {
                out.push(location);
            }
        }
        // Inside a root value, the id's place has no operations of its own
        // to find: its entry is the declaration.
        if places.iter().all(|place| place.steps().len() > 1) {
            if include_declaration && let Some(root) = snapshot.data_of(file) {
                for place in &places {
                    if let Some(value) = place.value_in(root)
                        && let Some(location) = span_location(
                            snapshot,
                            crate::features::entry_key_span(snapshot, value.span),
                            encoding,
                        )
                    {
                        out.push(location);
                    }
                }
            }
            return Some(out);
        }
    }
    let key = crate::features::root_key_at(&token)?;
    // With the data, ids were found by what they resolve to.
    let ids_found = !places.is_empty();
    // A plain key is written as it is, so most files need no parsing.
    let plain = crol_eval::is_identifier(&key);
    for (file, text) in documents(snapshot, uri, text) {
        if plain && !text.contains(&key) {
            continue;
        }
        let index = LineIndex::new(&text);
        let parse = crol_syntax::parse(&text);
        for range in key_occurrences(&parse.syntax(), &key, include_declaration, !ids_found) {
            out.push(Location::new(
                file.clone(),
                convert::range(&text, &index, range, encoding),
            ));
        }
    }
    Some(out)
}

/// The root entries of a document: its top-level entries (or those of its
/// untyped outer block) and those in top-level variant blocks.
fn root_entries(document: &SyntaxNode) -> Vec<ast::AnyEntry> {
    fn collect(entries: impl Iterator<Item = ast::AnyEntry>, out: &mut Vec<ast::AnyEntry>) {
        for entry in entries {
            if let ast::AnyEntry::VariantBlock(block) = &entry {
                collect(block.entries(), out);
            } else {
                out.push(entry);
            }
        }
    }
    let mut out = Vec::new();
    if let Some(document) = ast::Document::cast(document.clone()) {
        match document.expr() {
            // An untyped outer block holds the root entries (`SPEC.md` 4).
            Some(ast::Expr::Block(block)) if block.type_name().is_none() => {
                collect(block.entries(), &mut out)
            }
            _ => collect(document.entries(), &mut out),
        }
    }
    out
}

fn root_target(entry: &ast::AnyEntry) -> Option<ast::Target> {
    match entry {
        ast::AnyEntry::Entry(e) => e.target(),
        ast::AnyEntry::Remove(e) => e.target(),
        _ => None,
    }
}

/// The ranges naming the root key `key` in a document: the keys of root
/// entries that operate on it (if `operations`), `&root.key` and `@key`.
fn key_occurrences(
    document: &SyntaxNode,
    key: &str,
    operations: bool,
    ids: bool,
) -> Vec<TextRange> {
    let mut out = Vec::new();
    if operations {
        for entry in root_entries(document) {
            if let Some(k) = root_target(&entry).and_then(|t| t.key())
                && crate::features::key_text(&k).as_deref() == Some(key)
            {
                out.extend(k.token().map(|t| t.text_range()));
            }
        }
    }
    for node in document.descendants() {
        match node.kind() {
            SyntaxKind::ID_EXPR if ids => {
                if let Some(token) = node
                    .children_with_tokens()
                    .filter_map(|it| it.into_token())
                    .find(|t| matches!(t.kind(), SyntaxKind::IDENT | SyntaxKind::STRING))
                    && token.text().trim_matches('"') == key
                {
                    out.push(token.text_range());
                }
            }
            SyntaxKind::PATH_EXPR => {
                let Some(path) = ast::PathExpr::cast(node) else {
                    continue;
                };
                if let Some(ast::Expr::Reference(reference)) = path.base()
                    && reference.kind() == ast::ReferenceKind::Root
                    && let Some(ast::Selector::Field(field)) = path.selectors().next()
                    && let Some(name) = field.name()
                    && name.text() == key
                {
                    out.push(name.text_range());
                }
            }
            _ => {}
        }
    }
    out.sort_by_key(|r| r.start());
    out
}

// ---- Workspace symbols ----

/// Every root key in every file, at its definitions (or its first
/// operation if none defines it), filtered by `query` (`SPEC.md` 12.5).
#[allow(deprecated)]
pub(crate) fn workspace_symbols(
    snapshot: &Snapshot,
    query: &str,
    encoding: Encoding,
) -> Vec<SymbolInformation> {
    let query = query.to_lowercase();
    // A file without the query holds no key with it, unless written with an
    // escape, and most files need no parsing.
    let plain = query.chars().all(|c| c.is_alphanumeric() || c == '_');
    // By key: definitions, and else the first operation.
    let mut found: Vec<(String, Vec<Location>, bool)> = Vec::new();
    for (id, file) in snapshot.sources.files() {
        let text = file.text();
        if plain && !query.is_empty() && !text.to_lowercase().contains(&query) {
            continue;
        }
        let Some(uri) = snapshot.path(id).and_then(convert::path_to_uri) else {
            continue;
        };
        let index = LineIndex::new(text);
        let parse = crol_syntax::parse(text);
        for entry in root_entries(&parse.syntax()) {
            let Some(k) = root_target(&entry).and_then(|t| t.key()) else {
                continue;
            };
            let (Some(key), Some(token)) = (crate::features::key_text(&k), k.token()) else {
                continue;
            };
            if !key.to_lowercase().contains(&query) {
                continue;
            }
            let defines =
                matches!(&entry, ast::AnyEntry::Entry(e) if e.op() == ast::EntryOp::Declare);
            let location = Location::new(
                uri.clone(),
                convert::range(text, &index, token.text_range(), encoding),
            );
            match found.iter_mut().find(|(k, _, _)| *k == key) {
                None => found.push((key, vec![location], defines)),
                Some((_, locations, had)) => {
                    if defines && !*had {
                        locations.clear();
                        *had = true;
                    }
                    if defines {
                        locations.push(location);
                    }
                }
            }
        }
    }
    found
        .into_iter()
        .flat_map(|(key, locations, _)| {
            locations
                .into_iter()
                .map(move |location| SymbolInformation {
                    name: key.clone(),
                    kind: SymbolKind::FIELD,
                    tags: None,
                    deprecated: None,
                    location,
                    container_name: None,
                })
        })
        .collect()
}
