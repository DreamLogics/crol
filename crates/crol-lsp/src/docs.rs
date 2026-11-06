//! Doc comments written in data (`SPEC.md` 2.2, 12.5).

use std::borrow::Cow;
use std::collections::HashMap;

use crol_eval::{FileId, Key, Layer, Span};
use crol_syntax::ast::{self, AstNode};
use crol_syntax::{SyntaxKind, SyntaxNode};

use crate::Settings;
use crate::workspace::Snapshot;

/// The doc comments of every operation on `path` in the chain of `file`,
/// lowest layer first.
pub(crate) fn of_path(
    snapshot: &Snapshot,
    file: FileId,
    path: &str,
    settings: &Settings,
) -> Option<String> {
    let (layers, options) = snapshot.chain_of(file)?;
    if !has_docs(snapshot, layers) {
        return None;
    }
    let steps = snapshot.explain(layers, options, path, &settings.context);
    let mut docs: Vec<String> = Vec::new();
    for step in steps.ok()? {
        if let Some(doc) = at_step(snapshot, step.span, path)
            && !docs.contains(&doc)
        {
            docs.push(doc);
        }
    }
    (!docs.is_empty()).then(|| docs.join("\n\n"))
}

/// Whether any document of the layers has a doc comment: most generated data
/// has none, and is then not parsed again.
fn has_docs(snapshot: &Snapshot, layers: &[Layer]) -> bool {
    let find = || {
        layers
            .iter()
            .flat_map(|layer| &layer.documents)
            .any(|(_, file)| snapshot.sources.file(*file).text().contains("///"))
    };
    if std::ptr::eq(layers, snapshot.layers.as_slice()) {
        snapshot.project_has_docs(find)
    } else {
        find()
    }
}

/// The doc comment of what an operation does at `path`.
fn at_step(snapshot: &Snapshot, span: Span, path: &str) -> Option<String> {
    let parse = crol_syntax::parse(snapshot.sources.file(span.file).text());
    let root = parse.syntax();
    let token = root.token_at_offset(span.range.start()).right_biased()?;
    let entry = token.parent_ancestors().find_map(ast::Entry::cast)?;
    let mut prefix = Vec::new();
    for outer in entry
        .syntax()
        .ancestors()
        .skip(1)
        .filter_map(ast::Entry::cast)
    {
        prefix.push(target_text(&outer)?);
    }
    prefix.reverse();
    find(&entry, &prefix.join("."), path)
}

/// The doc comment of `path` in an entry at `prefix`, or in an entry of the
/// blocks in its value.
fn find(entry: &ast::Entry, prefix: &str, path: &str) -> Option<String> {
    let target = target_text(entry)?;
    let full = if prefix.is_empty() {
        target
    } else {
        format!("{prefix}.{target}")
    };
    if full == path {
        return ast::doc_comment(entry.syntax());
    }
    if !path.starts_with(&format!("{full}.")) {
        return None;
    }
    blocks_of(entry.syntax())
        .iter()
        .flat_map(|block| block.children().filter_map(ast::Entry::cast))
        .find_map(|inner| find(&inner, &full, path))
}

/// The blocks of an entry's value, and not those of the entries in them.
fn blocks_of(entry: &SyntaxNode) -> Vec<SyntaxNode> {
    entry
        .descendants()
        .filter(|node| node.kind() == SyntaxKind::BLOCK)
        .filter(|node| {
            let nearest = node
                .ancestors()
                .skip(1)
                .find(|n| n.kind() == SyntaxKind::ENTRY);
            nearest.as_ref() == Some(entry)
        })
        .collect()
}

/// An entry's target as a path, without conditions.
fn target_text(entry: &ast::Entry) -> Option<String> {
    let text = entry.target()?.syntax().text().to_string();
    let mut out = String::with_capacity(text.len());
    let mut depth = 0;
    for c in text.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            _ if depth == 0 && !c.is_whitespace() => out.push(c),
            _ => {}
        }
    }
    Some(out)
}

/// The doc comments of root keys, by key as completion labels them, for
/// where looking each up in turn would take too long. Entries in variant
/// blocks are left out.
pub(crate) fn of_root_keys<'a>(
    snapshot: &'a Snapshot,
    file: Option<FileId>,
) -> Cow<'a, HashMap<String, String>> {
    let layers = file
        .and_then(|file| snapshot.chain_of(file))
        .map_or(snapshot.layers.as_slice(), |(layers, _)| layers);
    if std::ptr::eq(layers, snapshot.layers.as_slice()) {
        Cow::Borrowed(snapshot.project_root_docs(|| root_docs_in(snapshot, layers)))
    } else {
        Cow::Owned(root_docs_in(snapshot, layers))
    }
}

fn root_docs_in(snapshot: &Snapshot, layers: &[Layer]) -> HashMap<String, String> {
    let mut docs: HashMap<String, String> = HashMap::new();
    for (_, id) in layers.iter().flat_map(|layer| &layer.documents) {
        let text = snapshot.sources.file(*id).text();
        if !text.contains("///") {
            continue;
        }
        let parse = crol_syntax::parse(text);
        let Some(document) = ast::Document::cast(parse.syntax()) else {
            continue;
        };
        for entry in document.entries() {
            let ast::AnyEntry::Entry(entry) = entry else {
                continue;
            };
            let (Some(doc), Some(target)) = (ast::doc_comment(entry.syntax()), target_text(&entry))
            else {
                continue;
            };
            if target.contains('.') {
                continue;
            }
            let key = Key::Str(target.trim_matches('"').into()).to_string();
            docs.entry(key)
                .and_modify(|all| {
                    if !all.contains(&doc) {
                        all.push_str("\n\n");
                        all.push_str(&doc);
                    }
                })
                .or_insert(doc);
        }
    }
    docs
}
