//! Renaming keys and the ids that name them (`SPEC.md` 12.5).
//!
//! A key is a place in the evaluated data. What names it in the documents
//! is found by what it resolves to there, not by its text: the targets of
//! the operations on the place, the references whose path goes through it,
//! and the ids that resolve to it where their value ends up (`SPEC.md`
//! 3.2). The rename is refused where one written name would have to change
//! for one place and stay for another, and where it would change what an
//! id names. What is left is checked by evaluating the edited documents:
//! the data must be what it was, with the one key renamed.

use std::collections::HashMap;

use crol::{DataPath, DataStep, ResolvedId};
use crol_eval::{
    Context, DataOptions, Evaluation, FileId, IdPath, Key, Layer, PathStep, Sources, Span, Value,
    ValueKind,
};
use crol_syntax::ast::{self, AstNode};
use crol_syntax::{LineIndex, SyntaxKind, SyntaxNode, SyntaxToken, TextRange};
use lsp_types::{TextEdit, Uri, WorkspaceEdit};

use crate::convert::{self, Encoding};
use crate::workspace::Snapshot;

// ---- Names as they are written ----

/// How a name is written where it can be renamed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// The key of a target or the first key of an id: bare, or a string.
    Name,
    /// `.name`, with where its `.` is: a name that is no identifier is
    /// written `["name"]` instead.
    Field(TextRange),
    /// The string of `["name"]`.
    Accessor,
}

/// A string key as written in a target, a reference or an id.
#[derive(Debug, Clone)]
struct Segment {
    /// The key it writes.
    name: String,
    /// The token that writes it.
    range: TextRange,
    form: Form,
}

impl Segment {
    fn new(token: &SyntaxToken, form: Form) -> Option<Segment> {
        let name = match token.kind() {
            SyntaxKind::IDENT => token.text().to_owned(),
            // A string literal, decoded as an id's is.
            SyntaxKind::STRING => IdPath::parse(&format!("@{}", token.text()))
                .ok()?
                .name()?
                .to_owned(),
            SyntaxKind::RAW_STRING => crate::features::string_text(token.text()),
            _ => return None,
        };
        Some(Segment {
            name,
            range: token.text_range(),
            form,
        })
    }

    /// The edit that writes `new` here instead (`SPEC.md` 3.1, 5.1).
    fn edit(&self, new: &str) -> (TextRange, String) {
        let plain = crol_eval::is_identifier(new);
        match self.form {
            Form::Name | Form::Field(_) if plain => (self.range, new.to_owned()),
            Form::Name | Form::Accessor => (self.range, crol_eval::quote(new)),
            Form::Field(dot) => (
                dot.cover(self.range),
                format!("[{}]", crol_eval::quote(new)),
            ),
        }
    }
}

/// A step of a written path, and how it is written if it is a string key.
#[derive(Debug, Clone)]
struct Step {
    step: PathStep,
    segment: Option<Segment>,
}

impl Step {
    fn key(segment: Segment) -> Step {
        Step {
            step: PathStep::Field(segment.name.clone()),
            segment: Some(segment),
        }
    }
}

fn accessor_step(accessor: &ast::Accessor) -> Option<Step> {
    let token = accessor.key()?;
    if token.kind() == SyntaxKind::INT {
        let index = token.text().parse::<i128>().ok()?;
        let index = if accessor.is_negative() {
            -index
        } else {
            index
        };
        return Some(Step {
            step: PathStep::Accessor(index),
            segment: None,
        });
    }
    Segment::new(&token, Form::Accessor).map(Step::key)
}

fn key_step(key: &ast::Key) -> Option<Step> {
    if let Some(accessor) = key.accessor() {
        return accessor_step(&accessor);
    }
    let token = key.token()?;
    if token.kind() == SyntaxKind::INT {
        return Some(Step {
            step: PathStep::Accessor(token.text().parse().ok()?),
            segment: None,
        });
    }
    Segment::new(&token, Form::Name).map(Step::key)
}

/// The steps selectors take, up to one that cannot be followed. A variant
/// condition is no step.
fn selector_steps(selectors: impl Iterator<Item = ast::Selector>) -> Vec<Step> {
    let mut out = Vec::new();
    for selector in selectors {
        let step = match &selector {
            ast::Selector::Variant(_) => continue,
            ast::Selector::Field(field) => field.name().and_then(|name| {
                let dot = field.syntax().first_token()?;
                Segment::new(&name, Form::Field(dot.text_range())).map(Step::key)
            }),
            ast::Selector::Index(index) => index.index().and_then(|token| {
                Some(Step {
                    step: PathStep::Element(token.text().parse().ok()?),
                    segment: None,
                })
            }),
            ast::Selector::Accessor(accessor) => accessor_step(accessor),
            ast::Selector::Parent(_) => None,
        };
        match step {
            Some(step) => out.push(step),
            None => break,
        }
    }
    out
}

/// A path as written in a target or after a reference.
struct Written {
    /// The place it starts from; `None` where that is not one place, as
    /// in a template.
    base: Option<Vec<PathStep>>,
    steps: Vec<Step>,
    /// Whether it is a target: an operation on what it names.
    target: bool,
}

fn written_target(target: &ast::Target) -> Option<Written> {
    let mut steps = vec![key_step(&target.key()?)?];
    steps.extend(selector_steps(target.selectors()));
    Some(Written {
        base: crate::features::place_path(&target.syntax().parent()?),
        steps,
        target: true,
    })
}

/// The place a reference names before its selectors (`SPEC.md` 6.1), where
/// that can be read off the document.
fn reference_base(reference: &ast::Reference) -> Option<Vec<PathStep>> {
    let inside = |kind| reference.syntax().ancestors().any(|n| n.kind() == kind);
    let blocks: Vec<ast::Block> = reference
        .syntax()
        .ancestors()
        .filter_map(ast::Block::cast)
        .collect();
    // The place of the block `up` blocks out; past the last, the root.
    let block = |up: usize| match blocks.get(up) {
        Some(block) => crate::features::place_path(block.syntax()),
        None if up == blocks.len() && !inside(SyntaxKind::VALUE_BIND) => Some(Vec::new()),
        None => None,
    };
    let entry_path = |entry: ast::Entry| {
        let mut path = crate::features::place_path(&entry.syntax().parent()?)?;
        let target = written_target(&entry.target()?)?;
        path.extend(target.steps.into_iter().map(|s| s.step));
        Some(path)
    };
    match reference.kind() {
        ast::ReferenceKind::Root => Some(Vec::new()),
        ast::ReferenceKind::SelfValue => block(0),
        ast::ReferenceKind::Parent(up) => block(up),
        ast::ReferenceKind::Top => Some(vec![block(0)?.into_iter().next()?]),
        ast::ReferenceKind::Bind(name) => {
            let declared = crate::navigate::local_declaration(&name)?;
            entry_path(declared.parent()?.parent().and_then(ast::Entry::cast)?)
        }
        ast::ReferenceKind::Below => {
            // In an element, the value below is an element too.
            let entry = reference
                .syntax()
                .ancestors()
                .find_map(|node| match node.kind() {
                    SyntaxKind::LIST_EXPR | SyntaxKind::TUPLE_EXPR | SyntaxKind::ARG_LIST => {
                        Some(None)
                    }
                    _ => ast::Entry::cast(node).map(Some),
                })??;
            entry_path(entry)
        }
        ast::ReferenceKind::Default => None,
    }
}

fn written_reference(path: &ast::PathExpr) -> Option<Written> {
    let Some(ast::Expr::Reference(reference)) = path.base() else {
        return None;
    };
    Some(Written {
        base: reference_base(&reference),
        steps: selector_steps(path.selectors()),
        target: false,
    })
}

/// The keys of the id written at `node`: `a` and `b` of `@a.b`.
fn id_segments(node: &SyntaxNode) -> Option<Vec<Segment>> {
    let (id, selectors) = match ast::PathExpr::cast(node.clone()) {
        Some(path) => match path.base()? {
            ast::Expr::Id(id) => (id, selector_steps(path.selectors())),
            _ => return None,
        },
        None => (ast::IdExpr::cast(node.clone())?, Vec::new()),
    };
    let mut out = vec![Segment::new(&id.name()?, Form::Name)?];
    out.extend(selectors.into_iter().map_while(|step| step.segment));
    Some(out)
}

/// What can be renamed at the cursor.
enum At {
    /// A key of an id: `filter` in `@settings.filter` is its second.
    Id { node: SyntaxNode, index: usize },
    /// A key of a target or of a reference's path, with the path up to it.
    Path {
        path: Option<Vec<PathStep>>,
        name: String,
    },
}

fn at(token: &SyntaxToken) -> Option<(At, TextRange)> {
    let range = token.text_range();
    if let Some(node) = crate::features::id_node(token)
        && let Some(index) =
            id_segments(&node).and_then(|segments| segments.iter().position(|s| s.range == range))
    {
        return Some((At::Id { node, index }, range));
    }
    let written = match token.parent_ancestors().find_map(ast::Target::cast) {
        Some(target) => written_target(&target),
        None => token
            .parent_ancestors()
            .find_map(ast::PathExpr::cast)
            .and_then(|path| written_reference(&path)),
    }?;
    let index = written
        .steps
        .iter()
        .position(|s| s.segment.as_ref().is_some_and(|s| s.range == range))?;
    let name = written.steps[index].segment.as_ref()?.name.clone();
    let path = written.base.map(|mut path| {
        path.extend(written.steps.into_iter().take(index + 1).map(|s| s.step));
        path
    });
    Some((At::Path { path, name }, range))
}

/// The range of the key at a token, if it is one that can be renamed: a
/// string key of a target or of a reference's path, or a key of an id.
pub(crate) fn prepare(token: &SyntaxToken) -> Option<TextRange> {
    at(token).map(|(_, range)| range)
}

// ---- Places in the data ----

fn key(name: &str) -> DataStep {
    DataStep::Key(Key::Str(name.into()))
}

fn step_into<'v>(value: &'v Value, step: &DataStep) -> Option<&'v Value> {
    DataPath::new([step.clone()]).value_in(value)
}

/// The place written steps reach in `root`, if the data has every one:
/// `[3]` is an index on a list and a key elsewhere, and a step goes into
/// the element of a named tuple of one (`SPEC.md` 5.1).
fn place_of(root: &Value, steps: &[PathStep]) -> Option<Vec<DataStep>> {
    let mut out = Vec::new();
    let mut value = root;
    for step in steps {
        while let ValueKind::NamedTuple(_, items) = &value.kind
            && let [item] = items.as_slice()
            && !matches!(step, PathStep::Element(_))
        {
            out.push(DataStep::Element(0));
            value = item;
        }
        let next = match (step, &value.kind) {
            (PathStep::Field(name), _) => key(name),
            (PathStep::Element(i), _) => DataStep::Element(*i),
            (PathStep::Accessor(i), ValueKind::List(_)) => {
                DataStep::Index(usize::try_from(*i).ok()?)
            }
            (PathStep::Accessor(i), _) => DataStep::Key(Key::Int(*i)),
        };
        value = step_into(value, &next)?;
        out.push(next);
    }
    Some(out)
}

fn value_mut<'v>(root: &'v mut Value, steps: &[DataStep]) -> Option<&'v mut Value> {
    let mut value = root;
    for step in steps {
        value = match (step, &mut value.kind) {
            (DataStep::Key(key), ValueKind::Keyed(keyed)) => keyed.entries.get_mut(key)?,
            (DataStep::Index(i), ValueKind::List(items)) => items.get_mut(*i)?,
            (DataStep::Element(i), ValueKind::Tuple(items) | ValueKind::NamedTuple(_, items)) => {
                items.get_mut(*i)?
            }
            _ => return None,
        };
    }
    Some(value)
}

fn show(steps: &[DataStep]) -> String {
    if steps.is_empty() {
        return "the root".to_owned();
    }
    format!("`{}`", DataPath::new(steps.iter().cloned()))
}

/// A rename: the place of the key, and its new name.
struct Rename<'a> {
    place: &'a [DataStep],
    old: &'a str,
    new: &'a str,
}

impl Rename<'_> {
    /// A path of the data as it is after the rename.
    fn path(&self, path: &DataPath) -> DataPath {
        let mut steps = path.steps().to_vec();
        if steps.starts_with(self.place) {
            steps[self.place.len() - 1] = key(self.new);
        }
        DataPath::new(steps)
    }

    /// Which key of an id names the place: the second of `@settings.filter`
    /// for `settings.filter`, the first for `settings`.
    fn key_of(&self, id: &ResolvedId) -> Option<usize> {
        let target = id.target.as_ref()?.steps();
        // The id's keys are the last steps of what it resolved to.
        let first = target.len().checked_sub(id.id.path().len())?;
        (target.starts_with(self.place) && self.place.len() > first)
            .then(|| self.place.len() - 1 - first)
    }

    fn describe(&self) -> String {
        format!("{} to `{}`", show(self.place), Key::Str(self.new.into()))
    }
}

/// The id `id` with its key `index` renamed.
fn renamed_id(id: &IdPath, index: usize, new: &str) -> Option<IdPath> {
    let mut keys = id.keys().to_vec();
    *keys.get_mut(index)? = new.into();
    IdPath::new(id.is_rooted(), keys)
}

/// Where two values differ, if they are not identical (`SPEC.md` 3.3).
fn difference(a: &Value, b: &Value, path: &mut Vec<DataStep>) -> bool {
    if a.identical(b) {
        return false;
    }
    let items = |a: &[Value], b: &[Value], step: fn(usize) -> DataStep, path: &mut Vec<_>| {
        if a.len() != b.len() {
            return;
        }
        for (i, (x, y)) in a.iter().zip(b).enumerate() {
            path.push(step(i));
            if difference(x, y, path) {
                return;
            }
            path.pop();
        }
    };
    match (&a.kind, &b.kind) {
        (ValueKind::Keyed(x), ValueKind::Keyed(y))
            if x.entries.len() == y.entries.len() && x.entries.keys().eq(y.entries.keys()) =>
        {
            for ((key, v), w) in x.entries.iter().zip(y.entries.values()) {
                path.push(DataStep::Key(key.clone()));
                if difference(v, w, path) {
                    break;
                }
                path.pop();
            }
        }
        (ValueKind::List(x), ValueKind::List(y)) => items(x, y, DataStep::Index, path),
        (ValueKind::Tuple(x), ValueKind::Tuple(y)) => items(x, y, DataStep::Element, path),
        (ValueKind::NamedTuple(n, x), ValueKind::NamedTuple(m, y)) if n == m => {
            items(x, y, DataStep::Element, path)
        }
        _ => {}
    }
    true
}

// ---- The data a document is in ----

/// An id met in the data, and whether it is a key of a map keyed by ids
/// rather than a value (`SPEC.md` 10.5).
struct Met {
    id: ResolvedId,
    map_key: bool,
}

/// The data of one context.
struct Root {
    context: Context,
    value: Value,
    ids: Vec<Met>,
}

/// What a document is evaluated with, and to.
struct World<'a> {
    snapshot: &'a Snapshot,
    file: FileId,
    /// The layers and options of a data document; `None` for a value
    /// document, which is evaluated alone.
    chain: Option<(&'a [Layer], &'a DataOptions)>,
    /// The base, then every other context.
    roots: Vec<Root>,
}

const NOT_EVALUATED: &str =
    "a key cannot be renamed while its document does not evaluate: what names it is not known";
const STALE: &str = "the documents changed since they were last evaluated: try again";

fn roots_of(evaluation: &Evaluation) -> Option<Vec<(Context, Value)>> {
    let base = evaluation.value.as_ref()?;
    let mut roots = vec![(Context::base(), base.clone())];
    for data in &evaluation.contexts {
        if data.context != Context::base() {
            roots.push((data.context.clone(), data.root(base)));
        }
    }
    Some(roots)
}

impl<'a> World<'a> {
    fn new(snapshot: &'a Snapshot, file: FileId) -> Result<Self, String> {
        let roots = snapshot
            .evaluation_of(file)
            .and_then(roots_of)
            .ok_or(NOT_EVALUATED)?;
        let mut world = World {
            snapshot,
            file,
            chain: snapshot.chain_of(file),
            roots: Vec::new(),
        };
        world.roots = roots
            .into_iter()
            .map(|(context, value)| {
                let ids = world.ids(&value);
                Root {
                    context,
                    value,
                    ids,
                }
            })
            .collect();
        Ok(world)
    }

    fn ids(&self, root: &Value) -> Vec<Met> {
        self.snapshot
            .id_resolver_in(self.file, root)
            .ids()
            .into_iter()
            .map(|id| Met {
                map_key: !matches!(
                    id.location.value_in(root).map(|v| &v.kind),
                    Some(ValueKind::Id(_))
                ),
                id,
            })
            .collect()
    }

    /// The documents that make the data.
    fn files(&self) -> Vec<FileId> {
        match self.chain {
            Some((layers, _)) => layers
                .iter()
                .flat_map(|layer| layer.documents.iter().map(|(_, file)| *file))
                .collect(),
            None => vec![self.file],
        }
    }

    /// `name:line:col` of where a span starts, with the file's own name.
    fn whereabouts(&self, span: Span) -> String {
        let location = self.snapshot.sources.location(span);
        let name = location.name.rsplit(['/', '\\']).next().unwrap_or_default();
        format!(
            "{name}:{}:{}",
            location.line_col.line, location.line_col.col
        )
    }

    /// The place of the key at the cursor.
    fn place(&self, at: &At) -> Result<Vec<DataStep>, String> {
        match at {
            At::Path { path: None, name } => Err(format!(
                "`{name}` is written where it has no place of its own, as in a template: \
                 rename the key where the value ends up"
            )),
            At::Path {
                path: Some(path), ..
            } => self
                .roots
                .iter()
                .find_map(|root| place_of(&root.value, path))
                .ok_or_else(|| {
                    let (key, steps) = path.split_first().map_or(("", &[][..]), |(k, s)| match k {
                        PathStep::Field(key) => (key.as_str(), s),
                        _ => ("", s),
                    });
                    format!(
                        "`{}` is not in the evaluated data",
                        crate::features::describe_path(key, steps)
                    )
                }),
            At::Id { node, index } => {
                let span = Span::new(self.file, node.text_range());
                let written = node.text();
                let mut places: Vec<Vec<DataStep>> = Vec::new();
                let mut met = false;
                for id in self.roots.iter().flat_map(|root| &root.ids) {
                    if id.id.span != span || id.map_key {
                        continue;
                    }
                    met = true;
                    let Some(target) = &id.id.target else {
                        continue;
                    };
                    // The keys after this one are the last steps.
                    let after = id.id.id.path().len().saturating_sub(index + 1);
                    let steps = target.steps();
                    let place = steps[..steps.len().saturating_sub(after)].to_vec();
                    if !places.contains(&place) {
                        places.push(place);
                    }
                }
                match places.len() {
                    0 if met => Err(format!("`{written}` names nothing")),
                    0 => Err(format!(
                        "`{written}` is not in the evaluated data, so what it names is not known"
                    )),
                    1 => Ok(places.remove(0)),
                    _ => {
                        let places: Vec<String> = places.iter().map(|p| show(p)).collect();
                        Err(format!(
                            "`{written}` is written once and names {}: renaming it for one \
                             would break the others",
                            places.join(" and ")
                        ))
                    }
                }
            }
        }
    }

    /// The written ids to change, each with the key of it that names the
    /// place. Refuses where one of them also names something else.
    fn ids_to_rename(&self, rename: &Rename<'_>) -> Result<Vec<(Span, usize)>, String> {
        let mut found: Vec<(Span, usize)> = Vec::new();
        let (_, parent) = rename.place.split_last().ok_or(NOT_EVALUATED)?;
        for met in self.roots.iter().flat_map(|root| &root.ids) {
            // The key to rename is itself an id, of a map keyed by ids.
            if met.map_key
                && met.id.location.steps() == parent
                && met.id.id.name() == Some(rename.old)
            {
                return Err(format!(
                    "{} is a key that is an id: renaming keys that are ids is not supported",
                    show(rename.place)
                ));
            }
            let Some(index) = rename.key_of(&met.id) else {
                continue;
            };
            if met.map_key {
                return Err(format!(
                    "the key `{}` of {} is an id that names {}: renaming keys that are ids \
                     is not supported",
                    Key::Str(rename.old.into()),
                    show(met.id.location.steps()),
                    show(rename.place),
                ));
            }
            if !found.contains(&(met.id.span, index)) {
                found.push((met.id.span, index));
            }
        }
        // An id written once, as in a template, is met in each place its
        // value ends up, and may name another key in each (`SPEC.md` 3.2).
        for (span, index) in &found {
            let mut others: Vec<String> = Vec::new();
            for met in self.roots.iter().flat_map(|root| &root.ids) {
                if met.id.span == *span
                    && !met.map_key
                    && rename.key_of(&met.id) != Some(*index)
                    && let Some(target) = &met.id.target
                {
                    let other = show(target.steps());
                    if !others.contains(&other) {
                        others.push(other);
                    }
                }
            }
            if !others.is_empty() {
                let id = self
                    .roots
                    .iter()
                    .flat_map(|root| &root.ids)
                    .find(|m| m.id.span == *span);
                return Err(format!(
                    "`{}` at {} names {} and also {}: it is written once, so renaming {} \
                     would break the others",
                    id.map(|m| m.id.id.to_string()).unwrap_or_default(),
                    self.whereabouts(*span),
                    show(rename.place),
                    others.join(" and "),
                    show(rename.place),
                ));
            }
        }
        Ok(found)
    }

    /// Each context's data as it is to be after the rename: the key
    /// renamed, and the ids that name it rewritten. Refuses where the new
    /// name is taken, and where an id would name another key than before.
    fn renamed(&self, rename: &Rename<'_>, ids: &[(Span, usize)]) -> Result<Vec<Value>, String> {
        let (_, parent) = rename.place.split_last().ok_or(NOT_EVALUATED)?;
        let mut out = Vec::new();
        for root in &self.roots {
            let mut value = root.value.clone();
            for met in root.ids.iter().filter(|met| !met.map_key) {
                if let Some((_, index)) = ids.iter().find(|(span, _)| *span == met.id.span)
                    && let Some(at) = value_mut(&mut value, met.id.location.steps())
                    && let ValueKind::Id(id) = &mut at.kind
                    && let Some(new) = renamed_id(id, *index, rename.new)
                {
                    *id = new;
                }
            }
            if let Some(Value {
                kind: ValueKind::Keyed(keyed),
                ..
            }) = value_mut(&mut value, parent)
            {
                if keyed.has_name(rename.new) {
                    return Err(format!(
                        "{} already has a key `{}`",
                        show(parent),
                        Key::Str(rename.new.into())
                    ));
                }
                let (old, new) = (Key::Str(rename.old.into()), Key::Str(rename.new.into()));
                keyed.entries = std::mem::take(&mut keyed.entries)
                    .into_iter()
                    .map(|(k, v)| (if k == old { new.clone() } else { k }, v))
                    .collect();
                if keyed.defaulted.remove(&old) {
                    keyed.defaulted.insert(new);
                }
            }
            // Every id names what it did, under its new name.
            let after = self.ids(&value);
            if after.len() != root.ids.len() {
                return Err(format!(
                    "renaming {} would change which ids the data has",
                    rename.describe()
                ));
            }
            for (before, after) in root.ids.iter().zip(&after) {
                let expected = before.id.target.as_ref().map(|t| rename.path(t));
                if after.id.target == expected {
                    continue;
                }
                let names = |target: &Option<DataPath>| match target {
                    Some(target) => show(target.steps()),
                    None => "nothing".to_owned(),
                };
                let at = self.whereabouts(before.id.span);
                // An id that is renamed finds the new name nearer by; one
                // that is not finds it where it found another, or nothing.
                return Err(if before.id.id == after.id.id {
                    format!(
                        "renaming {} would make `{}` at {at} name {} instead of {}: \
                         the new name would shadow the key it names",
                        rename.describe(),
                        before.id.id,
                        names(&after.id.target),
                        names(&before.id.target),
                    )
                } else {
                    format!(
                        "renaming {} would make `{}` at {at} name {} as `{}`, not {}: \
                         a nearer scope has a key of the new name, which shadows it",
                        rename.describe(),
                        before.id.id,
                        names(&after.id.target),
                        after.id.id,
                        names(&expected),
                    )
                });
            }
            out.push(value);
        }
        Ok(out)
    }

    /// The edits: to the ids, and to every target and reference whose
    /// path goes through the place, in every layer.
    fn edits(
        &self,
        rename: &Rename<'_>,
        ids: &[(Span, usize)],
    ) -> Result<Vec<(FileId, TextRange, String)>, String> {
        let sources = &self.snapshot.sources;
        let mut trees: HashMap<FileId, SyntaxNode> = HashMap::new();
        let mut tree = |file: FileId| -> Option<SyntaxNode> {
            if let std::collections::hash_map::Entry::Vacant(entry) = trees.entry(file) {
                entry.insert(crol_syntax::parse(sources.get(file)?.text()).syntax());
            }
            trees.get(&file).cloned()
        };
        let mut edits: Vec<(FileId, TextRange, String)> = Vec::new();
        for (span, index) in ids {
            let segment = tree(span.file)
                .and_then(|root| {
                    root.descendants().find(|node| {
                        node.text_range() == span.range
                            && matches!(node.kind(), SyntaxKind::ID_EXPR | SyntaxKind::PATH_EXPR)
                    })
                })
                .and_then(|node| id_segments(&node)?.into_iter().nth(*index))
                .filter(|segment| segment.name == rename.old)
                .ok_or_else(|| {
                    format!(
                        "an id that names {} is not written as one, at {}",
                        show(rename.place),
                        self.whereabouts(*span)
                    )
                })?;
            let (range, text) = segment.edit(rename.new);
            edits.push((span.file, range, text));
        }
        // The keys of targets, which are where the place is written.
        let mut keys: Vec<Span> = Vec::new();
        let plain = crol_eval::is_identifier(rename.old);
        for file in self.files() {
            // A plain key is written as it is, so most files need no parsing.
            if plain
                && !sources
                    .get(file)
                    .is_some_and(|f| f.text().contains(rename.old))
            {
                continue;
            }
            let Some(root) = tree(file) else { continue };
            for node in root.descendants() {
                let written = match node.kind() {
                    SyntaxKind::TARGET => ast::Target::cast(node).and_then(|t| written_target(&t)),
                    SyntaxKind::PATH_EXPR => {
                        ast::PathExpr::cast(node).and_then(|p| written_reference(&p))
                    }
                    _ => None,
                };
                let Some(Written {
                    base: Some(mut path),
                    steps,
                    target,
                }) = written
                else {
                    continue;
                };
                for step in steps {
                    path.push(step.step);
                    let Some(segment) = step.segment.filter(|s| s.name == rename.old) else {
                        continue;
                    };
                    let here = (self.roots.iter())
                        .any(|root| place_of(&root.value, &path).as_deref() == Some(rename.place));
                    if here {
                        let (range, text) = segment.edit(rename.new);
                        edits.push((file, range, text));
                        if target {
                            keys.push(Span::new(file, segment.range));
                        }
                    }
                }
            }
        }
        if keys.is_empty() {
            return Err(format!(
                "no entry writes {} at its own place",
                show(rename.place)
            ));
        }
        // A key that a template, a spread or a reference brings in is
        // written for every place it is brought to.
        for root in &self.roots {
            let Some(value) = DataPath::new(rename.place.iter().cloned()).value_in(&root.value)
            else {
                continue;
            };
            if value.span.is_detached() || sources.get(value.span.file).is_none() {
                continue;
            }
            let written = crate::features::entry_key_span(self.snapshot, value.span);
            let text = &sources.file(written.file).text()[written.range];
            let same_key = text == rename.old || text == crol_eval::quote(rename.old);
            if written != value.span && same_key && !keys.contains(&written) {
                return Err(format!(
                    "{} is written at {}, not at its own place, as what a template, a spread \
                     or a reference brings in is: renaming it there would rename it wherever \
                     that value goes",
                    show(rename.place),
                    self.whereabouts(written),
                ));
            }
        }
        edits.sort_by_key(|(file, range, _)| (*file, range.start()));
        edits.dedup();
        if let Some((file, ..)) = edits.iter().find(|(f, ..)| self.snapshot.is_read_only(*f)) {
            return Err(format!(
                "{} is also written in `{}`, which is in a read-only layer",
                show(rename.place),
                sources.file(*file).name()
            ));
        }
        Ok(edits)
    }

    /// Evaluates the documents with the edits, and refuses unless the data
    /// is `expected` in every context.
    fn verify(
        &self,
        rename: &Rename<'_>,
        edits: &[(FileId, TextRange, String)],
        expected: &[Value],
    ) -> Result<(), String> {
        let mut sources = Sources::new();
        for (file, source) in self.snapshot.sources.files() {
            let mut text = source.text().to_owned();
            for (_, range, new) in edits.iter().rev().filter(|(f, ..)| *f == file) {
                text.replace_range(std::ops::Range::<usize>::from(*range), new);
            }
            sources.add(source.name(), text);
        }
        let evaluation = match self.chain {
            Some((layers, options)) => crol_eval::evaluate_data(&sources, layers, options),
            None => crol_eval::evaluate_value(&sources, self.file),
        };
        let before: Vec<&str> = (self.snapshot.evaluation_of(self.file).into_iter())
            .flat_map(|e| &e.diagnostics)
            .filter(|d| d.is_error())
            .map(|d| d.message.as_str())
            .collect();
        let broken = (evaluation.diagnostics.iter())
            .find(|d| d.is_error() && !before.contains(&d.message.as_str()));
        if let Some(diagnostic) = broken {
            return Err(format!(
                "renaming {} would be an error: {}",
                rename.describe(),
                diagnostic.display(&sources)
            ));
        }
        let Some(after) = roots_of(&evaluation) else {
            return Err(format!(
                "renaming {} would leave the data without a value",
                rename.describe()
            ));
        };
        // A context that is as the base has no data of its own.
        let in_context = |roots: &[(Context, Value)], context: &Context| {
            roots
                .iter()
                .position(|(c, _)| c == context)
                .unwrap_or_default()
        };
        let before_at = |context: &Context| {
            (self.roots.iter())
                .position(|root| root.context == *context)
                .unwrap_or_default()
        };
        let contexts = (self.roots.iter().map(|root| &root.context))
            .chain(after.iter().map(|(context, _)| context));
        for context in contexts {
            let expected = &expected[before_at(context)];
            let actual = &after[in_context(&after, context)].1;
            let mut path = Vec::new();
            if difference(expected, actual, &mut path) {
                return Err(format!(
                    "renaming {} would change {} as well: the key is not only written at \
                     its own place",
                    rename.describe(),
                    show(&path),
                ));
            }
        }
        Ok(())
    }
}

/// The key a new name writes: the text itself, or what it writes as a
/// string literal, which is how a key that is no identifier is shown.
fn new_key(new_name: &str) -> Result<String, String> {
    if !new_name.starts_with('"') {
        return Ok(new_name.to_owned());
    }
    IdPath::parse(&format!("@{new_name}"))
        .ok()
        .and_then(|id| id.name().map(str::to_owned))
        .ok_or_else(|| format!("`{new_name}` is not a string, and so not a key"))
}

/// Renames the key at `token`, or the key that the id at `token` names:
/// every operation on its place in every layer, the references through it
/// and the ids that resolve to it (`SPEC.md` 12.5). `current` gives the
/// text of a document as the editor has it.
pub(crate) fn rename(
    snapshot: Option<&Snapshot>,
    uri: &Uri,
    text: &str,
    token: &SyntaxToken,
    new_name: &str,
    current: &dyn Fn(&Uri) -> Option<String>,
    encoding: Encoding,
) -> Result<Option<WorkspaceEdit>, String> {
    let Some((at, _)) = at(token) else {
        return Ok(None);
    };
    let new = new_key(new_name)?;
    let snapshot = snapshot.ok_or(NOT_EVALUATED)?;
    let file = convert::uri_to_path(uri)
        .and_then(|path| snapshot.file_of(&path))
        .ok_or(NOT_EVALUATED)?;
    if snapshot.sources.file(file).text() != text {
        return Err(STALE.to_owned());
    }
    let world = World::new(snapshot, file)?;
    let place = world.place(&at)?;
    let Some(DataStep::Key(Key::Str(old))) = place.last() else {
        return Err(format!("{} is not a string key", show(&place)));
    };
    if **old == *new {
        return Ok(None);
    }
    let rename = Rename {
        place: &place,
        old,
        new: &new,
    };
    let ids = world.ids_to_rename(&rename)?;
    let expected = world.renamed(&rename, &ids)?;
    let edits = world.edits(&rename, &ids)?;
    world.verify(&rename, &edits, &expected)?;

    // `WorkspaceEdit` has `Uri` keys.
    #[allow(clippy::mutable_key_type)]
    let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
    for (edited, range, new) in edits {
        let source = snapshot.sources.file(edited);
        let target = snapshot
            .path(edited)
            .and_then(convert::path_to_uri)
            .ok_or_else(|| format!("`{}` is not a file to edit", source.name()))?;
        if !changes.contains_key(&target)
            && edited != file
            && current(&target).is_some_and(|text| text != source.text())
        {
            return Err(STALE.to_owned());
        }
        let index = LineIndex::new(source.text());
        changes.entry(target).or_default().push(TextEdit::new(
            convert::range(source.text(), &index, range, encoding),
            new,
        ));
    }
    Ok(Some(WorkspaceEdit::new(changes)))
}
