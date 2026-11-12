//! Completion of attribute names: after `#[`, the entry attributes
//! (`SPEC.md` 6.1.2), and after `#![`, the document attributes (`SPEC.md`
//! 4.1). Only those that are no error where they would be written.

use crol::Mode;
use crol_syntax::SyntaxKind::{self, *};
use crol_syntax::ast;
use crol_syntax::{TextRange, TextSize};
use lsp_types::{CompletionItem, CompletionItemKind, Documentation, MarkupContent, MarkupKind};

use crate::place::{Cursor, Slot};

/// An attribute to offer.
struct Offer {
    name: &'static str,
    /// Whether it is written with `( ... )`.
    arguments: bool,
    /// How it is written, as a whole.
    written: &'static str,
    doc: &'static str,
}

const SCOPE: Offer = Offer {
    name: "scope",
    arguments: false,
    written: "#[scope]",
    doc: "Marks the value of the entry or value bind after it as a scope for ids: an \
          `@name` inside it names a key of that value first, and a root key otherwise. \
          Only a keyed value can be a scope.",
};

const TYPE: Offer = Offer {
    name: "type",
    arguments: true,
    written: "#[type(...)]",
    doc: "Gives the value bind after it a type, for tools to check and complete its \
          value with: a Rust type written out, `#[type(HashMap<String, i32>)]`, or a \
          path into a schema type, `#[type(Foo.bar)]`, `#[type(Foo.items.[])]`. The \
          loader ignores it.",
};

const USE: Offer = Offer {
    name: "use",
    arguments: true,
    written: "#![use(...)]",
    doc: "Says which schema type a type name in this document means, by its Rust \
          module path: `#![use(app::net::Config)]`. With `as`, the document writes \
          another name for it: `#![use(app::db::Config as DbConfig)]`. A document \
          can have several.",
};

const SCHEMA: Offer = Offer {
    name: "schema",
    arguments: true,
    written: "#![schema(...)]",
    doc: "Names the schema that tools check this document against: a file by module \
          path, `#![schema(::crol::schema)]`, a URL, \
          `#![schema(\"https://example.org/schema.crol\")]`, or the schema a crate \
          publishes, `#![schema(\"crate:app@2.1\")]`. A document has at most one.",
};

const DOCUMENT_TYPE: Offer = Offer {
    name: "type",
    arguments: true,
    written: "#![type(...)]",
    doc: "Gives the type name of a value document written as entries, as if they \
          were inside `Name { ... }`: `#![type(Config)]`. A document has at most \
          one.",
};

const EXTENDS: Offer = Offer {
    name: "extends",
    arguments: true,
    written: "#![extends(...)]",
    doc: "Names the documents this one is written on top of, by module path: a \
          `.crod` file or a directory, `#![extends(super::defaults::services)]`. \
          Each is a layer below this document, in the order written.",
};

const VARIANT: Offer = Offer {
    name: "variant",
    arguments: true,
    written: "#![variant(...)]",
    doc: "Declares a variant of the data, in a group the application has opened: \
          `#![variant(civ: empire)]`. A document can have several.",
};

/// A token that is not trivia.
#[derive(Clone, Copy)]
struct Tok {
    kind: SyntaxKind,
    range: TextRange,
}

fn significant_tokens(text: &str) -> Vec<Tok> {
    let mut start = TextSize::new(0);
    let mut tokens = Vec::new();
    for token in crol_syntax::tokenize(text).0 {
        let range = TextRange::at(start, token.len);
        start += token.len;
        if !token.kind.is_trivia() {
            tokens.push(Tok {
                kind: token.kind,
                range,
            });
        }
    }
    tokens
}

/// An attribute written in full, with its brackets closed.
struct Written<'a> {
    name: &'a str,
    /// `#![...]` rather than `#[...]`.
    document: bool,
    /// The token after its `]`.
    next: usize,
}

struct Tokens<'a> {
    text: &'a str,
    tokens: Vec<Tok>,
}

impl<'a> Tokens<'a> {
    fn kind(&self, i: usize) -> Option<SyntaxKind> {
        self.tokens.get(i).map(|t| t.kind)
    }

    /// The token after the bracket that closes the one opened before token
    /// `i`. An attribute holds no `#`, so one ends a bracket left open.
    fn closed(&self, mut i: usize, close: SyntaxKind) -> Option<usize> {
        let mut depth = 0usize;
        loop {
            match self.kind(i)? {
                POUND => return None,
                kind if kind == close && depth == 0 => return Some(i + 1),
                L_PAREN | L_BRACK | L_BRACE => depth += 1,
                R_PAREN | R_BRACK | R_BRACE => depth = depth.checked_sub(1)?,
                _ => {}
            }
            i += 1;
        }
    }

    /// The attribute whose `#` is token `i`.
    fn attribute(&self, i: usize) -> Option<Written<'a>> {
        if self.kind(i) != Some(POUND) {
            return None;
        }
        let document = self.kind(i + 1) == Some(BANG);
        let open = i + 1 + usize::from(document);
        if self.kind(open) != Some(L_BRACK) || self.kind(open + 1) != Some(IDENT) {
            return None;
        }
        Some(Written {
            name: &self.text[self.tokens[open + 1].range],
            document,
            next: self.closed(open + 2, R_BRACK)?,
        })
    }

    /// The attributes written in a row from token `i`, and the token after
    /// them.
    fn attributes_from(&self, mut i: usize) -> (Vec<Written<'a>>, usize) {
        let mut attributes = Vec::new();
        while let Some(attribute) = self.attribute(i) {
            i = attribute.next;
            attributes.push(attribute);
        }
        (attributes, i)
    }

    /// The token after the attribute at the cursor, whose name ends at
    /// token `i`: after its `( ... )` and its `]`, as far as they are there.
    fn after_name(&self, mut i: usize) -> usize {
        if self.kind(i) == Some(L_PAREN)
            && let Some(next) = self.closed(i + 1, R_PAREN)
        {
            i = next;
        }
        if self.kind(i) == Some(R_BRACK) {
            i += 1;
        }
        i
    }
}

/// What comes after the attributes of an entry.
#[derive(PartialEq, Eq)]
enum Follows {
    /// Nothing yet: the end of the block or the text, or a name alone.
    Nothing,
    /// `name := ...`
    ValueBind,
    /// An entry that sets a value: `key:`, `+key:`, `?key:`, `^key:`.
    Entry,
    /// What takes no attribute: a remove, a spread, a variant block.
    Other,
}

fn is_key(kind: SyntaxKind) -> bool {
    matches!(kind, IDENT | STRING | RAW_STRING | INT | TRUE_KW | FALSE_KW)
}

fn follows(tokens: &Tokens<'_>, i: usize) -> Follows {
    let end = |kind: Option<SyntaxKind>| matches!(kind, None | Some(COMMA | R_BRACE));
    match (tokens.kind(i), tokens.kind(i + 1)) {
        (first, _) if end(first) => Follows::Nothing,
        (Some(IDENT), Some(COLON_EQ)) => Follows::ValueBind,
        // A bind names its entry, and an attribute comes before neither.
        (Some(IDENT), Some(EQ)) => Follows::Other,
        (Some(first), second) if is_key(first) && end(second) => Follows::Nothing,
        (Some(first), _) if is_key(first) => Follows::Entry,
        (Some(PLUS | QUESTION | CARET), _) => Follows::Entry,
        _ => Follows::Other,
    }
}

/// The attribute names to offer for the word at `word`, if it is where the
/// name of an attribute goes: right after `#[` or `#![`. `None` anywhere
/// else. `cursor` is the place of the word.
pub(crate) fn complete(
    text: &str,
    word: TextRange,
    cursor: &Cursor<'_>,
    mode: Mode,
) -> Option<Vec<CompletionItem>> {
    let tokens = Tokens {
        text,
        tokens: significant_tokens(text),
    };
    // The `[` before the word, and the name if a part of it is written.
    let name = tokens
        .tokens
        .partition_point(|t| t.range.end() <= word.start());
    let open = name.checked_sub(1)?;
    if tokens.kind(open) != Some(L_BRACK) {
        return None;
    }
    let document = match (
        open.checked_sub(1).and_then(|i| tokens.kind(i)),
        open.checked_sub(2).and_then(|i| tokens.kind(i)),
    ) {
        (Some(POUND), _) => false,
        (Some(BANG), Some(POUND)) => true,
        _ => return None,
    };
    let pound = open - 1 - usize::from(document);
    let named = tokens
        .tokens
        .get(name)
        .is_some_and(|t| t.kind == IDENT && t.range.start() == word.start());
    let after_name = name + usize::from(named);
    let arguments = tokens.kind(after_name) == Some(L_PAREN);
    let (after, rest) = tokens.attributes_from(tokens.after_name(after_name));

    // The attributes in a row before this one.
    let mut before = Vec::new();
    let mut i = 0;
    while i < pound {
        match tokens.attribute(i) {
            Some(attribute) if attribute.next <= pound => {
                i = attribute.next;
                before.push(attribute);
            }
            _ => {
                before.clear();
                i += 1;
            }
        }
    }
    let has = |name: &str, of_document: bool| {
        before
            .iter()
            .chain(&after)
            .any(|a| a.name == name && a.document == of_document)
    };

    let mut offers = Vec::new();
    if document {
        // Document attributes come before the first entry or expression.
        let mut i = 0;
        while let Some(attribute) = tokens
            .attribute(i)
            .filter(|a| a.document && a.next <= pound)
        {
            i = attribute.next;
        }
        let first = i == pound;
        if !first {
            return Some(Vec::new());
        }
        offers.push(&USE);
        if !has("schema", true) {
            offers.push(&SCHEMA);
        }
        match mode {
            Mode::Value => {
                if !has("type", true) && takes_type_name(&tokens, rest) {
                    offers.push(&DOCUMENT_TYPE);
                }
            }
            Mode::Data => offers.extend([&EXTENDS, &VARIANT]),
        }
    } else {
        if cursor.slot != Slot::Key {
            return Some(Vec::new());
        }
        let follows = follows(&tokens, rest);
        if !has("scope", false) && follows != Follows::Other {
            offers.push(&SCOPE);
        }
        // A variant block has no value binds.
        if !has("type", false)
            && matches!(follows, Follows::Nothing | Follows::ValueBind)
            && !cursor.in_variant_block()
        {
            offers.push(&TYPE);
        }
    }
    Some(offers.into_iter().map(|o| item(o, arguments)).collect())
}

/// Whether the document that starts at token `i`, after its attributes, can
/// have a `#![type(...)]`: entries, in braces or not, with no type name.
fn takes_type_name(tokens: &Tokens<'_>, i: usize) -> bool {
    let Some(first) = tokens.tokens.get(i) else {
        return true;
    };
    let rest = &tokens.text[usize::from(first.range.start())..];
    match crol_syntax::parse(rest).document().expr() {
        None => true,
        Some(ast::Expr::Block(block)) => block.type_name().is_none(),
        Some(_) => false,
    }
}

/// The item of an attribute. With `arguments`, its `(` is written already.
fn item(offer: &Offer, arguments: bool) -> CompletionItem {
    let insert = (offer.arguments && !arguments).then(|| format!("{}()", offer.name));
    CompletionItem {
        label: offer.name.to_owned(),
        kind: Some(CompletionItemKind::KEYWORD),
        detail: Some(offer.written.to_owned()),
        insert_text: insert,
        documentation: Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: offer.doc.to_owned(),
        })),
        ..Default::default()
    }
}
