//! Semantic tokens: what the grammar alone cannot tell apart (`SPEC.md`
//! 12.5).

use crol_eval::StepKind;
use crol_syntax::ast::{self, AstNode};
use crol_syntax::{LineIndex, SyntaxKind, SyntaxNode, SyntaxToken, TextRange};
use lsp_types::{
    SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens, SemanticTokensLegend,
};

use crate::convert::{self, Encoding};

/// The token types, in legend order.
const TYPES: [SemanticTokenType; 5] = [
    SemanticTokenType::PROPERTY,
    SemanticTokenType::TYPE,
    SemanticTokenType::ENUM_MEMBER,
    SemanticTokenType::VARIABLE,
    SemanticTokenType::KEYWORD,
];
const KEY: u32 = 0;
const TYPE: u32 = 1;
const VARIANT: u32 = 2;
const BIND: u32 = 3;
const REFERENCE: u32 = 4;

/// The modifiers, as bits in legend order. `early` is `&`, the value below;
/// `late` every other reference (`SPEC.md` 7.3).
fn modifiers() -> Vec<SemanticTokenModifier> {
    vec![
        SemanticTokenModifier::DECLARATION,
        SemanticTokenModifier::DEFINITION,
        SemanticTokenModifier::MODIFICATION,
        SemanticTokenModifier::new("early"),
        SemanticTokenModifier::new("late"),
    ]
}
const DECLARATION: u32 = 1;
const DEFINITION: u32 = 1 << 1;
const PATCH: u32 = 1 << 2;
const EARLY: u32 = 1 << 3;
const LATE: u32 = 1 << 4;

pub(crate) fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: TYPES.to_vec(),
        token_modifiers: modifiers(),
    }
}

/// The tokens of a document. `operations` are its root operations, for a
/// data-mode document: the keys they target are marked as definitions or
/// patches.
pub(crate) fn tokens(
    text: &str,
    operations: &[(TextRange, StepKind)],
    encoding: Encoding,
) -> SemanticTokens {
    let parse = crol_syntax::parse(text);
    let mut found: Vec<(TextRange, u32, u32)> = Vec::new();
    for node in parse.syntax().descendants() {
        classify(&node, &mut found);
    }
    for (range, kind) in operations {
        let modifier = match kind {
            StepKind::Definition => DEFINITION,
            StepKind::Patch | StepKind::Remove => PATCH,
        };
        // The key the operation's target starts with.
        if let Some(token) = found
            .iter_mut()
            .find(|(r, t, _)| *t == KEY && r.start() == range.start())
        {
            token.2 |= modifier;
        }
    }
    found.sort_by_key(|(r, _, _)| r.start());
    found.dedup_by_key(|(r, _, _)| r.start());

    let index = LineIndex::new(text);
    let mut data = Vec::with_capacity(found.len());
    let (mut line, mut character) = (0, 0);
    for (range, token_type, modifiers) in found {
        let start = convert::position(text, &index, range.start(), encoding);
        let end = convert::position(text, &index, range.end(), encoding);
        if end.line != start.line {
            continue;
        }
        let delta_line = start.line - line;
        let delta_start = if delta_line == 0 {
            start.character - character
        } else {
            start.character
        };
        data.push(SemanticToken {
            delta_line,
            delta_start,
            length: end.character - start.character,
            token_type,
            token_modifiers_bitset: modifiers,
        });
        (line, character) = (start.line, start.character);
    }
    SemanticTokens {
        result_id: None,
        data,
    }
}

fn classify(node: &SyntaxNode, found: &mut Vec<(TextRange, u32, u32)>) {
    let mut push = |token: Option<SyntaxToken>, kind: u32, modifiers: u32| {
        if let Some(token) = token {
            found.push((token.text_range(), kind, modifiers));
        }
    };
    match node.kind() {
        SyntaxKind::TARGET => {
            let key = ast::Target::cast(node.clone()).and_then(|t| t.key());
            push(key.and_then(|k| k.token()), KEY, 0);
        }
        SyntaxKind::BIND => push(
            ast::Bind::cast(node.clone()).and_then(|b| b.name()),
            BIND,
            DECLARATION,
        ),
        SyntaxKind::VALUE_BIND => push(
            ast::ValueBind::cast(node.clone()).and_then(|b| b.name()),
            BIND,
            DECLARATION,
        ),
        SyntaxKind::BLOCK => push(
            ast::Block::cast(node.clone()).and_then(|b| b.type_name()),
            TYPE,
            0,
        ),
        SyntaxKind::NAMED_TUPLE => push(
            ast::NamedTuple::cast(node.clone()).and_then(|t| t.name()),
            VARIANT,
            0,
        ),
        SyntaxKind::UNIT_NAME => push(
            ast::UnitName::cast(node.clone()).and_then(|u| u.name()),
            VARIANT,
            0,
        ),
        SyntaxKind::REFERENCE => {
            let Some(reference) = ast::Reference::cast(node.clone()) else {
                return;
            };
            let (kind, modifiers) = match reference.kind() {
                ast::ReferenceKind::Below => (REFERENCE, EARLY),
                ast::ReferenceKind::Bind(_) => (BIND, LATE),
                ast::ReferenceKind::Default => (REFERENCE, 0),
                _ => (REFERENCE, LATE),
            };
            // `&` and the name, each a token: `&parent.&parent` is four.
            for token in node
                .children_with_tokens()
                .filter_map(|it| it.into_token())
                .filter(|t| !t.kind().is_trivia() && t.kind() != SyntaxKind::DOT)
            {
                push(Some(token), kind, modifiers);
            }
        }
        _ => {}
    }
}
