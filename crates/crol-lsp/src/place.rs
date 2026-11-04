//! Where an offset is in the data: the type the schema expects there and
//! the path of the value (`SPEC.md` 12.5).
//!
//! This works from the tokens before the offset rather than the syntax
//! tree, since an unfinished line is the normal state of a document while
//! completion runs, and the parser's recovery depends on what comes after
//! the cursor: a value not written yet takes the next line's key as its
//! value, and the block after that lands in an error node. Tokens only
//! need the brackets, and go down one bracket at a time, so the depth does
//! not matter.

use crol_eval::{Evaluation, FileId, PathStep, ValueKind};
use crol_syntax::SyntaxKind::{self, *};
use crol_syntax::{TextRange, TextSize};

use crate::features;
use crate::schema::{self, Shape};
use crate::workspace::{DocumentSchema, Snapshot};

/// A place in the data.
#[derive(Clone, Default)]
pub(crate) struct Place<'a> {
    /// What the schema says the place holds, if it says.
    pub(crate) shape: Option<Shape<'a>>,
    /// The path from the root, if the place has one.
    pub(crate) path: Option<Vec<PathStep>>,
    /// The doc of the field the place is in: the last one stepped into, also
    /// from an element of its list or a payload in it. It may be empty.
    pub(crate) doc: Option<String>,
}

/// What the schema and the latest evaluation say about a document.
pub(crate) struct Scope<'a> {
    schema: Option<DocumentSchema<'a>>,
    evaluation: &'a Evaluation,
}

impl<'a> Scope<'a> {
    pub(crate) fn new(snapshot: &'a Snapshot, file: Option<FileId>) -> Self {
        Self {
            schema: snapshot.schema_for(file),
            evaluation: features::evaluation_for(snapshot, file),
        }
    }

    /// The document as a whole: a value of the root type, or in data whose
    /// root type is that of each root value, root keys. Everything below
    /// follows from this place (`SPEC.md` 4.1).
    fn root(&self) -> Place<'a> {
        Place {
            shape: self.schema.as_ref().map(|schema| {
                if schema.is_whole_value() {
                    Shape::Type(schema::root_expr(schema))
                } else {
                    Shape::Keys
                }
            }),
            path: Some(Vec::new()),
            doc: None,
        }
    }

    /// A place of an enum type, as the variant of the value there; serde
    /// has no other way to say which fields a path goes through.
    fn narrowed(&self, place: Place<'a>) -> Place<'a> {
        let (Some(schema), Some(Shape::Type(_)), Some(path)) =
            (&self.schema, &place.shape, &place.path)
        else {
            return place;
        };
        if !place
            .shape
            .as_ref()
            .is_some_and(|s| schema::is_enum(schema.file, s))
        {
            return place;
        }
        let shape = self
            .value_name(path)
            .and_then(|name| schema::named_shape(schema.file, place.shape.as_ref(), &name));
        match shape {
            Some(shape) => Place {
                shape: Some(shape),
                ..place
            },
            None => place,
        }
    }

    /// The type name of the value at `path`, in the base context.
    fn value_name(&self, path: &[PathStep]) -> Option<String> {
        let (PathStep::Field(key), rest) = path.split_first()? else {
            return None;
        };
        let value = features::value_in(self.evaluation, key, rest, &[])?;
        match value.kind {
            ValueKind::Keyed(keyed) => keyed.name,
            ValueKind::NamedTuple(name, _) | ValueKind::UnitName(name) => Some(name),
            _ => None,
        }
    }

    /// The place one step further in.
    fn step(&self, place: &Place<'a>, step: PathStep) -> Place<'a> {
        let place = self.narrowed(place.clone());
        let path = place.path.as_ref().map(|path| {
            let mut path = path.clone();
            path.push(step.clone());
            path
        });
        let stepped = match (&self.schema, &place.shape) {
            (Some(schema), Some(Shape::Keys)) => {
                let root = Shape::Type(schema::root_expr(schema));
                let at_key = Place {
                    shape: Some(root),
                    path: path.clone(),
                    doc: None,
                };
                // A root value is its variant of the root type.
                let shape = self.narrowed(at_key).shape;
                shape.map(|shape| (shape, None))
            }
            (Some(schema), Some(shape)) => schema::step_shape(schema.file, shape, &step),
            _ => None,
        };
        let (shape, doc) = match stepped {
            // An element or a map's value is still in the field around it.
            Some((shape, doc)) => (Some(shape), doc.or(place.doc)),
            None => (None, None),
        };
        Place { shape, path, doc }
    }

    /// The block `Name { ... }` at a place.
    fn named(&self, place: &Place<'a>, name: &str) -> Place<'a> {
        let shape = self
            .schema
            .as_ref()
            .and_then(|s| schema::named_shape(s.file, place.shape.as_ref(), name));
        Place {
            shape,
            path: place.path.clone(),
            doc: place.doc.clone(),
        }
    }

    /// The element `index` of `(a, b)`, which is the place itself in
    /// parentheses around one value.
    fn group_element(&self, place: &Place<'a>, index: usize) -> Place<'a> {
        let tuple = self.schema.as_ref().is_some_and(|s| {
            place
                .shape
                .as_ref()
                .is_some_and(|shape| schema::is_tuple(s.file, shape))
        });
        if tuple || index > 0 {
            self.step(place, PathStep::Element(index))
        } else {
            place.clone()
        }
    }

    /// The argument `index` of `Name(a, b)`.
    fn argument(&self, place: &Place<'a>, name: &str, index: usize) -> Place<'a> {
        let shape = self.schema.as_ref().and_then(|s| {
            schema::payload(s.file, place.shape.as_ref(), name, index).map(Shape::Type)
        });
        let path = place.path.as_ref().map(|path| {
            let mut path = path.clone();
            path.push(PathStep::Element(index));
            path
        });
        Place {
            shape,
            path,
            doc: place.doc.clone(),
        }
    }
}

/// What a bracket holds.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bracket {
    /// The document.
    Root,
    /// `{ ... }`, with entries.
    Block,
    /// The `{ ... }` of a variant block, at the place of the block around.
    Variant,
    /// `[ ... ]`
    List,
    /// `( ... )`
    Group,
    /// The `( ... )` of `Name( ... )`.
    Args,
}

impl Bracket {
    fn has_entries(self) -> bool {
        matches!(self, Bracket::Root | Bracket::Block | Bracket::Variant)
    }
}

/// How far a bracket's current entry or element is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Progress {
    /// Before a key.
    Key,
    /// In a target, before the `:`.
    Target,
    /// After the `:`, or in an element; `started` once a value is there
    /// that an operator does not continue.
    Value { started: bool },
}

#[derive(Clone)]
struct Frame<'a> {
    bracket: Bracket,
    /// The place the bracket holds.
    place: Place<'a>,
    progress: Progress,
    /// The place of the current entry's value, or the current element.
    current: Place<'a>,
    /// The element the bracket is at.
    index: usize,
    /// The `Name` of `Name( ... )`.
    name: String,
    /// The key of the current entry.
    key: Option<(String, TextRange)>,
    /// The keys written in the bracket.
    written: Vec<String>,
    /// After a `.` in a target.
    dot: bool,
    /// In `<...>`.
    condition: bool,
    /// Right after the `>` of a variant.
    after_condition: bool,
    /// After `-` before a key.
    removing: bool,
}

impl<'a> Frame<'a> {
    fn new(bracket: Bracket, place: Place<'a>) -> Self {
        Self {
            bracket,
            current: place.clone(),
            place,
            progress: Progress::Key,
            index: 0,
            name: String::new(),
            key: None,
            written: Vec::new(),
            dot: false,
            condition: false,
            after_condition: false,
            removing: false,
        }
    }
}

/// What is written at an offset.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Slot {
    /// A key of the innermost bracket.
    Key,
    /// The field after a `.` in a target.
    Path,
    /// A value.
    Value,
    /// Nothing the schema has a say in.
    None,
}

/// An offset in a document, and the brackets around it.
pub(crate) struct Cursor<'a> {
    frames: Vec<Frame<'a>>,
    /// What is written at the offset.
    pub(crate) slot: Slot,
    /// The keys of the innermost bracket, on both sides of the offset;
    /// not the one at the offset.
    pub(crate) written: Vec<String>,
}

impl<'a> Cursor<'a> {
    /// The place the innermost bracket holds.
    pub(crate) fn bracket(&self) -> Option<&Place<'a>> {
        self.frames.last().map(|f| &f.place)
    }

    /// The place of what the slot writes: of the value being written, or
    /// the place a path has reached.
    pub(crate) fn current(&self) -> Option<&Place<'a>> {
        self.frames.last().map(|f| &f.current)
    }

    /// The place of the value being written, even with a part of it there.
    pub(crate) fn value(&self) -> Option<&Place<'a>> {
        self.frames
            .last()
            .filter(|f| matches!(f.progress, Progress::Value { .. }))
            .map(|f| &f.current)
    }

    /// The place of a value at the offset: as [`Cursor::value`], or the
    /// document's, for the value that is the whole of it.
    pub(crate) fn value_or_root(&self) -> Option<&Place<'a>> {
        self.value().or_else(|| {
            self.frames
                .last()
                .filter(|f| f.bracket == Bracket::Root)
                .map(|f| &f.place)
        })
    }

    /// The `Name` of the `Name( ... )` the offset is directly in.
    pub(crate) fn call(&self) -> Option<&str> {
        self.frames
            .last()
            .filter(|f| f.bracket == Bracket::Args)
            .map(|f| f.name.as_str())
    }

    /// The blocks around the offset, innermost first, with the document
    /// last when it has no block of its own: what `&self`, `&parent` and
    /// so on name.
    pub(crate) fn blocks(&self) -> Vec<&Place<'a>> {
        let braces = self.frames.get(1).is_some_and(|f| {
            f.bracket == Bracket::Block && f.place.path.as_ref().is_some_and(Vec::is_empty)
        });
        self.frames[usize::from(braces)..]
            .iter()
            .rev()
            .filter(|f| matches!(f.bracket, Bracket::Root | Bracket::Block))
            .map(|f| &f.place)
            .collect()
    }
}

/// Where `offset` is in `text`. Everything from `offset` on only counts
/// for what the innermost bracket has written.
pub(crate) fn locate<'a>(scope: &Scope<'a>, text: &str, offset: TextSize) -> Cursor<'a> {
    let mut walker = Walker {
        scope,
        text,
        tokens: significant_tokens(text),
        frames: vec![Frame::new(Bracket::Root, scope.root())],
        offset,
        skip_to: 0,
        depth: None,
        written: Vec::new(),
    };
    let mut cursor = None;
    for i in 0..walker.tokens.len() {
        if cursor.is_none() && walker.tokens[i].range.start() >= offset {
            cursor = Some(walker.cursor(i));
            walker.depth = Some(walker.frames.len());
        }
        if walker.depth.is_some_and(|d| walker.frames.len() < d) {
            break;
        }
        if i >= walker.skip_to {
            walker.token(i);
        }
    }
    let end = walker.tokens.len();
    let mut cursor = cursor.unwrap_or_else(|| {
        walker.depth = Some(walker.frames.len());
        walker.cursor(end)
    });
    // Still open at the end of the text.
    if let Some(depth) = walker.depth
        && let Some(frame) = walker.frames.get(depth - 1)
    {
        walker.written = frame.written.clone();
    }
    cursor.written = walker.written;
    cursor
}

#[derive(Clone, Copy)]
struct Tok {
    kind: SyntaxKind,
    range: TextRange,
    /// Whether a line ends between the token before it and this one.
    on_new_line: bool,
}

/// The tokens that are not trivia, without the attributes.
fn significant_tokens(text: &str) -> Vec<Tok> {
    let mut tokens = Vec::new();
    let mut start = TextSize::new(0);
    let mut on_new_line = false;
    let mut attribute = None::<usize>;
    for token in crol_syntax::tokenize(text).0 {
        let range = TextRange::at(start, token.len);
        start += token.len;
        if token.kind.is_trivia() {
            on_new_line |= text[range].contains('\n');
            continue;
        }
        match (attribute, token.kind) {
            (None, POUND) => attribute = Some(0),
            (Some(depth), L_BRACK) => attribute = Some(depth + 1),
            (Some(1), R_BRACK) => attribute = None,
            (Some(depth), R_BRACK) => attribute = Some(depth - 1),
            (Some(_), _) => {}
            (None, kind) => {
                tokens.push(Tok {
                    kind,
                    range,
                    on_new_line,
                });
                on_new_line = false;
            }
        }
    }
    tokens
}

/// Whether a token ends with an operand still to come.
fn expects_operand(kind: SyntaxKind) -> bool {
    matches!(kind, PLUS | MINUS | STAR | SLASH | PERCENT | TILDE | PIPE)
}

/// Whether a token can be the key of an entry.
fn is_key(kind: SyntaxKind) -> bool {
    matches!(kind, IDENT | STRING | RAW_STRING | INT | TRUE_KW | FALSE_KW)
}

struct Walker<'s, 'a> {
    scope: &'s Scope<'a>,
    text: &'s str,
    tokens: Vec<Tok>,
    frames: Vec<Frame<'a>>,
    offset: TextSize,
    /// Tokens before this one are taken care of.
    skip_to: usize,
    /// How many frames there are at the offset, once past it.
    depth: Option<usize>,
    /// The keys of the bracket the offset is in, once it is closed.
    written: Vec<String>,
}

impl<'a> Walker<'_, 'a> {
    fn kind(&self, i: usize) -> Option<SyntaxKind> {
        self.tokens.get(i).map(|t| t.kind)
    }

    fn token_text(&self, i: usize) -> &str {
        &self.text[self.tokens[i].range]
    }

    fn top(&mut self) -> &mut Frame<'a> {
        self.frames.last_mut().expect("the root is never closed")
    }

    /// What the state at token `i` means for an offset before it.
    fn cursor(&mut self, i: usize) -> Cursor<'a> {
        let previous = i.checked_sub(1).map(|p| self.tokens[p]);
        let new_line = previous.is_some_and(|p| {
            let end = p.range.end().min(self.offset);
            self.text[TextRange::new(end, self.offset)].contains('\n')
        });
        let operand = previous.is_some_and(|p| expects_operand(p.kind));
        let scope = self.scope;
        let top = self.top();
        let entries = top.bracket.has_entries();
        let slot = match top.progress {
            _ if top.condition => Slot::None,
            Progress::Key => Slot::Key,
            Progress::Target if new_line => Slot::Key,
            Progress::Target if top.dot => Slot::Path,
            Progress::Target => Slot::None,
            Progress::Value { started: false } => Slot::Value,
            Progress::Value { started: true } if new_line && !operand && entries => Slot::Key,
            Progress::Value { started: true } if new_line && !operand => Slot::Value,
            Progress::Value { started: true } => Slot::None,
        };
        if slot == Slot::Path {
            top.current = scope.narrowed(top.current.clone());
        }
        if slot == Slot::Value && matches!(top.progress, Progress::Value { started: true }) {
            self.next_element();
        }
        Cursor {
            frames: self.frames.clone(),
            slot,
            written: Vec::new(),
        }
    }

    /// Takes token `i` into account.
    fn token(&mut self, i: usize) {
        let kind = self.tokens[i].kind;
        match kind {
            R_BRACE => return self.close(&[Bracket::Block, Bracket::Variant]),
            R_BRACK => return self.close(&[Bracket::List]),
            R_PAREN => return self.close(&[Bracket::Group, Bracket::Args]),
            _ => {}
        }
        let on_new_line = self.tokens[i].on_new_line;
        let top = self.top();
        if top.condition {
            match kind {
                R_ANGLE => {
                    top.condition = false;
                    top.after_condition = true;
                    return;
                }
                // The `>` is missing.
                L_BRACE | COMMA => top.condition = false,
                _ => return,
            }
        } else if kind == L_ANGLE {
            // A variant block after an entry without a `,`.
            if on_new_line
                && top.bracket.has_entries()
                && matches!(top.progress, Progress::Value { started: true })
            {
                top.progress = Progress::Key;
            }
            top.condition = true;
            return;
        }
        match self.top().progress {
            Progress::Key => self.key_token(i),
            Progress::Target => self.target_token(i),
            Progress::Value { started } => self.value_token(i, started),
        }
        self.top().after_condition = false;
    }

    fn key_token(&mut self, i: usize) {
        let kind = self.tokens[i].kind;
        let next = self.kind(i + 1);
        let root = self.top().bracket == Bracket::Root;
        match kind {
            PLUS | QUESTION => {}
            MINUS => self.top().removing = true,
            DOT2 => {
                let top = self.top();
                top.current = Place::default();
                top.progress = Progress::Value { started: true };
            }
            // The outer braces of a document, or a variant block.
            L_BRACE => {
                let top = self.top();
                let after_condition = std::mem::take(&mut top.after_condition);
                let bracket = if top.bracket == Bracket::Root && !after_condition {
                    Bracket::Block
                } else {
                    Bracket::Variant
                };
                let place = top.place.clone();
                self.frames.push(Frame::new(bracket, place));
            }
            L_BRACK if self.accessor(i).is_some() => {
                let top_place = self.top().place.clone();
                let (step, after) = self.accessor(i).expect("just checked");
                self.skip_to = after;
                let current = self.scope.step(&top_place, step.clone());
                let name = match &step {
                    PathStep::Field(name) => name.clone(),
                    PathStep::Accessor(i) => i.to_string(),
                    PathStep::Element(i) => i.to_string(),
                };
                let range = self.tokens[i].range;
                self.start_target(current, name, range);
            }
            // A document that is one value.
            L_BRACK | L_PAREN if root => self.start_value(i),
            IDENT if next == Some(COLON_EQ) => {
                let top = self.top();
                top.current = Place::default();
                top.progress = Progress::Value { started: false };
                self.skip_to = i + 2;
            }
            IDENT if next == Some(EQ) => self.skip_to = i + 2,
            IDENT if root && matches!(next, Some(L_BRACE | L_PAREN)) => self.start_value(i),
            kind if is_key(kind) => {
                let text = self.token_text(i);
                let name = features::string_text(text);
                let step = match kind {
                    INT => text
                        .parse()
                        .map_or_else(|_| PathStep::Field(name.clone()), PathStep::Accessor),
                    _ => PathStep::Field(name.clone()),
                };
                let place = self.top().place.clone();
                let current = self.scope.step(&place, step);
                let range = self.tokens[i].range;
                self.start_target(current, name, range);
            }
            _ => {}
        }
    }

    fn start_target(&mut self, current: Place<'a>, name: String, range: TextRange) {
        let top = self.top();
        top.current = current;
        top.dot = false;
        top.progress = Progress::Target;
        top.key = Some((name, range));
        if top.removing {
            top.removing = false;
            self.write_key();
        }
    }

    /// The value of the bracket as a whole starts at token `i`.
    fn start_value(&mut self, i: usize) {
        let top = self.top();
        top.current = top.place.clone();
        top.progress = Progress::Value { started: false };
        self.value_token(i, false);
    }

    /// `["key"]` or `[1]` at token `i`: the step, and the token after it.
    fn accessor(&self, i: usize) -> Option<(PathStep, usize)> {
        let (first, negative) = match self.kind(i + 1)? {
            MINUS => (i + 2, true),
            _ => (i + 1, false),
        };
        let step = match self.kind(first)? {
            STRING | RAW_STRING if !negative => {
                PathStep::Field(features::string_text(self.token_text(first)))
            }
            INT => {
                let index: i128 = self.token_text(first).parse().ok()?;
                PathStep::Accessor(if negative { -index } else { index })
            }
            _ => return None,
        };
        // Closed or not; the next token decides.
        let after = first + 1 + usize::from(self.kind(first + 1) == Some(R_BRACK));
        Some((step, after))
    }

    /// Records the key of the current entry as written, unless it is the
    /// one at the offset.
    fn write_key(&mut self) {
        let offset = self.offset;
        let top = self.top();
        if let Some((key, range)) = top.key.take()
            && !range.contains_inclusive(offset)
        {
            top.written.push(key);
        }
    }

    fn target_token(&mut self, i: usize) {
        let token = self.tokens[i];
        // An unfinished line: `name` or `a.b.` and then the next entry.
        if token.on_new_line && is_key(token.kind) {
            return self.next_entry(i);
        }
        let top = self.top();
        let dot = std::mem::take(&mut top.dot);
        match token.kind {
            DOT => self.top().dot = true,
            COLON => {
                self.write_key();
                self.top().progress = Progress::Value { started: false };
            }
            COMMA => self.top().progress = Progress::Key,
            L_BRACK => {
                if let Some((step, after)) = self.accessor(i) {
                    self.skip_to = after;
                    self.step_current(step);
                }
            }
            IDENT | TRUE_KW | FALSE_KW if dot => {
                let name = self.token_text(i).to_owned();
                self.step_current(PathStep::Field(name));
            }
            INT if dot => {
                if let Ok(index) = self.token_text(i).parse() {
                    self.step_current(PathStep::Element(index));
                }
            }
            // `key { ... }`: the `:` is missing.
            _ => {
                self.top().progress = Progress::Value { started: false };
                self.value_token(i, false);
            }
        }
    }

    fn step_current(&mut self, step: PathStep) {
        let current = self.top().current.clone();
        self.top().current = self.scope.step(&current, step);
    }

    /// Whether a target with a `:` after it starts at token `i`, which no
    /// expression has.
    fn target_with_colon(&self, i: usize) -> bool {
        let first = match self.kind(i) {
            Some(PLUS | QUESTION) => i + 1,
            _ => i,
        };
        if !self.kind(first).is_some_and(is_key) {
            return false;
        }
        let mut n = first + 1;
        loop {
            match self.kind(n) {
                Some(COLON) => return true,
                Some(DOT) if self.kind(n + 1).is_some_and(is_key) => n += 2,
                Some(L_BRACK) => match self.accessor(n) {
                    Some((_, after)) => n = after,
                    None => return false,
                },
                Some(L_ANGLE) => {
                    while !matches!(self.kind(n), Some(R_ANGLE | L_BRACE | COMMA) | None) {
                        n += 1;
                    }
                    if self.kind(n) != Some(R_ANGLE) {
                        return false;
                    }
                    n += 1;
                }
                _ => return false,
            }
        }
    }

    fn next_entry(&mut self, i: usize) {
        let top = self.top();
        top.progress = Progress::Key;
        top.dot = false;
        top.key = None;
        self.key_token(i);
    }

    fn next_element(&mut self) {
        let top = self.top();
        top.index += 1;
        top.progress = Progress::Value { started: false };
        let (bracket, index) = (top.bracket, top.index);
        let place = top.place.clone();
        let name = top.name.clone();
        self.top().current = self.element(bracket, &place, &name, index);
    }

    fn element(&self, bracket: Bracket, place: &Place<'a>, name: &str, index: usize) -> Place<'a> {
        match bracket {
            Bracket::List => self.scope.step(place, PathStep::Accessor(index as i128)),
            Bracket::Group => self.scope.group_element(place, index),
            Bracket::Args => self.scope.argument(place, name, index),
            _ => place.clone(),
        }
    }

    fn value_token(&mut self, i: usize, started: bool) {
        let token = self.tokens[i];
        let previous = i.checked_sub(1).map(|p| self.tokens[p].kind);
        let continues = previous.is_some_and(expects_operand);
        let entries = self.top().bracket.has_entries();
        // No `,` after the value, or no value: the next entry.
        let next_line = started && token.on_new_line && !continues;
        // On a line of its own, `a.b` is a target being written; a value
        // that goes through a path starts with `&`.
        let path = token.on_new_line && token.kind == IDENT && self.kind(i + 1) == Some(DOT);
        if entries
            && (self.target_with_colon(i)
                || path
                || next_line && (is_key(token.kind) || token.kind == DOT2))
        {
            return self.next_entry(i);
        }
        if !entries && next_line && token.kind != COMMA && !expects_operand(token.kind) {
            self.next_element();
        }
        let next = self.kind(i + 1);
        match token.kind {
            COMMA if entries => {
                let top = self.top();
                top.progress = Progress::Key;
                top.key = None;
            }
            COMMA => self.next_element(),
            L_BRACE => self.open_block(None),
            L_BRACK => self.open(Bracket::List, String::new()),
            L_PAREN => self.open(Bracket::Group, String::new()),
            IDENT if next == Some(L_BRACE) => {
                let name = self.token_text(i).to_owned();
                self.skip_to = i + 2;
                self.open_block(Some(&name));
            }
            IDENT if next == Some(L_PAREN) => {
                let name = self.token_text(i).to_owned();
                self.skip_to = i + 2;
                self.open(Bracket::Args, name);
            }
            kind if expects_operand(kind) => {
                self.top().progress = Progress::Value { started: false };
            }
            _ => self.top().progress = Progress::Value { started: true },
        }
    }

    fn open_block(&mut self, name: Option<&str>) {
        let current = self.top().current.clone();
        let place = match name {
            Some(name) => self.scope.named(&current, name),
            None => self.scope.narrowed(current),
        };
        self.frames.push(Frame::new(Bracket::Block, place));
    }

    /// Opens a list or parentheses at the current value.
    fn open(&mut self, bracket: Bracket, name: String) {
        let place = self.top().current.clone();
        let mut frame = Frame::new(bracket, place);
        frame.name = name;
        frame.progress = Progress::Value { started: false };
        frame.current = self.element(bracket, &frame.place, &frame.name, 0);
        self.frames.push(frame);
    }

    /// Closes the innermost bracket of the kinds, and those inside it. A
    /// closing bracket that closes nothing is ignored.
    fn close(&mut self, kinds: &[Bracket]) {
        let Some(at) = self.frames.iter().rposition(|f| kinds.contains(&f.bracket)) else {
            return;
        };
        if let Some(depth) = self.depth
            && at < depth
            && self.frames.len() >= depth
        {
            self.written = self.frames[depth - 1].written.clone();
        }
        let closed = self.frames[at].bracket;
        self.frames.truncate(at);
        let top = self.top();
        if closed != Bracket::Variant && matches!(top.progress, Progress::Value { .. }) {
            top.progress = Progress::Value { started: true };
        }
    }
}
