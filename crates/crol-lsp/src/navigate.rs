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
fn bind_name(token: &SyntaxToken) -> Option<SyntaxToken> {
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
fn declared_in(container: &SyntaxNode, name: &str) -> Option<SyntaxToken> {
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
                && out.iter().all(|(u, _)| *u != other)
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

/// The range of the bind name at the cursor, if it can be renamed.
pub(crate) fn prepare_rename(text: &str, position: Position, encoding: Encoding) -> Option<Range> {
    let (_, token) = crate::features::token_at(text, position, encoding)?;
    let name = bind_name(&token)?;
    Some(convert::range(
        text,
        &LineIndex::new(text),
        name.text_range(),
        encoding,
    ))
}

/// Renames the bind at the cursor, its declaration and every use: within
/// its block for an entry bind or a value bind in a block, everywhere for
/// a template (`SPEC.md` 12.5).
pub(crate) fn rename(
    snapshot: Option<&Snapshot>,
    uri: &Uri,
    text: &str,
    position: Position,
    new_name: &str,
    encoding: Encoding,
) -> Result<Option<WorkspaceEdit>, String> {
    if !crol_eval::is_identifier(new_name)
        || matches!(new_name, "self" | "parent" | "root" | "default")
    {
        return Err(format!("`{new_name}` cannot be the name of a bind"));
    }
    let Some(found) = bind_occurrences(snapshot, uri, text, position, encoding) else {
        return Ok(None);
    };
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
    let key = crate::features::root_key_at(&token)?;
    let mut out = Vec::new();
    for (file, text) in documents(snapshot, uri, text) {
        let index = LineIndex::new(&text);
        let parse = crol_syntax::parse(&text);
        for range in key_occurrences(&parse.syntax(), &key, include_declaration) {
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
fn key_occurrences(document: &SyntaxNode, key: &str, operations: bool) -> Vec<TextRange> {
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
            SyntaxKind::ID_EXPR => {
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
    // By key: definitions, and else the first operation.
    let mut found: Vec<(String, Vec<Location>, bool)> = Vec::new();
    for (id, file) in snapshot.sources.files() {
        let Some(uri) = snapshot.path(id).and_then(convert::path_to_uri) else {
            continue;
        };
        let text = file.text();
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
