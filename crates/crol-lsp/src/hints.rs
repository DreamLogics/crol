//! Inlay hints: the result after an expression that computes something
//! (`SPEC.md` 12.5).

use crol_eval::{Span, Value, print};
use crol_syntax::ast::{self, AstNode};
use crol_syntax::{LineIndex, SyntaxKind, SyntaxNode};
use lsp_types::{InlayHint, InlayHintLabel, Range};

use crate::Settings;
use crate::convert::{self, Encoding};
use crate::features;
use crate::workspace::Snapshot;

/// The longest value a hint shows; longer ones end in `…`.
const MAX_HINT: usize = 40;

/// The hints in `range` of a document, in the context of the `context`
/// setting. None while the document has edits not evaluated yet, since the
/// values would land in the wrong places.
pub(crate) fn inlay_hints(
    snapshot: &Snapshot,
    path: &std::path::Path,
    text: &str,
    range: Range,
    settings: &Settings,
    encoding: Encoding,
) -> Vec<InlayHint> {
    if !settings.inlay_hints {
        return Vec::new();
    }
    let Some(file) = snapshot.file_of(path) else {
        return Vec::new();
    };
    if snapshot.sources.file(file).text() != text {
        return Vec::new();
    }
    let index = LineIndex::new(text);
    let start = convert::offset(text, range.start, encoding);
    let end = convert::offset(text, range.end, encoding);
    let parse = crol_syntax::parse(text);
    // Every value that computes something: of an entry, or an element of a
    // list, tuple or `Name(...)`, in document order.
    let values: Vec<Hinted> = parse
        .syntax()
        .descendants()
        .filter(|node| {
            let r = node.text_range();
            r.end() >= start && r.start() <= end
        })
        .filter_map(hinted)
        .collect();
    if values.is_empty() {
        return Vec::new();
    }

    // In data, the value after the operation the entry is part of; in a
    // value document, the final value.
    let operations = if let Some((layers, options)) = snapshot.chain_of(file) {
        match crol_eval::operation_values(
            &snapshot.sources,
            layers,
            options,
            file,
            &settings.context,
        ) {
            Ok(values) => Some(values),
            Err(_) => return Vec::new(),
        }
    } else {
        None
    };
    let mut hints = Vec::new();
    for Hinted {
        expr,
        entry,
        key,
        path,
    } in values
    {
        let value = match &operations {
            // The operation's value is the root key's.
            Some(values) => operation_of(&entry, values)
                .and_then(|v| crol_eval::value_at_path(v, &path).cloned()),
            None => features::value_at(snapshot, file, &key, &path, &settings.context),
        };
        let Some(value) = value else {
            continue;
        };
        hints.push(InlayHint {
            position: convert::position(text, &index, expr.text_range().end(), encoding),
            label: InlayHintLabel::String(format!("= {}", short(&value))),
            kind: None,
            text_edits: None,
            tooltip: None,
            padding_left: Some(true),
            padding_right: None,
            data: None,
        });
    }
    hints
}

/// A value worth a hint.
struct Hinted {
    expr: SyntaxNode,
    /// The entry around it, or that it is the value of.
    entry: ast::Entry,
    key: String,
    path: Vec<crol_eval::PathStep>,
}

/// `node` as a value worth a hint, if it is one: an entry whose value
/// computes something, or an element that does.
fn hinted(node: SyntaxNode) -> Option<Hinted> {
    if let Some(entry) = ast::Entry::cast(node.clone()) {
        let expr = entry.value().filter(|v| computes(v.syntax()))?;
        let token = entry.target()?.key()?.token()?;
        let (key, path) = features::target_path(&token)?;
        return Some(Hinted {
            expr: expr.syntax().clone(),
            entry,
            key,
            path,
        });
    }
    let parent = node.parent()?;
    let element = matches!(
        parent.kind(),
        SyntaxKind::LIST_EXPR | SyntaxKind::TUPLE_EXPR | SyntaxKind::ARG_LIST
    );
    if !element || ast::Expr::cast(node.clone()).is_none() || !computes(&node) {
        return None;
    }
    let (key, path) = features::element_path(&node)?;
    Some(Hinted {
        entry: node.ancestors().find_map(ast::Entry::cast)?,
        expr: node,
        key,
        path,
    })
}

/// Whether an expression computes something, so that its result is worth
/// showing: arithmetic, references and paths, not a literal or a block.
fn computes(node: &SyntaxNode) -> bool {
    match node.kind() {
        SyntaxKind::BINARY_EXPR | SyntaxKind::REFERENCE | SyntaxKind::PATH_EXPR => true,
        SyntaxKind::PREFIX_EXPR | SyntaxKind::PAREN_EXPR => node.children().any(|c| computes(&c)),
        _ => false,
    }
}

/// The value after the operation `entry` belongs to: the innermost entry
/// around it (itself included) whose target an operation starts at.
fn operation_of<'a>(entry: &ast::Entry, values: &'a [(Span, Option<Value>)]) -> Option<&'a Value> {
    entry
        .syntax()
        .ancestors()
        .filter_map(ast::Entry::cast)
        .find_map(|e| {
            let start = e.target()?.syntax().text_range().start();
            values
                .iter()
                .find(|(span, _)| span.range.start() == start)
                .map(|(_, v)| v.as_ref())
        })
        .flatten()
}

fn short(value: &Value) -> String {
    let text = print::to_text(value);
    if text.chars().count() <= MAX_HINT {
        return text;
    }
    let mut cut: String = text.chars().take(MAX_HINT - 1).collect();
    cut.push('…');
    cut
}
