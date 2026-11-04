//! Completion (`SPEC.md` 12.5).

use crol::schema::{SchemaFile, TypeDef, TypeExpr, VariantShape};
use crol_eval::{FileId, PathStep, Value, ValueKind, print};
use crol_syntax::ast::{self, AstNode};
use crol_syntax::{SyntaxKind, SyntaxNode, TextSize};
use lsp_types::{
    CompletionItem, CompletionItemKind, Documentation, MarkupContent, MarkupKind, Position, Uri,
};

use crate::Settings;
use crate::convert::{self, Encoding};
use crate::features;
use crate::schema;
use crate::workspace::Snapshot;

struct Completer<'a> {
    snapshot: &'a Snapshot,
    file: Option<FileId>,
    root: SyntaxNode,
    offset: TextSize,
    context: &'a [String],
}

/// What to offer at the cursor: after `&root.`, `&self.`, `&parent.` or a
/// bind, the keys of the value there; after `&`, the names a reference can
/// start with; after `@`, root keys; in `<...>`, variant names; in a block
/// of a known type, its fields not written yet; after `key:`, the values
/// its type allows.
pub(crate) fn complete(
    snapshot: &Snapshot,
    uri: &Uri,
    text: &str,
    position: Position,
    encoding: Encoding,
    settings: &Settings,
) -> Vec<CompletionItem> {
    let offset = convert::offset(text, position, encoding);
    let at = usize::from(offset);
    let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let before = &text[line_start..at];
    let partial_start = before
        .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .map_or(0, |i| i + 1);
    let lead = &before[..partial_start];
    let completer = Completer {
        snapshot,
        file: convert::uri_to_path(uri).and_then(|p| snapshot.file_of(&p)),
        root: crol_syntax::parse(text).syntax(),
        offset,
        context: &settings.context,
    };

    if let Some((reference, segments)) = reference_prefix(lead) {
        let spread = lead[..lead.len() - reference.len()]
            .trim_end()
            .ends_with("..");
        return completer.reference_keys(&segments, spread);
    }
    if lead.ends_with('@') {
        return completer.ids();
    }
    if lead.ends_with('&') {
        return completer.reference_starts();
    }
    let open_angle = before.rfind('<');
    if open_angle.is_some_and(|i| !before[i..].contains('>')) {
        let variants = &snapshot.evaluation.variants;
        let mut items: Vec<CompletionItem> = variants
            .ids()
            .map(|id| {
                item(
                    variants.name(id).to_owned(),
                    CompletionItemKind::ENUM_MEMBER,
                    None,
                )
            })
            .collect();
        // In a condition (`SPEC.md` 8.2).
        items.push(item("!".to_owned(), CompletionItemKind::OPERATOR, None));
        return items;
    }
    // Before the partial word, on this line or the ones before.
    let previous = text[..line_start + partial_start].trim_end();
    if previous.ends_with(':') && !previous.ends_with("::") {
        return completer.values();
    }
    if previous.ends_with(['{', ',', '+', '?']) {
        return completer.fields();
    }
    Vec::new()
}

/// `&self.limits.` → (`&self.limits.`, [`self`, `limits`]): a reference
/// path the lead ends in, with a `.` last.
fn reference_prefix(lead: &str) -> Option<(&str, Vec<&str>)> {
    if !lead.ends_with('.') {
        return None;
    }
    let start = lead.rfind('&')?;
    let reference = &lead[start..];
    let segments: Vec<&str> = reference[1..reference.len() - 1].split('.').collect();
    segments
        .iter()
        .all(|s| crol_eval::is_identifier(s) || s.chars().all(|c| c.is_ascii_digit()))
        .then_some((reference, segments))
}

fn item(label: String, kind: CompletionItemKind, detail: Option<String>) -> CompletionItem {
    CompletionItem {
        label,
        kind: Some(kind),
        detail,
        ..Default::default()
    }
}

fn documented(mut item: CompletionItem, doc: &str) -> CompletionItem {
    if !doc.is_empty() {
        item.documentation = Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: doc.to_owned(),
        }));
    }
    item
}

/// What a value is, in a word: its type name or kind.
fn describe(value: &Value) -> Option<String> {
    match &value.kind {
        ValueKind::Keyed(k) => k.name.clone(),
        ValueKind::NamedTuple(n, _) | ValueKind::UnitName(n) => Some(n.clone()),
        _ => {
            let text = print::to_text(value);
            (text.len() <= 40).then_some(text)
        }
    }
}

impl Completer<'_> {
    /// The evaluation the document belongs to, else the project's; while
    /// that has no value, the latest of the project's that had one.
    fn evaluation(&self) -> &crol_eval::Evaluation {
        let evaluation = self
            .file
            .and_then(|f| self.snapshot.evaluation_of(f))
            .unwrap_or(&self.snapshot.evaluation);
        match (&evaluation.value, &self.snapshot.last_good) {
            (None, Some(good)) if self.file.is_none_or(|f| self.snapshot.in_layers(f)) => good,
            _ => evaluation,
        }
    }

    fn root_value(&self) -> Option<&Value> {
        self.evaluation().value.as_ref()
    }

    fn keys_of(value: &Value, only_keyed: bool) -> Vec<CompletionItem> {
        let ValueKind::Keyed(keyed) = &value.kind else {
            return Vec::new();
        };
        keyed
            .entries
            .iter()
            .filter(|(_, v)| !only_keyed || matches!(v.kind, ValueKind::Keyed(_)))
            .map(|(key, v)| item(key.to_string(), CompletionItemKind::FIELD, describe(v)))
            .collect()
    }

    /// The value at a root key and path.
    fn value_at(&self, key: &str, path: &[PathStep]) -> Option<Value> {
        features::value_in(self.evaluation(), key, path, self.context)
    }

    /// The innermost blocks around the cursor, innermost first.
    fn blocks(&self) -> Vec<ast::Block> {
        let Some(mut token) = self.root.token_at_offset(self.offset).left_biased() else {
            return Vec::new();
        };
        // Trailing whitespace may belong to no block, as at the end of an
        // unclosed one; the token before it does.
        while token.kind().is_trivia()
            && let Some(previous) = token.prev_token()
        {
            token = previous;
        }
        token
            .parent_ancestors()
            .filter_map(ast::Block::cast)
            .filter(|block| {
                let inside_open = block
                    .syntax()
                    .children_with_tokens()
                    .find(|t| t.kind() == SyntaxKind::L_BRACE)
                    .is_some_and(|t| t.text_range().end() <= self.offset);
                let before_close = block
                    .syntax()
                    .children_with_tokens()
                    .find(|t| t.kind() == SyntaxKind::R_BRACE)
                    .is_none_or(|t| self.offset <= t.text_range().start());
                inside_open && before_close
            })
            .collect()
    }

    /// After `&root.`, `&self.`, `&parent.` or `&bind.`, and further
    /// fields: the keys of the value there.
    fn reference_keys(&self, segments: &[&str], spread: bool) -> Vec<CompletionItem> {
        let steps = |names: &[&str]| -> Vec<PathStep> {
            names
                .iter()
                .map(|n| match n.parse::<usize>() {
                    Ok(i) => PathStep::Element(i),
                    Err(_) => PathStep::Field((*n).to_owned()),
                })
                .collect()
        };
        let (base, rest) = (segments[0], &segments[1..]);
        let value = match base {
            "root" => match rest.split_first() {
                None => {
                    return self
                        .root_value()
                        .map_or_else(Vec::new, |v| Self::keys_of(v, spread));
                }
                Some((key, path)) => self.value_at(key, &steps(path)),
            },
            "self" | "parent" => {
                let blocks = self.blocks();
                let block = if base == "self" {
                    blocks.first()
                } else {
                    blocks.get(1)
                };
                block.and_then(|b| {
                    let (key, mut path) = features::block_path(b)?;
                    path.extend(steps(rest));
                    self.value_at(&key, &path)
                })
            }
            "default" => None,
            name => self.bind_location(name).and_then(|(key, mut path)| {
                path.extend(steps(rest));
                self.value_at(&key, &path)
            }),
        };
        value.map_or_else(Vec::new, |v| Self::keys_of(&v, spread))
    }

    /// Where an entry bind visible at the cursor points: its target's
    /// root key and path.
    fn bind_location(&self, name: &str) -> Option<(String, Vec<PathStep>)> {
        let token = self.root.token_at_offset(self.offset).left_biased()?;
        let declared = token
            .parent_ancestors()
            .filter(|n| matches!(n.kind(), SyntaxKind::BLOCK | SyntaxKind::DOCUMENT))
            .find_map(|n| crate::navigate::declared_in(&n, name))?;
        let entry = declared.parent()?.parent().and_then(ast::Entry::cast)?;
        features::target_path(&entry.target()?.key()?.token()?)
    }

    /// After `&`: `self`, `parent`, `root`, `default`, the binds visible at
    /// the cursor and the templates.
    fn reference_starts(&self) -> Vec<CompletionItem> {
        let mut items: Vec<CompletionItem> = ["self", "parent", "root", "default"]
            .into_iter()
            .map(|k| item(k.to_owned(), CompletionItemKind::KEYWORD, None))
            .collect();
        let mut names = Vec::new();
        for block in self.blocks() {
            for child in block.syntax().children() {
                let name = match child.kind() {
                    SyntaxKind::VALUE_BIND => ast::ValueBind::cast(child).and_then(|b| b.name()),
                    _ => child
                        .children()
                        .find_map(ast::Bind::cast)
                        .and_then(|b| b.name()),
                };
                if let Some(name) = name {
                    names.push((name.text().to_owned(), "bind"));
                }
            }
        }
        for name in template_names(self.snapshot) {
            names.push((name, "template"));
        }
        for (name, what) in names {
            if items.iter().all(|i| i.label != name) {
                items.push(item(
                    name,
                    CompletionItemKind::VARIABLE,
                    Some(what.to_owned()),
                ));
            }
        }
        items
    }

    /// After `@`: root keys, only those of the types the field may refer
    /// to when the schema says (`#[crol(refers(...))]`).
    fn ids(&self) -> Vec<CompletionItem> {
        let Some(root) = self.root_value() else {
            return Vec::new();
        };
        let refers = self.value_type().and_then(|ty| refers_of(&ty));
        let names: Option<Vec<&str>> = refers.map(|paths| {
            let schema = self.snapshot.schema_for(self.file).map(|s| s.file);
            paths
                .iter()
                .filter_map(|p| schema.and_then(|s| s.get(p)).map(TypeDef::name))
                .collect()
        });
        let mut items = Self::keys_of(root, false);
        if let Some(names) = names.filter(|n| !n.is_empty()) {
            items.retain(|i| i.detail.as_deref().is_some_and(|d| names.contains(&d)));
        }
        items
    }

    /// The type of the value being written at the cursor.
    fn value_type(&self) -> Option<TypeExpr> {
        let token = self.root.token_at_offset(self.offset).left_biased()?;
        let entry = token.parent_ancestors().find_map(ast::Entry::cast)?;
        let (ty, _) = schema::entry_type(self.snapshot, self.file, &entry)?;
        Some(ty)
    }

    /// After `key:`: the values the field's type allows.
    fn values(&self) -> Vec<CompletionItem> {
        let (Some(schema), Some(ty)) = (
            self.snapshot.schema_for(self.file).map(|s| s.file),
            self.value_type(),
        ) else {
            return Vec::new();
        };
        let mut items = Vec::new();
        value_items(schema, &ty, &mut items, 0);
        items
    }

    /// In a block of a known type: its fields not written yet, each with
    /// its doc. Accepting one writes the value it has now, if any, else a
    /// placeholder.
    fn fields(&self) -> Vec<CompletionItem> {
        let Some(document_schema) = self.snapshot.schema_for(self.file) else {
            return Vec::new();
        };
        let schema = document_schema.file;
        let block = self.blocks().into_iter().next();
        let (ty, entries): (_, Vec<ast::AnyEntry>) = match &block {
            Some(block) => (
                schema::block_type(self.snapshot, self.file, block),
                block.entries().collect(),
            ),
            // A document on its own written as entries, without braces:
            // the fields of its root type.
            None if document_schema.value_mode => {
                let Some(document) = ast::Document::cast(self.root.clone()) else {
                    return Vec::new();
                };
                (
                    Some(TypeExpr::Named(document_schema.root_type().to_owned())),
                    document.entries().collect(),
                )
            }
            None => return Vec::new(),
        };
        let Some((_, fields)) = ty.and_then(|ty| schema::struct_of(schema, &ty)) else {
            return Vec::new();
        };
        // The entry being typed is not written yet.
        let written: Vec<String> = entries
            .into_iter()
            .filter(|e| !e.syntax().text_range().contains_inclusive(self.offset))
            .filter_map(|e| match e {
                ast::AnyEntry::Entry(e) => e.target()?.key(),
                ast::AnyEntry::Remove(e) => e.target()?.key(),
                _ => None,
            })
            .filter_map(|k| features::key_text(&k))
            .collect();
        let location = block.as_ref().and_then(features::block_path);
        fields
            .iter()
            .filter(|(name, _)| !written.contains(name))
            .map(|(name, field)| {
                let current = location.as_ref().and_then(|(key, path)| {
                    let mut path = path.clone();
                    path.push(PathStep::Field(name.clone()));
                    self.value_at(key, &path)
                });
                let value = current
                    .as_ref()
                    .map_or_else(|| schema::placeholder(schema, &field.ty), print::to_text);
                let written_name = if crol_eval::is_identifier(name) {
                    name.clone()
                } else {
                    crol_eval::quote(name)
                };
                let mut item = documented(
                    item(
                        name.clone(),
                        CompletionItemKind::FIELD,
                        Some(schema::rust_type(schema, &field.ty)),
                    ),
                    &field.doc,
                );
                item.insert_text = Some(format!("{written_name}: {value}"));
                item
            })
            .collect()
    }
}

/// The types ids of a type may refer to: for `Id`, or ids in an option,
/// list or set.
fn refers_of(ty: &TypeExpr) -> Option<Vec<String>> {
    match ty {
        TypeExpr::Id(refers) => Some(refers.clone()),
        TypeExpr::Option(inner)
        | TypeExpr::List(inner)
        | TypeExpr::Set(inner)
        | TypeExpr::Array(inner, _) => refers_of(inner),
        _ => None,
    }
}

/// The values a type allows, as completions: `true` and `false`, `None`
/// and `Some(...)`, enum variants, and the type names of structs.
fn value_items(schema: &SchemaFile, ty: &TypeExpr, out: &mut Vec<CompletionItem>, depth: usize) {
    if depth > 8 {
        return;
    }
    let with_insert = |mut item: CompletionItem, text: String| {
        if text != item.label {
            item.insert_text = Some(text);
        }
        item
    };
    match ty {
        TypeExpr::Bool => {
            for b in ["true", "false"] {
                out.push(item(b.to_owned(), CompletionItemKind::KEYWORD, None));
            }
        }
        TypeExpr::Option(inner) => {
            out.push(item(
                "None".to_owned(),
                CompletionItemKind::ENUM_MEMBER,
                None,
            ));
            out.push(with_insert(
                item("Some".to_owned(), CompletionItemKind::ENUM_MEMBER, None),
                format!("Some({})", schema::placeholder(schema, inner)),
            ));
            value_items(schema, inner, out, depth + 1);
        }
        TypeExpr::Named(path) => match schema.get(path) {
            Some(TypeDef::Enum { variants, .. }) => {
                for (name, variant) in variants {
                    let text = match &variant.shape {
                        VariantShape::Unit => name.clone(),
                        VariantShape::Newtype(inner) => {
                            format!("{name}({})", schema::placeholder(schema, inner))
                        }
                        VariantShape::Tuple(items) => {
                            let items: Vec<String> = items
                                .iter()
                                .map(|t| schema::placeholder(schema, t))
                                .collect();
                            format!("{name}({})", items.join(", "))
                        }
                        VariantShape::Struct(_) => format!("{name} {{ .. }}"),
                    };
                    out.push(with_insert(
                        documented(
                            item(name.clone(), CompletionItemKind::ENUM_MEMBER, None),
                            &variant.doc,
                        ),
                        text,
                    ));
                }
            }
            Some(def @ (TypeDef::Struct { .. } | TypeDef::TupleStruct { .. })) => {
                out.push(with_insert(
                    documented(
                        item(def.name().to_owned(), CompletionItemKind::STRUCT, None),
                        def.doc(),
                    ),
                    schema::placeholder(schema, ty),
                ));
            }
            Some(TypeDef::UnitStruct { name, doc, .. }) => {
                out.push(documented(
                    item(name.clone(), CompletionItemKind::STRUCT, None),
                    doc,
                ));
            }
            Some(TypeDef::Newtype {
                name, doc, inner, ..
            }) => {
                out.push(with_insert(
                    documented(item(name.clone(), CompletionItemKind::STRUCT, None), doc),
                    schema::placeholder(schema, ty),
                ));
                value_items(schema, inner, out, depth + 1);
            }
            Some(TypeDef::Transparent { inner, .. }) => value_items(schema, inner, out, depth + 1),
            None => {}
        },
        _ => {}
    }
}

/// The names of the templates in every file.
fn template_names(snapshot: &Snapshot) -> Vec<String> {
    let mut names = Vec::new();
    for (_, file) in snapshot.sources.files() {
        let parse = crol_syntax::parse(file.text());
        for entry in parse.document().entries() {
            if let ast::AnyEntry::ValueBind(bind) = entry
                && let Some(name) = bind.name()
            {
                names.push(name.text().to_owned());
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_prefixes() {
        assert_eq!(
            reference_prefix("x: &self.limits."),
            Some(("&self.limits.", vec!["self", "limits"]))
        );
        assert_eq!(reference_prefix("..&root."), Some(("&root.", vec!["root"])));
        assert_eq!(reference_prefix("x: &self"), None);
        assert_eq!(reference_prefix("x: & + 1."), None);
    }
}
