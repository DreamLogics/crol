//! Code actions: quick fixes for diagnostics, the missing fields of a
//! struct, and the values below written as comments (`SPEC.md` 12.5).
//! Nothing is written unless the user picks an action.

use std::collections::HashMap;

use crol_eval::{FileId, Key, Value, ValueKind, print};
use crol_syntax::ast::{self, AstNode};
use crol_syntax::{LineIndex, SyntaxKind, SyntaxNode, SyntaxToken, TextRange, TextSize};
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, CodeActionTriggerKind,
    Diagnostic, TextEdit, Uri, WorkspaceEdit,
};

use crate::Settings;
use crate::convert::{self, Encoding};
use crate::features;
use crate::schema;
use crate::workspace::Snapshot;

/// What the actions work from.
struct Document<'a> {
    snapshot: Option<&'a Snapshot>,
    uri: &'a Uri,
    text: &'a str,
    index: LineIndex,
    root: SyntaxNode,
    encoding: Encoding,
    /// The file in the snapshot, if the snapshot has this text for it.
    file: Option<FileId>,
    context: &'a [String],
}

pub(crate) fn code_actions(
    snapshot: Option<&Snapshot>,
    uri: &Uri,
    text: &str,
    params: &CodeActionParams,
    settings: &Settings,
    encoding: Encoding,
) -> Vec<CodeActionOrCommand> {
    let file = snapshot.and_then(|s| {
        let file = s.file_of(&convert::uri_to_path(uri)?)?;
        (s.sources.file(file).text() == text).then_some(file)
    });
    if let (Some(snapshot), Some(file)) = (snapshot, file)
        && snapshot.is_read_only(file)
    {
        return Vec::new();
    }
    let document = Document {
        snapshot,
        uri,
        text,
        index: LineIndex::new(text),
        root: crol_syntax::parse(text).syntax(),
        encoding,
        file,
        context: &settings.context,
    };
    let mut actions = Vec::new();
    for diagnostic in &params.context.diagnostics {
        document.quick_fixes(diagnostic, &mut actions);
    }
    // Reading the values below evaluates the data again, so only when the
    // user asks for actions.
    if params.context.trigger_kind != Some(CodeActionTriggerKind::AUTOMATIC) {
        let offset = convert::offset(text, params.range.start, encoding);
        document.comment_values_below(offset, &mut actions);
    }
    actions
}

impl Document<'_> {
    fn range(&self, range: TextRange) -> lsp_types::Range {
        convert::range(self.text, &self.index, range, self.encoding)
    }

    fn action(
        &self,
        title: String,
        kind: CodeActionKind,
        diagnostic: Option<&Diagnostic>,
        edits: Vec<(TextRange, String)>,
    ) -> CodeActionOrCommand {
        let edits = edits
            .into_iter()
            .map(|(range, text)| TextEdit::new(self.range(range), text))
            .collect();
        // `WorkspaceEdit` has `Uri` keys.
        #[allow(clippy::mutable_key_type)]
        let changes = HashMap::from([(self.uri.clone(), edits)]);
        CodeActionOrCommand::CodeAction(CodeAction {
            title,
            kind: Some(kind),
            diagnostics: diagnostic.map(|d| vec![d.clone()]),
            edit: Some(WorkspaceEdit::new(changes)),
            is_preferred: Some(diagnostic.is_some()),
            ..Default::default()
        })
    }

    /// The innermost node covering a diagnostic.
    fn covering(&self, diagnostic: &Diagnostic) -> Option<SyntaxNode> {
        let start = convert::offset(self.text, diagnostic.range.start, self.encoding);
        let end = convert::offset(self.text, diagnostic.range.end, self.encoding);
        let range = TextRange::new(start, end.max(start));
        if range.end() > self.root.text_range().end() {
            return None;
        }
        Some(match self.root.covering_element(range) {
            crol_syntax::SyntaxElement::Node(node) => node,
            crol_syntax::SyntaxElement::Token(token) => token.parent()?,
        })
    }

    // ---- Quick fixes ----

    fn quick_fixes(&self, diagnostic: &Diagnostic, out: &mut Vec<CodeActionOrCommand>) {
        let message = diagnostic.message.as_str();
        let names = backticked(message);
        let Some(node) = self.covering(diagnostic) else {
            return;
        };
        let quick_fix = CodeActionKind::QUICKFIX;
        if message.contains("` already exists") {
            // `+x` on a key that exists: make it a patch.
            if let Some(entry) = node
                .ancestors()
                .filter_map(ast::Entry::cast)
                .find(|e| e.op() == ast::EntryOp::Declare)
                && let Some(plus) = entry.op_token()
            {
                let target = entry.target().map_or(String::new(), |t| {
                    t.syntax().text().to_string().trim().to_owned()
                });
                out.push(self.action(
                    format!("Patch `{target}` instead of declaring it"),
                    quick_fix,
                    Some(diagnostic),
                    vec![(plus.text_range(), String::new())],
                ));
            }
        } else if message.starts_with("no bind `") {
            // `&name` without a bind: `&self.name`.
            if let Some(name) = names.first()
                && let Some(reference) = node
                    .ancestors()
                    .chain(node.descendants())
                    .filter_map(ast::Reference::cast)
                    .find(|r| matches!(r.kind(), ast::ReferenceKind::Bind(t) if t.text() == *name))
            {
                out.push(self.action(
                    format!("Change to `&self.{name}`"),
                    quick_fix,
                    Some(diagnostic),
                    vec![(reference.syntax().text_range(), format!("&self.{name}"))],
                ));
            }
        } else if message.contains("names no root key") {
            // A misspelt id.
            let id = node
                .ancestors()
                .chain(node.descendants())
                .find(|n| n.kind() == SyntaxKind::ID_EXPR)
                .and_then(|n| {
                    n.children_with_tokens()
                        .filter_map(|it| it.into_token())
                        .find(|t| matches!(t.kind(), SyntaxKind::IDENT | SyntaxKind::STRING))
                });
            if let Some(token) = id {
                let name = token.text().trim_matches('"');
                for key in closest(name, &self.root_keys()) {
                    out.push(self.rename_to(&token, &key, diagnostic));
                }
            }
        } else if message.contains("` has no field `") && names.len() >= 3 {
            // A misspelt field: the message lists the fields.
            let field = names[1];
            let candidates: Vec<String> = names[2..].iter().map(|s| (*s).to_owned()).collect();
            if let Some(token) = target_key(&node, field) {
                for name in closest(field, &candidates) {
                    out.push(self.rename_to(&token, &name, diagnostic));
                }
            }
        } else if message.contains("` has no variant `") && names.len() >= 3 {
            let variant = names[1];
            let candidates: Vec<String> = names[2..].iter().map(|s| (*s).to_owned()).collect();
            let token = node
                .descendants_with_tokens()
                .filter_map(|it| it.into_token())
                .find(|t| {
                    t.kind() == SyntaxKind::IDENT
                        && t.text() == variant
                        && t.parent().is_some_and(|p| {
                            matches!(
                                p.kind(),
                                SyntaxKind::UNIT_NAME | SyntaxKind::NAMED_TUPLE | SyntaxKind::BLOCK
                            )
                        })
                });
            if let Some(token) = token {
                for name in closest(variant, &candidates) {
                    out.push(self.rename_to(&token, &name, diagnostic));
                }
            }
        } else if message.contains("is missing the field") || message.starts_with("missing field `")
        {
            let block = node
                .ancestors()
                .chain(node.descendants())
                .find_map(ast::Block::cast);
            let Some(block) = block else {
                return;
            };
            let missing: Vec<&str> = if message.starts_with("missing field `") {
                names.iter().take(1).copied().collect()
            } else {
                names
                    .iter()
                    .skip(1)
                    .copied()
                    .filter(|n| *n != "..")
                    .collect()
            };
            self.add_fields(&block, &missing, diagnostic, out);
            if message.contains("is missing the field") && !has_defaults_marker(&block) {
                out.push(self.action(
                    "End the block with `..`".to_owned(),
                    quick_fix,
                    Some(diagnostic),
                    vec![insert_at_end(self.text, &block, &["..".to_owned()])],
                ));
            }
        }
    }

    fn rename_to(
        &self,
        token: &SyntaxToken,
        name: &str,
        diagnostic: &Diagnostic,
    ) -> CodeActionOrCommand {
        let written = if token.kind() == SyntaxKind::STRING || !crol_eval::is_identifier(name) {
            crol_eval::quote(name)
        } else {
            name.to_owned()
        };
        self.action(
            format!("Change to `{name}`"),
            CodeActionKind::QUICKFIX,
            Some(diagnostic),
            vec![(token.text_range(), written)],
        )
    }

    /// The root keys of the evaluation the document belongs to.
    fn root_keys(&self) -> Vec<String> {
        let (Some(snapshot), Some(file)) = (self.snapshot, self.file) else {
            return Vec::new();
        };
        match snapshot
            .evaluation_of(file)
            .and_then(|e| e.value.as_ref())
            .map(|v| &v.kind)
        {
            Some(ValueKind::Keyed(keyed)) => keyed
                .entries
                .keys()
                .filter_map(|k| match k {
                    Key::Str(s) => Some(s.to_string()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Adds the fields `missing` to a block: each from the value below if
    /// it has one, else a placeholder of its type.
    fn add_fields(
        &self,
        block: &ast::Block,
        missing: &[&str],
        diagnostic: &Diagnostic,
        out: &mut Vec<CodeActionOrCommand>,
    ) {
        if missing.is_empty() {
            return;
        }
        let below = self.value_below(block);
        let fields = self.fields_of(block);
        let mut lines = Vec::new();
        for name in missing {
            let from_below = match below.as_ref().map(|v| &v.kind) {
                Some(ValueKind::Keyed(keyed)) => keyed.entries.get(&Key::Str((*name).into())),
                _ => None,
            };
            let value = match (from_below, &fields) {
                (Some(value), _) => print::to_text(value),
                (None, Some((schema, fields))) => match schema::field(fields, name) {
                    Some(field) => schema::placeholder(schema, &field.ty),
                    None => return,
                },
                (None, None) => return,
            };
            lines.push(format!("{}: {value}", key_name(name)));
        }
        let s = if missing.len() == 1 { "" } else { "s" };
        out.push(self.action(
            format!("Add the missing field{s}"),
            CodeActionKind::QUICKFIX,
            Some(diagnostic),
            vec![insert_at_end(self.text, block, &lines)],
        ));
    }

    /// The schema's fields for a block, from its type.
    fn fields_of(
        &self,
        block: &ast::Block,
    ) -> Option<(
        &crol::schema::SchemaFile,
        &indexmap::IndexMap<String, crol::schema::Field>,
    )> {
        let snapshot = self.snapshot?;
        let schema = snapshot.schema.as_ref()?;
        let ty = schema::block_type(snapshot, block)?;
        schema::struct_of(schema, &ty).map(|(_, fields)| (schema, fields))
    }

    /// The value below the entry a block is the value of: the value at its
    /// target before the operation the entry is part of, in the context of
    /// the `context` setting (`SPEC.md` 7.3).
    fn value_below(&self, block: &ast::Block) -> Option<Value> {
        let snapshot = self.snapshot?;
        let file = self.file?;
        if !snapshot.in_layers(file) {
            return None;
        }
        let (key, path) = block_path(block)?;
        let steps = crol_eval::explain(
            &snapshot.sources,
            &snapshot.layers,
            &snapshot.options,
            &features::describe_path(&key, &path),
            self.context,
        )
        .ok()?;
        // The operation the block is in: the innermost entry around it
        // whose target an operation in this file starts at.
        let at = block
            .syntax()
            .ancestors()
            .filter_map(ast::Entry::cast)
            .find_map(|e| {
                let start = e.target()?.syntax().text_range().start();
                steps
                    .iter()
                    .position(|s| s.span.file == file && s.span.range.start() == start)
            })?;
        steps[..at].last()?.value.clone()
    }

    // ---- Refactors ----

    /// On an entry whose value is a block: the fields of the value below
    /// that the block does not set, as `// field: value` lines.
    fn comment_values_below(&self, offset: TextSize, out: &mut Vec<CodeActionOrCommand>) {
        let Some(token) = self.root.token_at_offset(offset).right_biased() else {
            return;
        };
        let Some(block) = token.parent_ancestors().find_map(|n| {
            let entry = ast::Entry::cast(n)?;
            match entry.value()? {
                ast::Expr::Block(block) => Some(block),
                _ => None,
            }
        }) else {
            return;
        };
        let Some(below) = self.value_below(&block) else {
            return;
        };
        let ValueKind::Keyed(keyed) = &below.kind else {
            return;
        };
        let set: Vec<String> = block
            .entries()
            .filter_map(|e| match e {
                ast::AnyEntry::Entry(e) => e.target()?.key(),
                ast::AnyEntry::Remove(e) => e.target()?.key(),
                _ => None,
            })
            .filter_map(|k| features::key_text(&k))
            .collect();
        let lines: Vec<String> = keyed
            .entries
            .iter()
            .filter(|(k, _)| !matches!(k, Key::Str(s) if set.iter().any(|n| **n == **s)))
            .map(|(k, v)| format!("// {k}: {}", print::to_text(v)))
            .collect();
        if lines.is_empty() {
            return;
        }
        out.push(self.action(
            "Write the values below as comments".to_owned(),
            CodeActionKind::REFACTOR_REWRITE,
            None,
            insert_at_start(self.text, &block, &lines),
        ));
    }
}

/// The text between each pair of backticks.
fn backticked(message: &str) -> Vec<&str> {
    message.split('`').skip(1).step_by(2).collect()
}

/// The key named `name` of an entry around the node (the diagnostic may
/// be on its value) or inside it.
fn target_key(node: &SyntaxNode, name: &str) -> Option<SyntaxToken> {
    node.ancestors()
        .filter_map(ast::Entry::cast)
        .filter_map(|e| e.target())
        .chain(node.descendants().filter_map(ast::Target::cast))
        .filter_map(|t| t.key())
        .find(|k| features::key_text(k).as_deref() == Some(name))
        .and_then(|k| k.token())
}

/// The root key and path of the entry a block is the value of.
fn block_path(block: &ast::Block) -> Option<(String, Vec<crol_eval::PathStep>)> {
    let entry = block.syntax().ancestors().find_map(ast::Entry::cast)?;
    features::target_path(&entry.target()?.key()?.token()?)
}

fn key_name(name: &str) -> String {
    if crol_eval::is_identifier(name) {
        name.to_owned()
    } else {
        crol_eval::quote(name)
    }
}

fn has_defaults_marker(block: &ast::Block) -> bool {
    block
        .entries()
        .any(|e| matches!(e, ast::AnyEntry::Defaults(_)))
}

/// The names in `candidates` closest to `name` by edit distance, if close
/// enough to be a misspelling, best first.
fn closest(name: &str, candidates: &[String]) -> Vec<String> {
    let limit = (name.chars().count() / 3).max(1);
    let mut found: Vec<(usize, &String)> = candidates
        .iter()
        .filter(|c| c.as_str() != name)
        .map(|c| (distance(name, c), c))
        .filter(|(d, _)| *d <= limit)
        .collect();
    found.sort();
    found.into_iter().take(3).map(|(_, c)| c.clone()).collect()
}

/// The edit distance between two names, with swapping two neighbouring
/// characters as one edit (optimal string alignment).
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[a.len()][b.len()]
}

// ---- Edits in blocks ----

fn brace(block: &ast::Block, kind: SyntaxKind) -> Option<SyntaxToken> {
    block
        .syntax()
        .children_with_tokens()
        .filter_map(|it| it.into_token())
        .find(|t| t.kind() == kind)
}

/// The whitespace a line starts with.
fn line_indent(text: &str, offset: TextSize) -> &str {
    let start = text[..usize::from(offset)].rfind('\n').map_or(0, |i| i + 1);
    let line = &text[start..];
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
}

/// The indentation of a block's entries: that of its first entry if it
/// starts a line, else one level more than the line the block is on.
fn entry_indent(text: &str, block: &ast::Block) -> String {
    if let Some(first) = block.entries().next() {
        let start = usize::from(first.syntax().text_range().start());
        let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
        if text[line_start..start].trim().is_empty() {
            return text[line_start..start].to_owned();
        }
    }
    format!(
        "{}    ",
        line_indent(text, block.syntax().text_range().start())
    )
}

/// Inserts entries after a block's last entry, before a `..`.
fn insert_at_end(text: &str, block: &ast::Block, lines: &[String]) -> (TextRange, String) {
    let multiline = block.syntax().text().contains_char('\n');
    let last = block
        .entries()
        .filter(|e| !matches!(e, ast::AnyEntry::Defaults(_)))
        .last();
    let (at, comma) = match &last {
        Some(entry) => {
            let comma = entry
                .syntax()
                .last_token()
                .and_then(|t| next_significant(&t))
                .filter(|t| t.kind() == SyntaxKind::COMMA);
            match comma {
                Some(comma) => (comma.text_range().end(), true),
                None => (entry.syntax().text_range().end(), false),
            }
        }
        None => match brace(block, SyntaxKind::L_BRACE) {
            Some(brace) => (brace.text_range().end(), true),
            None => (block.syntax().text_range().end(), true),
        },
    };
    let mut insert = String::new();
    if multiline {
        let indent = entry_indent(text, block);
        if !comma {
            insert.push(',');
        }
        for line in lines {
            insert.push_str(&format!("\n{indent}{line},"));
        }
    } else if comma {
        for line in lines {
            insert.push_str(&format!(" {line},"));
        }
    } else {
        for line in lines {
            insert.push_str(&format!(", {line}"));
        }
    }
    (TextRange::empty(at), insert)
}

/// Inserts lines at the start of a block, on lines of their own.
fn insert_at_start(text: &str, block: &ast::Block, lines: &[String]) -> Vec<(TextRange, String)> {
    let Some(open) = brace(block, SyntaxKind::L_BRACE) else {
        return Vec::new();
    };
    let indent = entry_indent(text, block);
    let mut insert: String = lines.iter().map(|l| format!("\n{indent}{l}")).collect();
    let at = open.text_range().end();
    // In a block on one line, what follows the brace moves to a line of its
    // own as well.
    let next = next_significant(&open);
    let end = match &next {
        Some(next) if !block.syntax().text().contains_char('\n') => {
            if next.kind() == SyntaxKind::R_BRACE {
                insert.push('\n');
                insert.push_str(line_indent(text, block.syntax().text_range().start()));
            } else {
                insert.push('\n');
                insert.push_str(&indent);
            }
            next.text_range().start()
        }
        _ => at,
    };
    vec![(TextRange::new(at, end), insert)]
}

fn next_significant(token: &SyntaxToken) -> Option<SyntaxToken> {
    let mut next = token.next_token();
    while let Some(t) = &next {
        if !t.kind().is_trivia() {
            break;
        }
        next = t.next_token();
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn misspellings() {
        let names = ["replicas", "port", "ports", "image"].map(str::to_owned);
        assert_eq!(closest("replcas", &names), ["replicas"]);
        assert_eq!(closest("prot", &names), ["port"]);
        assert_eq!(closest("prts", &names), ["ports"]);
        assert!(closest("zzz", &names).is_empty());
        assert_eq!(distance("kitten", "sitting"), 3);
        assert_eq!(distance("prot", "port"), 1);
    }

    #[test]
    fn backticks() {
        assert_eq!(
            backticked("`Service` has no field `prot`; its fields are `port`, `image`"),
            ["Service", "prot", "port", "image"]
        );
    }

    fn apply(text: &str, edits: &[(TextRange, String)]) -> String {
        let mut out = text.to_owned();
        for (range, insert) in edits.iter().rev() {
            out.replace_range(usize::from(range.start())..usize::from(range.end()), insert);
        }
        out
    }

    fn block(text: &str) -> ast::Block {
        crol_syntax::parse(text)
            .syntax()
            .descendants()
            .find_map(ast::Block::cast)
            .unwrap()
    }

    #[test]
    fn inserting_entries() {
        let lines = ["a: 1".to_owned(), "b: 2".to_owned()];
        let at_end = |text: &str| {
            let edit = insert_at_end(text, &block(text), &lines);
            apply(text, &[edit])
        };
        assert_eq!(at_end("x: S { c: 3 },"), "x: S { c: 3, a: 1, b: 2 },");
        assert_eq!(
            at_end("x: S { c: 3, .. },"),
            "x: S { c: 3, a: 1, b: 2, .. },"
        );
        assert_eq!(at_end("x: S { .. },"), "x: S { a: 1, b: 2, .. },");
        assert_eq!(
            at_end("x: S {\n    c: 3,\n},"),
            "x: S {\n    c: 3,\n    a: 1,\n    b: 2,\n},"
        );
        assert_eq!(
            at_end("x: S {\n    c: 3\n},"),
            "x: S {\n    c: 3,\n    a: 1,\n    b: 2,\n},"
        );

        let comments = ["// a: 1".to_owned()];
        let at_start = |text: &str| {
            let edits = insert_at_start(text, &block(text), &comments);
            apply(text, &edits)
        };
        assert_eq!(
            at_start("x: {\n    c: 3,\n},"),
            "x: {\n    // a: 1\n    c: 3,\n},"
        );
        assert_eq!(at_start("x: { c: 3 },"), "x: {\n    // a: 1\n    c: 3 },");
        assert_eq!(at_start("x: {},"), "x: {\n    // a: 1\n},");
    }
}
