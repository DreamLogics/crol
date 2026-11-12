//! Completion (`SPEC.md` 12.5).

use crol::Mode;
use crol::schema::{SchemaFile, TypeDef, TypeExpr, VariantShape};
use crol_eval::{FileId, PathStep, Value, ValueKind, print};
use crol_syntax::ast::{self, AstNode};
use crol_syntax::{LineIndex, SyntaxKind, SyntaxNode, TextRange, TextSize};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionTextEdit, Documentation, MarkupContent,
    MarkupKind, Position, TextEdit, Uri,
};

use crate::Settings;
use crate::attributes;
use crate::convert::{self, Encoding};
use crate::features;
use crate::place::{self, Cursor, Place, Scope, Slot};
use crate::schema::{self, Shape};
use crate::workspace::Snapshot;

struct Completer<'a> {
    snapshot: &'a Snapshot,
    file: Option<FileId>,
    root: SyntaxNode,
    offset: TextSize,
    context: &'a [String],
    /// Where the word at the cursor starts in the data.
    cursor: Cursor<'a>,
    /// Whether a `:` follows the word, so that a key needs no value.
    before_colon: bool,
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Whether an offset is inside a string or comment, where the characters
/// that trigger completion mean nothing. A string or comment that is not
/// closed goes on up to the end of the text.
fn in_string_or_comment(text: &str, at: usize) -> bool {
    let mut start = 0;
    for token in crol_syntax::tokenize(text).0 {
        let end = start + usize::from(token.len);
        if start < at && at <= end {
            let open = match token.kind {
                SyntaxKind::LINE_COMMENT => true,
                SyntaxKind::BLOCK_COMMENT => !text[start..end].ends_with("*/"),
                SyntaxKind::STRING => !closed_string(&text[start..end]),
                SyntaxKind::RAW_STRING => {
                    let hashes = text[start + 1..end]
                        .chars()
                        .take_while(|c| *c == '#')
                        .count();
                    !text[start..end].ends_with(&format!("\"{}", "#".repeat(hashes)))
                        || end - start < hashes * 2 + 3
                }
                _ => false,
            };
            let text_like = matches!(
                token.kind,
                SyntaxKind::STRING
                    | SyntaxKind::RAW_STRING
                    | SyntaxKind::CHAR
                    | SyntaxKind::LINE_COMMENT
                    | SyntaxKind::BLOCK_COMMENT
            );
            return text_like && (at < end || open);
        }
        start = end;
    }
    false
}

/// Whether a string token has its closing quote, which a `\` would escape.
fn closed_string(token: &str) -> bool {
    let Some(body) = token.strip_prefix('"').and_then(|t| t.strip_suffix('"')) else {
        return false;
    };
    let backslashes = body.chars().rev().take_while(|c| *c == '\\').count();
    backslashes % 2 == 0
}

/// What to offer at the cursor: after `&root.`, `&self.`, `&parent.` or a
/// bind, the keys of the value there; after `&`, the names a reference can
/// start with; after `@`, root keys; in `<...>`, variant names; in a block
/// of a known type, its fields not written yet; after a `.` in a target,
/// the fields there; where a value goes, the values its type allows; after
/// `#[` and `#![`, the attributes. Each item replaces the word at the
/// cursor. Nothing in strings and comments.
pub(crate) fn complete(
    snapshot: &Snapshot,
    uri: &Uri,
    text: &str,
    position: Position,
    encoding: Encoding,
    settings: &Settings,
) -> (Vec<CompletionItem>, bool) {
    let offset = convert::offset(text, position, encoding);
    let at = usize::from(offset);
    if in_string_or_comment(text, at) {
        let items = type_paths_in_string(snapshot, uri, text, offset, encoding);
        return (items, false);
    }
    let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let before = &text[line_start..at];
    let partial_start = before
        .char_indices()
        .rfind(|(_, c)| !is_word(*c))
        .map_or(0, |(i, c)| i + c.len_utf8());
    let word_start = line_start + partial_start;
    let word_end = text[at..]
        .find(|c| !is_word(c))
        .map_or(text.len(), |i| at + i);
    let mut items = items_at(
        snapshot,
        uri,
        text,
        (offset, word_start, word_end),
        settings,
    );
    let incomplete = narrow(&mut items, &text[word_start..at]);
    let index = LineIndex::new(text);
    let word = TextRange::new(
        TextSize::try_from(word_start).unwrap_or_default(),
        TextSize::try_from(word_end).unwrap_or_default(),
    );
    let range = convert::range(text, &index, word, encoding);
    for item in &mut items {
        let new_text = item.insert_text.clone().unwrap_or(item.label.clone());
        item.text_edit = Some(CompletionTextEdit::Edit(TextEdit::new(range, new_text)));
        item.filter_text = Some(item.label.clone());
    }
    (items, incomplete)
}

/// The most items a list of completions holds: a large project has thousands
/// of root keys, too many to send at every key.
const MAX_ITEMS: usize = 500;

/// Keeps a list too long to send to the items that match what is typed, as
/// editors match them, the best first, up to [`MAX_ITEMS`]. Returns whether
/// it left any out: the editor then asks again as the word grows.
fn narrow(items: &mut Vec<CompletionItem>, typed: &str) -> bool {
    if items.len() <= MAX_ITEMS {
        return false;
    }
    let typed = typed.to_lowercase();
    let in_order = |label: &str| {
        let mut chars = label.chars();
        typed.chars().all(|wanted| chars.any(|c| c == wanted))
    };
    items.retain(|item| in_order(&item.label.to_lowercase()));
    items.sort_by_cached_key(|item| {
        let label = item.label.to_lowercase();
        (
            !label.starts_with(&typed),
            !label.contains(&typed),
            label.len(),
            label,
        )
    });
    let left_out = items.len() > MAX_ITEMS;
    items.truncate(MAX_ITEMS);
    left_out
}

/// Whether a schema is the one of schema files.
fn of_schema_files(schema: &SchemaFile) -> bool {
    schema.root == crol::schema::SCHEMA_PATH
}

/// The paths of a schema file's types, from its latest evaluation.
fn type_paths(snapshot: &Snapshot, file: Option<FileId>) -> Vec<String> {
    let evaluation = features::evaluation_for(snapshot, file);
    let types = features::value_in(evaluation, "types", &[], &[]);
    let Some(ValueKind::Keyed(keyed)) = types.map(|v| v.kind) else {
        return Vec::new();
    };
    keyed
        .entries
        .keys()
        .filter_map(|key| match key {
            crol_eval::Key::Str(path) => Some(path.to_string()),
            crol_eval::Key::Int(_) | crol_eval::Key::Id(_) => None,
        })
        .collect()
}

/// Whether a value at the cursor is the path of a type in a schema file:
/// `root`, or what `Named(...)` holds.
fn wants_type_path(cursor: &Cursor<'_>) -> bool {
    let root = [PathStep::Field("root".to_owned())];
    cursor.call() == Some("Named")
        || cursor
            .value()
            .is_some_and(|place| place.path.as_deref() == Some(&root))
}

/// In a string of a schema file that is the path of a type: the paths of its
/// types, each replacing what the quotes hold.
fn type_paths_in_string(
    snapshot: &Snapshot,
    uri: &Uri,
    text: &str,
    offset: TextSize,
    encoding: Encoding,
) -> Vec<CompletionItem> {
    let file = convert::uri_to_path(uri).and_then(|p| snapshot.file_of(&p));
    if !snapshot
        .schema_for(file)
        .is_some_and(|s| of_schema_files(s.file))
    {
        return Vec::new();
    }
    let root = crol_syntax::parse(text).syntax();
    let Some(token) = root
        .token_at_offset(offset)
        .find(|t| t.kind() == SyntaxKind::STRING)
    else {
        return Vec::new();
    };
    let scope = Scope::new(snapshot, file);
    if !wants_type_path(&place::locate(&scope, text, token.text_range().start())) {
        return Vec::new();
    }
    // What the quotes hold. A string not closed yet goes on past its line.
    let quote = TextSize::of('"');
    let start = token.text_range().start() + quote;
    let rest = &token.text()[1..];
    let line = rest.split('\n').next().unwrap_or(rest);
    let line = line.strip_suffix('"').unwrap_or(line);
    let inside = TextRange::at(start, TextSize::of(line));
    let index = LineIndex::new(text);
    let edit_range = convert::range(text, &index, inside, encoding);
    type_paths(snapshot, file)
        .into_iter()
        .map(|path| {
            let mut item = item(path.clone(), CompletionItemKind::CLASS, None);
            item.filter_text = Some(path.clone());
            item.text_edit = Some(CompletionTextEdit::Edit(TextEdit::new(edit_range, path)));
            item
        })
        .collect()
}

fn items_at(
    snapshot: &Snapshot,
    uri: &Uri,
    text: &str,
    (offset, word_start, word_end): (TextSize, usize, usize),
    settings: &Settings,
) -> Vec<CompletionItem> {
    let at = usize::from(offset);
    let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let before = &text[line_start..at];
    let lead = &text[line_start..word_start];
    let file = convert::uri_to_path(uri).and_then(|p| snapshot.file_of(&p));
    let scope = Scope::new(snapshot, file);
    let after = text[word_end..].trim_start_matches([' ', '\t']);
    let completer = Completer {
        snapshot,
        file,
        root: crol_syntax::parse(text).syntax(),
        offset,
        context: &settings.context,
        cursor: place::locate(
            &scope,
            text,
            TextSize::try_from(word_start).unwrap_or_default(),
        ),
        before_colon: after.starts_with(':') && !after.starts_with("::"),
    };

    let word = TextRange::new(
        TextSize::try_from(word_start).unwrap_or_default(),
        TextSize::try_from(word_end).unwrap_or_default(),
    );
    // A name that states no mode is value mode here (`SPEC.md` 12.5).
    let mode = convert::uri_to_path(uri)
        .and_then(|p| Mode::of_path(&p))
        .unwrap_or(Mode::Value);
    if let Some(items) = attributes::complete(text, word, &completer.cursor, mode) {
        return items;
    }
    if let Some(items) = completer.in_type(text, word_start) {
        return items;
    }

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
        let evaluation = file.and_then(|f| snapshot.evaluation_of(f));
        let variants = &evaluation.unwrap_or(&snapshot.evaluation).variants;
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
    match completer.cursor.slot {
        Slot::Key => completer.bracket_fields(),
        Slot::Path => completer.path_fields(),
        Slot::Value => completer.values(),
        Slot::None => Vec::new(),
    }
}

/// `&self.limits.` → (`&self.limits.`, [`self`, `limits`]): a reference
/// path the lead ends in, with a `.` last. An accessor is a segment of its
/// own: `&root.a[0].` → [`root`, `a`, `[0]`].
fn reference_prefix(lead: &str) -> Option<(&str, Vec<&str>)> {
    if !lead.ends_with('.') {
        return None;
    }
    let start = lead.rfind('&')?;
    let reference = &lead[start..];
    let mut segments = Vec::new();
    for part in reference[1..reference.len() - 1].split('.') {
        let name_end = part.find('[').unwrap_or(part.len());
        let (name, mut accessors) = part.split_at(name_end);
        if !(crol_eval::is_identifier(name) || name.chars().all(|c| c.is_ascii_digit()))
            || name.is_empty()
        {
            return None;
        }
        segments.push(name);
        while !accessors.is_empty() {
            let end = accessors.find(']')? + 1;
            let accessor = &accessors[..end];
            accessor_step(accessor)?;
            segments.push(accessor);
            accessors = &accessors[end..];
        }
    }
    Some((reference, segments))
}

/// The step of `[0]` or `["key"]`.
fn accessor_step(accessor: &str) -> Option<PathStep> {
    let inner = accessor.strip_prefix('[')?.strip_suffix(']')?;
    match inner.parse() {
        Ok(index) => Some(PathStep::Accessor(index)),
        Err(_) => inner
            .starts_with('"')
            .then(|| PathStep::Field(features::string_text(inner))),
    }
}

/// The Rust names a type can be written with (`SPEC.md` 6.1.2).
const RUST_TYPE_NAMES: &[&str] = &[
    "bool",
    "char",
    "i8",
    "i16",
    "i32",
    "i64",
    "i128",
    "isize",
    "u8",
    "u16",
    "u32",
    "u64",
    "u128",
    "usize",
    "f32",
    "f64",
    "String",
    "PathBuf",
    "Option",
    "Vec",
    "VecDeque",
    "HashSet",
    "BTreeSet",
    "HashMap",
    "BTreeMap",
    "IndexMap",
    "Box",
    "crol::Id",
    "crol::IdOr",
    "crol::Value",
];

impl Completer<'_> {
    /// In the `( ... )` of a `#[type(...)]`: after a `.`, the steps the type
    /// there has; else type names. `None` outside one.
    fn in_type(&self, text: &str, word_start: usize) -> Option<Vec<CompletionItem>> {
        let token = self.root.token_at_offset(self.offset).left_biased()?;
        let attr = token
            .parent_ancestors()
            .find(|n| n.kind() == SyntaxKind::ENTRY_ATTR)?;
        let open = attr
            .children_with_tokens()
            .filter_map(|t| t.into_token())
            .find(|t| t.kind() == SyntaxKind::L_PAREN)?;
        let start = usize::from(open.text_range().end());
        if word_start < start {
            return Some(Vec::new());
        }
        let schema = self.snapshot.schema_for(self.file)?.file;
        let written = text[start..word_start].trim_end();
        let Some(base) = written.strip_suffix('.') else {
            let mut items: Vec<CompletionItem> = RUST_TYPE_NAMES
                .iter()
                .map(|name| item((*name).to_owned(), CompletionItemKind::KEYWORD, None))
                .collect();
            let mut names: Vec<&str> = schema.types.values().map(TypeDef::name).collect();
            names.sort_unstable();
            names.dedup();
            items.extend(
                names
                    .into_iter()
                    .filter(|n| !n.is_empty())
                    .map(|n| item(n.to_owned(), CompletionItemKind::STRUCT, None)),
            );
            return Some(items);
        };
        // What is written before the `.`, as a type reference of its own.
        let document = crol_syntax::parse(&format!("#[type({base})] x := ()")).document();
        let type_ref = document
            .syntax()
            .descendants()
            .find_map(ast::TypeRef::cast)?;
        let uses = crol::typed::TypeUses::of(&ast::Document::cast(self.root.clone())?, schema);
        let at = crol::typed::resolve_steps(schema, &uses, &type_ref, usize::MAX).ok()?;
        let items = crol::typed::next_steps(schema, &at)
            .into_iter()
            .map(|offer| {
                let detail = Some(schema::rust_type(schema, &offer.ty));
                let item = item(offer.label, CompletionItemKind::FIELD, detail);
                documented(item, offer.doc.as_deref().unwrap_or_default())
            })
            .collect();
        Some(items)
    }
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
        ValueKind::Keyed(k) => k.name.as_deref().map(str::to_owned),
        ValueKind::NamedTuple(n, _) | ValueKind::UnitName(n) => Some(n.to_string()),
        _ => {
            let text = print::to_text(value);
            (text.len() <= 40).then_some(text)
        }
    }
}

impl Completer<'_> {
    fn evaluation(&self) -> &crol_eval::Evaluation {
        features::evaluation_for(self.snapshot, self.file)
    }

    fn root_value(&self) -> Option<&Value> {
        self.evaluation().value.as_ref()
    }

    /// The keys `@name` can name at `place`, innermost scope first, with
    /// what each holds.
    fn visible_keys(
        root: &Value,
        resolver: &crol::IdResolver<'_>,
        place: &crol::DataPath,
    ) -> Vec<CompletionItem> {
        resolver
            .visible(place)
            .into_iter()
            .map(|name| {
                let id: crol::Id = crol::Id::key(name.as_str());
                let detail = resolver
                    .resolve(&id, place)
                    .and_then(|target| target.value_in(root).and_then(describe));
                item(
                    crol_eval::Key::Str(name.as_str().into()).to_string(),
                    CompletionItemKind::FIELD,
                    detail,
                )
            })
            .collect()
    }

    /// The keys of a value, through a named tuple of one element, which a
    /// field selects in (`SPEC.md` 5.1).
    fn keys_of(value: &Value, only_keyed: bool) -> Vec<CompletionItem> {
        let ValueKind::Keyed(keyed) = &crol_eval::single_payload_value(value).kind else {
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

    /// After `&root.`, `&top.`, `&self.`, `&parent.` (or `&super.`) or `&bind.`, and further
    /// fields: the keys of the value there.
    fn reference_keys(&self, segments: &[&str], spread: bool) -> Vec<CompletionItem> {
        let steps = |names: &[&str]| -> Vec<PathStep> {
            names
                .iter()
                .map(|n| match (accessor_step(n), n.parse::<usize>()) {
                    (Some(step), _) => step,
                    (None, Ok(i)) => PathStep::Element(i),
                    (None, Err(_)) => PathStep::Field((*n).to_owned()),
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
            "self" | "parent" | "super" | "top" => {
                let blocks = self.cursor.blocks();
                let block = blocks.get(usize::from(matches!(base, "parent" | "super")));
                let mut path = block.and_then(|b| b.path.clone()).unwrap_or_default();
                if block.is_none_or(|b| b.path.is_none()) {
                    return Vec::new();
                }
                if base == "top" {
                    // The root entry the block is in; nothing at the top level.
                    if path.is_empty() {
                        return Vec::new();
                    }
                    path.truncate(1);
                }
                path.extend(steps(rest));
                match path.split_first() {
                    None => {
                        return self
                            .root_value()
                            .map_or_else(Vec::new, |v| Self::keys_of(v, spread));
                    }
                    Some((PathStep::Field(key), path)) => self.value_at(key, path),
                    Some(_) => None,
                }
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

    /// After `&`: `self`, `parent`, `super`, `top`, `root`, `default`, the binds visible at
    /// the cursor and the templates.
    fn reference_starts(&self) -> Vec<CompletionItem> {
        let mut items: Vec<CompletionItem> = ["self", "parent", "super", "top", "root", "default"]
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

    /// After `@`: the keys of the scopes around the cursor, innermost first
    /// (`SPEC.md` 10.5), only those of the types the field may refer to when
    /// the schema says (`#[crol(refers(...))]`).
    fn ids(&self) -> Vec<CompletionItem> {
        let Some(root) = self.root_value() else {
            return Vec::new();
        };
        let schema = self.snapshot.schema_for(self.file);
        // An id key names what the scopes around the value it is a key of
        // have (`SPEC.md` 3.2): the block the cursor is in, or what a path
        // has reached.
        let keyed = match self.cursor.slot {
            Slot::Key => self.cursor.bracket(),
            Slot::Path => self.cursor.current(),
            _ => None,
        };
        let place = keyed
            .or_else(|| self.cursor.value_or_root())
            .and_then(|place| place.path.as_deref())
            .map(|steps| features::data_path_lenient(root, steps));
        let refers = match keyed {
            // The types the keys of a map keyed by ids may name.
            Some(place) => match &place.shape {
                Some(Shape::Type(TypeExpr::Map(key, _))) => refers_of(key),
                _ => None,
            },
            None => self.value_type().and_then(|ty| refers_of(&ty)),
        };
        let names: Option<Vec<&str>> = refers.map(|paths| {
            let schema = self.snapshot.schema_for(self.file).map(|s| s.file);
            paths
                .iter()
                .filter_map(|p| schema.and_then(|s| s.get(p)).map(TypeDef::name))
                .collect()
        });
        let mut items = match place {
            Some(place) if place.steps().len() > 1 => {
                let mut resolver = crol::IdResolver::new(root);
                if let Some(schema) = &schema {
                    resolver = resolver.schema(schema.file);
                    resolver = match (schema.value_mode, schema.root) {
                        (true, Some(root)) => resolver.value_type(root),
                        (false, Some(root)) => resolver.root_type(root),
                        _ => resolver,
                    };
                }
                Self::visible_keys(root, &resolver, &place)
            }
            _ => Self::keys_of(root, false),
        };
        if let Some(names) = names.filter(|n| !n.is_empty()) {
            items.retain(|i| i.detail.as_deref().is_some_and(|d| names.contains(&d)));
        }
        let docs = crate::docs::of_root_keys(self.snapshot, self.file);
        items
            .into_iter()
            .map(|item| match docs.get(&item.label) {
                Some(doc) => documented(item, doc),
                None => item,
            })
            .collect()
    }

    /// The type of the value being written at the cursor.
    fn value_type(&self) -> Option<TypeExpr> {
        match self.cursor.value()?.shape.as_ref()? {
            Shape::Type(ty) => Some(ty.clone()),
            _ => None,
        }
    }

    /// Where a value goes: the values the type there allows.
    fn values(&self) -> Vec<CompletionItem> {
        let (Some(schema), Some(ty)) = (
            self.snapshot.schema_for(self.file).map(|s| s.file),
            self.value_type(),
        ) else {
            return Vec::new();
        };
        let mut items = Vec::new();
        value_items(schema, &ty, &mut items, 0);
        if of_schema_files(schema) && wants_type_path(&self.cursor) {
            for path in type_paths(self.snapshot, self.file) {
                let quoted = crol_eval::quote(&path);
                let mut item = item(path, CompletionItemKind::CLASS, None);
                item.insert_text = Some(quoted);
                items.push(item);
            }
        }
        items
    }

    /// In a bracket of a known type: its fields not written yet.
    fn bracket_fields(&self) -> Vec<CompletionItem> {
        self.cursor
            .bracket()
            .map_or_else(Vec::new, |place| self.fields(place, &self.cursor.written))
    }

    /// After a `.` in a target: the fields there.
    fn path_fields(&self) -> Vec<CompletionItem> {
        self.cursor
            .current()
            .map_or_else(Vec::new, |place| self.fields(place, &[]))
    }

    /// The fields of a place, each with its doc. Accepting one writes the
    /// value it has now, if any, else a placeholder; only its name when a
    /// `:` is there already.
    /// Whether a layer below this document's has the root key of a place.
    /// It looks at the text, since a document being edited rarely evaluates.
    fn defined_below(&self, place: &Place<'_>) -> bool {
        let Some(PathStep::Field(key)) = place.path.as_ref().and_then(|path| path.first()) else {
            return false;
        };
        let snapshot = self.snapshot;
        let holds_this = |layer: &crol_eval::Layer| {
            layer
                .documents
                .iter()
                .any(|(_, file)| Some(*file) == self.file)
        };
        let Some(this) = snapshot.layers.iter().position(holds_this) else {
            return false;
        };
        snapshot.layers[..this]
            .iter()
            .flat_map(|layer| &layer.documents)
            .map(|(_, file)| snapshot.sources.file(*file).text())
            .filter(|text| text.contains(key.as_str()))
            .any(|text| {
                crol_syntax::parse(text).document().entries().any(|entry| {
                    let target = match &entry {
                        ast::AnyEntry::Entry(entry) => entry.target(),
                        ast::AnyEntry::Remove(entry) => entry.target(),
                        _ => None,
                    };
                    target.is_some_and(|target| {
                        let text = target.syntax().text().to_string();
                        text == *key
                            || text.starts_with(&format!("{key}."))
                            || text.starts_with(&format!("{key}<"))
                    })
                })
            })
    }

    fn fields(&self, place: &Place<'_>, written: &[String]) -> Vec<CompletionItem> {
        let (Some(schema), Some(shape)) = (
            self.snapshot.schema_for(self.file).map(|s| s.file),
            place.shape.clone(),
        ) else {
            return Vec::new();
        };
        let fields = schema::fields_of(schema, &shape);
        // A layer above the one that defines a value cannot set a sealed
        // field of it (`SPEC.md` 10.13).
        let sealed = fields.iter().any(|(_, field)| field.sealed) && self.defined_below(place);
        fields
            .iter()
            .filter(|(name, field)| !written.contains(name) && !(sealed && field.sealed))
            .map(|(name, field)| {
                let current = place.path.as_ref().and_then(|path| {
                    let (key, rest) = match path.split_first() {
                        Some((PathStep::Field(key), rest)) => {
                            let mut rest = rest.to_vec();
                            rest.push(PathStep::Field(name.clone()));
                            (key.clone(), rest)
                        }
                        Some(_) => return None,
                        None => (name.clone(), Vec::new()),
                    };
                    self.value_at(&key, &rest)
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
                    &schema::field_doc(field),
                );
                item.insert_text = Some(if self.before_colon {
                    written_name
                } else {
                    format!("{written_name}: {value}")
                });
                item
            })
            .collect()
    }
}

/// The types ids of a type may refer to: for `Id` and `IdOr`, or ids in an
/// option, list or set.
fn refers_of(ty: &TypeExpr) -> Option<Vec<String>> {
    match ty {
        TypeExpr::Id(refers) | TypeExpr::IdOr(refers, _) => Some(refers.clone()),
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
        // The values of the type written in place; ids come after `@`.
        TypeExpr::IdOr(_, inner) => value_items(schema, inner, out, depth + 1),
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
            // A type data writes with no name has no value to offer by one.
            Some(def) if def.name().is_empty() => {}
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
        assert_eq!(
            reference_prefix("&root.a[0].b[\"c\"]."),
            Some((
                "&root.a[0].b[\"c\"].",
                vec!["root", "a", "[0]", "b", "[\"c\"]"]
            ))
        );
        assert_eq!(reference_prefix("&root.a[x]."), None);
        assert_eq!(reference_prefix("x: &self"), None);
        assert_eq!(reference_prefix("x: & + 1."), None);
    }
}
