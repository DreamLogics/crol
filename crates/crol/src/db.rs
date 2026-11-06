//! `CrolDb`: reading layered data through typed paths, and changing it by
//! writing operations to one layer (`SPEC.md` 10.14).

use std::fs;
use std::io::{self, Write as _};
use std::marker::PhantomData;
use std::path::{Path as FilePath, PathBuf};

use crol_eval::{ExplainStep, Key, Value, print, uses_value_below};
use crol_syntax::ast::{self, AstNode, EntryOp};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::compose::{CrolContext, CrolData, DocumentChange, Reloaded, VariantName};
use crate::error::Error;
use crate::expression::IntoExpression;
use crate::path::{self, IntoPath, Step};
use crate::ser::{self, PrettyConfig};
use crate::view::{Query, View};
use crate::writer::DataWriter;

/// The layer of the operations of a database without an active layer.
pub(crate) const MEMORY_LAYER: &str = "(db)";
/// Its one document.
pub(crate) const MEMORY_DOCUMENT: &str = "db.crod";

/// An operation in the active layer.
#[derive(Debug, Clone, PartialEq)]
struct Operation {
    target: String,
    kind: OperationKind,
}

#[derive(Debug, Clone, PartialEq)]
enum OperationKind {
    /// `^target: value`
    Replace(String),
    /// `target: expression`
    Set(String),
    /// `-target`. `settle` holds its steps while it is not known that the
    /// layers below set it, if it took out an operation that did.
    Remove { settle: Option<Vec<Step>> },
}

/// What the active layer holds: for each condition, the operations under it,
/// in the order they were made (`SPEC.md` 10.14). A condition has no spaces,
/// and the empty one is the base.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Log {
    conditions: Vec<(String, Vec<Operation>)>,
}

/// Whether target `a` is `b`, or below it.
fn at_or_below(a: &str, b: &str) -> bool {
    a.strip_prefix(b)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(['.', '[', '?']))
}

impl Log {
    /// Reads what a database wrote. Anything else in the document is an error.
    pub(crate) fn parse(name: &str, text: &str) -> Result<Self, Error> {
        let parse = crol_syntax::parse(text);
        if let Some(error) = parse.errors().first() {
            return Err(Error::new(format!(
                "{name}: the active layer does not parse: {}",
                error.message()
            )));
        }
        let document = parse.document();
        let mut log = Self::default();
        if let Some(attribute) = document.attributes().next() {
            return Err(foreign(name, attribute.syntax()));
        }
        for entry in document.entries() {
            match entry {
                ast::AnyEntry::VariantBlock(block) if block.bind().is_none() => {
                    let condition: String = block
                        .variant()
                        .map(|v| v.syntax().text().to_string())
                        .unwrap_or_default()
                        .chars()
                        .filter(|c| !c.is_whitespace() && !matches!(c, '<' | '>'))
                        .collect();
                    for entry in block.entries() {
                        let operation = Self::read(name, &entry)?;
                        log.add(&condition, operation);
                    }
                }
                entry => {
                    let operation = Self::read(name, &entry)?;
                    log.add("", operation);
                }
            }
        }
        Ok(log)
    }

    fn read(name: &str, entry: &ast::AnyEntry) -> Result<Operation, Error> {
        let plain = |target: Option<ast::Target>| {
            target
                .filter(|target| {
                    !target.selectors().any(|selector| {
                        matches!(
                            selector,
                            ast::Selector::Variant(_) | ast::Selector::Parent(_)
                        )
                    })
                })
                .map(|target| target.syntax().text().to_string().trim().to_owned())
        };
        let operation = match entry {
            ast::AnyEntry::Entry(entry) if entry.bind().is_none() => {
                let value = entry.value().map(|v| v.syntax().text().to_string());
                match (entry.op(), plain(entry.target()), value) {
                    (EntryOp::Replace, Some(target), Some(value)) => Some(Operation {
                        target,
                        kind: OperationKind::Replace(value),
                    }),
                    (EntryOp::Set, Some(target), Some(value)) => Some(Operation {
                        target,
                        kind: OperationKind::Set(value),
                    }),
                    _ => None,
                }
            }
            ast::AnyEntry::Remove(remove) if remove.bind().is_none() => {
                plain(remove.target()).map(|target| Operation {
                    target,
                    kind: OperationKind::Remove { settle: None },
                })
            }
            _ => None,
        };
        operation.ok_or_else(|| foreign(name, entry.syntax()))
    }

    /// Adds an operation under a condition, as section 10.14 says. Returns
    /// whether it took out any other.
    fn add(&mut self, condition: &str, operation: Operation) -> bool {
        let at = match self.conditions.iter().position(|(c, _)| c == condition) {
            Some(at) => at,
            None => {
                self.conditions.push((condition.to_owned(), Vec::new()));
                self.conditions.len() - 1
            }
        };
        let list = &mut self.conditions[at].1;
        let replaces = match &operation.kind {
            OperationKind::Set(expression) => !uses_value_below(expression),
            _ => true,
        };
        let before = list.len();
        if replaces {
            list.retain(|old| !at_or_below(&old.target, &operation.target));
        }
        let took_out = list.len() != before;
        list.push(operation);
        took_out
    }

    /// The document.
    fn render(&self) -> Result<String, Error> {
        fn write(writer: &mut DataWriter, operations: &[Operation]) {
            for Operation { target, kind } in operations {
                match kind {
                    OperationKind::Replace(value) => writer.entry("^", target, value),
                    OperationKind::Set(value) => writer.entry("", target, value),
                    OperationKind::Remove { .. } => writer.remove_target(target),
                }
            }
        }
        let mut writer = DataWriter::new();
        for (condition, operations) in &self.conditions {
            if condition.is_empty() {
                write(&mut writer, operations);
            }
        }
        for (condition, operations) in &self.conditions {
            if !condition.is_empty() {
                writer.variant(condition, |writer| {
                    write(writer, operations);
                    Ok(())
                })?;
            }
        }
        writer.finish()
    }

    fn unsettled(&mut self) -> impl Iterator<Item = (&mut Option<Vec<Step>>, &str)> {
        self.conditions
            .iter_mut()
            .flat_map(|(_, operations)| operations)
            .filter_map(|operation| match &mut operation.kind {
                OperationKind::Remove { settle } if settle.is_some() => {
                    Some((settle, operation.target.as_str()))
                }
                _ => None,
            })
    }

    /// Marks the last operation under a condition as known to stand.
    fn settled(&mut self, condition: &str, target: &str) {
        let list = self.conditions.iter_mut().find(|(c, _)| c == condition);
        let last = list.and_then(|(_, operations)| operations.last_mut());
        if let Some(Operation {
            target: at,
            kind: OperationKind::Remove { settle },
        }) = last
            && at == target
        {
            *settle = None;
        }
    }

    /// Takes out the removes whose steps were left empty.
    fn drop_settled_away(&mut self) {
        for (_, operations) in &mut self.conditions {
            operations.retain(|operation| {
                !matches!(&operation.kind, OperationKind::Remove { settle: Some(steps) } if steps.is_empty())
            });
        }
        self.conditions.retain(|(_, list)| !list.is_empty());
    }
}

fn foreign(name: &str, syntax: &crol_syntax::SyntaxNode) -> Error {
    Error::new(format!(
        "{name}: the active layer holds `{}`, which a database does not write",
        syntax.text().to_string().trim()
    ))
}

/// The document of the active layer.
#[derive(Debug, Clone)]
pub(crate) struct Active {
    /// The layer's name.
    pub(crate) layer: String,
    /// The document's path in the layer.
    pub(crate) document: String,
    /// The file it is saved to; `None` until `save_as`.
    pub(crate) file: Option<PathBuf>,
    /// The file's text when it was last read or saved; `None` if there was
    /// no file.
    pub(crate) read: Option<String>,
    /// What it holds.
    pub(crate) log: Log,
}

impl Active {
    /// The layer of a database that has no active layer.
    pub(crate) fn in_memory() -> Self {
        Self {
            layer: MEMORY_LAYER.to_owned(),
            document: MEMORY_DOCUMENT.to_owned(),
            file: None,
            read: None,
            log: Log::default(),
        }
    }
}

/// That a file can be the active layer's document: it says nothing of
/// whether it exists.
pub(crate) fn check_document(file: &FilePath) -> Result<(), Error> {
    if file.is_dir() {
        return Err(Error::new(format!(
            "{}: the active layer is one document, not a directory",
            file.display()
        )));
    }
    if file.extension().is_none_or(|extension| extension != "crod") {
        return Err(Error::new(format!(
            "{}: the active layer is a `.crod` document",
            file.display()
        )));
    }
    Ok(())
}

/// A file's text; `None` if there is no file.
pub(crate) fn read_document(file: &FilePath) -> Result<Option<String>, Error> {
    match fs::read_to_string(file) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::new(format!(
            "cannot read {}: {error}",
            file.display()
        ))),
    }
}

/// Replaces a file through a temporary file in its directory, so that a
/// reader finds the old text or the new.
fn write_document(file: &FilePath, text: &str) -> Result<(), Error> {
    let failed = |error: io::Error| Error::new(format!("cannot write {}: {error}", file.display()));
    let name = file
        .file_name()
        .ok_or_else(|| Error::new(format!("{} is no file name", file.display())))?;
    let mut temporary = name.to_owned();
    temporary.push(format!(".{}.tmp", std::process::id()));
    let temporary = file.with_file_name(temporary);
    fs::write(&temporary, text).map_err(failed)?;
    fs::rename(&temporary, file).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        failed(error)
    })
}

/// The value at a path in a context. Steps that do not start at a root key
/// lead nowhere.
fn found<'v, V: VariantName>(
    data: &'v CrolData<V>,
    steps: &[Step],
    context: &CrolContext,
) -> Option<&'v Value> {
    let (Step::Key(Key::Str(key)), rest) = steps.split_first()? else {
        return None;
    };
    path::value_at(data.value(key, context)?, rest)
}

fn read<T: DeserializeOwned, V: VariantName>(
    data: &CrolData<V>,
    steps: &[Step],
    context: &CrolContext,
) -> Result<T, Error> {
    if steps.is_empty() {
        return data.deserialize_root(Some(context));
    }
    let value = found(data, steps, context).ok_or_else(|| not_set(steps))?;
    let key = match &steps[0] {
        Step::Key(Key::Str(key)) => Some(&**key),
        _ => None,
    };
    let options = data.deserialize_options().clone();
    crate::deserialize(value, data.sources(), options, key).map(|(value, _)| value)
}

fn not_set(steps: &[Step]) -> Error {
    let target = path::target(steps).unwrap_or_default();
    Error::new(format!("`{target}` is not set in this context"))
}

/// Reads a [`View`] of the value at a path, which must be set. Fields the
/// value lacks go by the rules of the view (`SPEC.md` 10.11).
fn read_view<W: View, V: VariantName>(
    data: &CrolData<V>,
    steps: &[Step],
    context: &CrolContext,
) -> Result<W, Error> {
    let Some(Step::Key(Key::Str(key))) = steps.first() else {
        return Err(Error::new("a view needs a path to a root key or below it"));
    };
    let value = found(data, steps, context).ok_or_else(|| not_set(steps))?;
    data.read_view(key, value)
}

/// Layered data, read through typed paths and changed by transactions
/// (`SPEC.md` 10.14). `R` is the type of the root, and `V` that of the
/// variants. From [`CrolComposer::compose_db`](crate::CrolComposer::compose_db).
///
/// ```
/// use crol::{CrolComposer, CrolDb, CrolLayer, Navigable as _};
/// use crol_derive::{Navigable, croxp};
/// use serde::{Deserialize, Serialize};
///
/// #[derive(Deserialize, Serialize, Navigable)]
/// struct Config {
///     port: u16,
///     url: String,
/// }
///
/// let base = CrolLayer::from_sources("base", [("a.crod", r#"
///     +port: 80
///     +url: "http://web:" + &root.port
/// "#)]);
/// let mut db: CrolDb<Config> = CrolComposer::new().load_layer(base).compose_db()?;
///
/// db.transaction()
///     .at(Config::path().port())
///     .set_expr(croxp! { & + 1 })?
///     .commit()?;
///
/// assert_eq!(db.select().get(Config::path().url())?, "http://web:81");
/// # Ok::<(), crol::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct CrolDb<R, V = String> {
    data: CrolData<V>,
    active: Active,
    root: PhantomData<fn() -> R>,
}

impl<R, V: VariantName> CrolDb<R, V> {
    pub(crate) fn new(data: CrolData<V>, active: Active) -> Self {
        Self {
            data,
            active,
            root: PhantomData,
        }
    }

    /// The data as it is now, with what has been committed.
    pub fn data(&self) -> &CrolData<V> {
        &self.data
    }

    /// Evaluates a context, if it is not yet, so that [`select`](Self::select)
    /// can read it. Every context is evaluated on loading, unless
    /// `CrolConfig::contexts` or `each_variant` left it out. A transaction does
    /// this for itself.
    pub fn evaluate<W: Into<V>>(
        &mut self,
        variants: impl IntoIterator<Item = W>,
    ) -> Result<(), Error> {
        self.data.evaluate(variants).map(|_| ())
    }

    /// Reads values, in the base context unless `with_variant` says.
    pub fn select(&self) -> Select<'_, R, V> {
        Select {
            data: &self.data,
            variants: Vec::new(),
            root: PhantomData,
        }
    }

    /// Starts a transaction, in the base context unless `with_variant` says.
    pub fn transaction(&mut self) -> Transaction<'_, R, V> {
        Transaction {
            state: State {
                log: self.active.log.clone(),
                db: Db {
                    data: &mut self.data,
                    active: &mut self.active,
                },
                work: None,
                below: None,
                dirty: false,
                variants: Vec::new(),
                condition: String::new(),
                writes: Vec::new(),
            },
            prefix: Vec::new(),
            base: PhantomData,
        }
    }

    /// Writes what has been committed to the active layer's document. If its
    /// text is not what was last read or saved, this is an error, and
    /// nothing is written.
    pub fn save(&mut self) -> Result<(), Error> {
        let file = self.active.file.clone().ok_or_else(|| {
            Error::new("there is no active layer to save to: use `save_as`".to_owned())
        })?;
        if read_document(&file)? != self.active.read {
            return Err(Error::new(format!(
                "{} changed since it was read; nothing was saved",
                file.display()
            )));
        }
        let text = self.active.log.render()?;
        write_document(&file, &text)?;
        self.active.read = Some(text);
        Ok(())
    }

    /// Writes what has been committed to a new document, which becomes the
    /// active layer's. It is an error if the file exists. The next run has to
    /// load it, with `load_active`.
    pub fn save_as(&mut self, file: impl AsRef<FilePath>) -> Result<(), Error> {
        let file = file.as_ref();
        check_document(file)?;
        let text = self.active.log.render()?;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(file)
            .and_then(|mut opened| opened.write_all(text.as_bytes()))
            .map_err(|error| Error::new(format!("cannot write {}: {error}", file.display())))?;
        self.active.file = Some(file.to_owned());
        self.active.read = Some(text);
        Ok(())
    }
}

/// Reads values of a [`CrolDb`]: [`CrolDb::select`].
#[derive(Debug)]
#[must_use = "a selection reads nothing until `get`"]
pub struct Select<'d, R, V = String> {
    data: &'d CrolData<V>,
    variants: Vec<V>,
    root: PhantomData<fn() -> R>,
}

impl<'d, R, V: VariantName> Select<'d, R, V> {
    /// Makes a variant active in the context to read, as it does on the
    /// composer. The context must be evaluated: see [`CrolDb::evaluate`].
    pub fn with_variant(mut self, variant: impl Into<V>) -> Result<Self, Error> {
        self.variants.push(variant.into());
        self.data.context_of(self.variants.iter().cloned())?;
        Ok(self)
    }

    /// The value at a path. It is an error if the path is not set in this
    /// context.
    pub fn get<P: IntoPath<R>>(&self, path: P) -> Result<P::Target, Error>
    where
        P::Target: DeserializeOwned,
    {
        let context = self.data.context(self.variants.iter().cloned())?;
        read(self.data, path.into_path().steps(), &context)
    }

    /// A [`View`] of the value at a path: `.view::<Stats>(path)`. Where the
    /// value lacks a field, the view's rules say what it reads, but the path
    /// itself must be set.
    pub fn view<W: View>(&self, path: impl IntoPath<R>) -> Result<W, Error> {
        let context = self.data.context(self.variants.iter().cloned())?;
        read_view(self.data, path.into_path().steps(), &context)
    }

    /// A [`Query`] of this context. If it was not evaluated, reading the query
    /// is an error.
    pub fn query(&self) -> Query<'d, V> {
        match self.data.context(self.variants.iter().cloned()) {
            Ok(context) => self.data.query().in_context(&context),
            Err(error) => self.data.query().failed(error),
        }
    }
}

/// What a write said, to check that it took effect.
#[derive(Debug)]
enum Written {
    Value(Value),
    Expression,
    Removed,
}

#[derive(Debug)]
struct Write {
    steps: Vec<Step>,
    target: String,
    written: Written,
}

#[derive(Debug)]
struct Db<'d, V> {
    data: &'d mut CrolData<V>,
    active: &'d mut Active,
}

#[derive(Debug)]
struct State<'d, V> {
    db: Db<'d, V>,
    /// The operations, with this transaction's.
    log: Log,
    /// The data with them, once something needed it.
    work: Option<CrolData<V>>,
    /// The data without the active layer, once a remove needed it.
    below: Option<CrolData<V>>,
    /// Whether `work` lacks an operation.
    dirty: bool,
    variants: Vec<V>,
    condition: String,
    writes: Vec<Write>,
}

impl<V: VariantName> State<'_, V> {
    fn data(&self) -> &CrolData<V> {
        self.work.as_ref().unwrap_or(&*self.db.data)
    }

    /// The data to work on. What the layers below lower to stays in the
    /// original, for the transactions after this one, whatever becomes of it.
    fn clone_data(&mut self) -> CrolData<V> {
        self.db.data.lower_unchanged(&[&self.db.active.layer]);
        self.db.data.clone()
    }

    fn reload(&self, data: &mut CrolData<V>, text: &str) -> Result<(), Error> {
        let change = DocumentChange {
            layer: &self.db.active.layer,
            path: &self.db.active.document,
            text: Some(text),
        };
        data.reload(&[change]).map(|_| ())
    }

    /// Whether the layers below set what a remove removes, as they must for
    /// it to stay (`SPEC.md` 10.14).
    fn settle_removes(&mut self) -> Result<(), Error> {
        if self.log.unsettled().next().is_none() {
            return Ok(());
        }
        let mut below = match self.below.take() {
            Some(below) => below,
            None => {
                let mut below = self.clone_data();
                self.reload(&mut below, "")?;
                below
            }
        };
        let context = below.evaluate(self.variants.iter().cloned())?;
        for (settle, _) in self.log.unsettled() {
            let steps = settle.take().unwrap_or_default();
            if found(&below, &steps, &context).is_none() {
                *settle = Some(Vec::new());
            }
        }
        self.log.drop_settled_away();
        self.below = Some(below);
        Ok(())
    }

    /// Evaluates what is pending, and gives the transaction's context.
    fn evaluate(&mut self) -> Result<CrolContext, Error> {
        if self.dirty {
            self.settle_removes()?;
            let text = self.log.render()?;
            let mut work = match self.work.take() {
                Some(work) => work,
                None => self.clone_data(),
            };
            let reloaded = self.reload(&mut work, &text);
            self.work = Some(work);
            reloaded?;
            self.dirty = false;
        }
        let data = match &mut self.work {
            Some(work) => work,
            None => &mut *self.db.data,
        };
        data.evaluate(self.variants.iter().cloned())
    }

    fn write(
        &mut self,
        steps: Vec<Step>,
        kind: OperationKind,
        written: Written,
    ) -> Result<(), Error> {
        let target = path::target(&steps)
            .ok_or_else(|| Error::new("a write needs a path to a root key or below it"))?;
        let removing = matches!(kind, OperationKind::Remove { .. });
        let kind = match kind {
            OperationKind::Remove { .. } => OperationKind::Remove {
                settle: Some(steps.clone()),
            },
            kind => kind,
        };
        let took_out = self.log.add(
            &self.condition,
            Operation {
                target: target.clone(),
                kind,
            },
        );
        if removing && !took_out {
            self.log.settled(&self.condition, &target);
        }
        self.writes.push(Write {
            steps,
            target,
            written,
        });
        self.dirty = true;
        Ok(())
    }

    /// That each write took effect in the context (`SPEC.md` 10.14).
    fn check_writes(&self, context: &CrolContext) -> Result<(), Error> {
        let data = self.data();
        let last: Vec<&Write> = self
            .writes
            .iter()
            .enumerate()
            .filter(|(i, write)| {
                !self.writes[i + 1..].iter().any(|later| {
                    at_or_below(&later.target, &write.target)
                        || at_or_below(&write.target, &later.target)
                })
            })
            .map(|(_, write)| write)
            .collect();
        let expressions: Vec<&str> = last
            .iter()
            .filter(|write| matches!(write.written, Written::Expression))
            .map(|write| write.target.as_str())
            .collect();
        let mut histories = data.explain_all(&expressions, context)?.into_iter();
        for write in last {
            match &write.written {
                Written::Removed => {}
                Written::Value(value) => {
                    let held = found(data, &write.steps, context);
                    if !held.is_some_and(|held| held.equals(value)) {
                        let history = data.explain(&write.target, context)?;
                        return Err(did_not_apply(&write.target, &history));
                    }
                }
                Written::Expression => {
                    let history = histories.next().unwrap_or_default();
                    let layer = &self.db.active.layer;
                    let after = history.iter().rposition(|step| step.layer == *layer);
                    let decided = after.is_none_or(|after| {
                        history[after + 1..].iter().any(|step| !step.uses_below)
                    });
                    if decided {
                        return Err(did_not_apply(&write.target, &history));
                    }
                }
            }
        }
        Ok(())
    }
}

fn did_not_apply(target: &str, history: &[ExplainStep]) -> Error {
    match history.iter().rfind(|step| !step.uses_below) {
        Some(step) => Error::new(format!(
            "the write to `{target}` did not take effect: `{}` in the layer `{}` decides it",
            step.entry, step.layer
        )),
        None => Error::new(format!("the write to `{target}` did not take effect")),
    }
}

/// A sequence of changes to a [`CrolDb`]: [`CrolDb::transaction`]. Paths
/// start at `B`. Reads see the operations before them, and nothing changes
/// until [`commit`](Self::commit). Dropping a transaction discards it.
#[derive(Debug)]
#[must_use = "a transaction changes nothing until `commit`"]
pub struct Transaction<'d, B, V = String> {
    state: State<'d, V>,
    prefix: Vec<Step>,
    base: PhantomData<fn() -> B>,
}

impl<'d, B, V: VariantName> Transaction<'d, B, V> {
    fn full<P: IntoPath<B>>(&self, path: P) -> Vec<Step> {
        let mut steps = self.prefix.clone();
        steps.extend(path.into_path().into_steps());
        steps
    }

    /// Makes a variant active in the transaction's context, as it does on the
    /// composer. It is an error after the first operation.
    pub fn with_variant(mut self, variant: impl Into<V>) -> Result<Self, Error> {
        if !self.state.writes.is_empty() {
            return Err(Error::new(
                "`with_variant` comes before a transaction's first operation",
            ));
        }
        self.state.variants.push(variant.into());
        let data = self.state.data();
        let context = data.context_of(self.state.variants.iter().cloned())?;
        let names = data.context_names(&CrolContext::new(context));
        self.state.condition = names.join("&");
        Ok(self)
    }

    /// Moves to a path, for one of [`Cursor`]'s operations.
    pub fn at<P: IntoPath<B>>(self, path: P) -> Cursor<'d, B, P::Target, V> {
        let steps = self.full(path);
        Cursor {
            transaction: self,
            steps,
            target: PhantomData,
        }
    }

    /// The value at a path, at this point of the transaction. An error
    /// leaves the transaction open.
    pub fn get<P: IntoPath<B>>(&mut self, path: P) -> Result<P::Target, Error>
    where
        P::Target: DeserializeOwned,
    {
        let steps = self.full(path);
        let context = self.state.evaluate()?;
        read(self.state.data(), &steps, &context)
    }

    /// A [`View`] of the value at a path, at this point of the transaction:
    /// `.view::<Stats>(path)`. An error leaves the transaction open.
    pub fn view<W: View>(&mut self, path: impl IntoPath<B>) -> Result<W, Error> {
        let steps = self.full(path);
        let context = self.state.evaluate()?;
        read_view(self.state.data(), &steps, &context)
    }

    /// A [`Query`] of the transaction's context, at this point of it, over the
    /// whole data, whatever `within` says. An error leaves the transaction
    /// open.
    pub fn query(&mut self) -> Result<Query<'_, V>, Error> {
        let context = self.state.evaluate()?;
        Ok(self.state.data().query().in_context(&context))
    }

    /// Gives a closure this transaction with paths that start at `path`, and
    /// goes on from there when it returns.
    pub fn within<P, F>(self, path: P, body: F) -> Result<Self, Error>
    where
        P: IntoPath<B>,
        F: FnOnce(Transaction<'d, P::Target, V>) -> Result<Transaction<'d, P::Target, V>, Error>,
    {
        let steps = self.full(path);
        let Self { state, prefix, .. } = self;
        let scope = Transaction {
            state,
            prefix: steps,
            base: PhantomData,
        };
        let state = body(scope)?.state;
        Ok(Self {
            state,
            prefix,
            base: PhantomData,
        })
    }

    /// Applies the operations: it evaluates what is pending, with every check
    /// that evaluating runs, and checks that each write took effect. If it
    /// fails, the data stays as it was. [`Reloaded::changed`] lists the root
    /// keys that changed. This changes the data in memory:
    /// [`CrolDb::save`] writes it.
    pub fn commit(mut self) -> Result<Reloaded, Error> {
        if self.state.writes.is_empty() {
            return Ok(Reloaded {
                changed: Vec::new(),
            });
        }
        let context = self.state.evaluate()?;
        self.state.check_writes(&context)?;
        let State { db, log, work, .. } = self.state;
        let work = work.ok_or_else(|| Error::new("the transaction was not evaluated"))?;
        let changed = crate::changed_keys(&db.data.key_fingerprints(), &work.key_fingerprints());
        *db.data = work;
        db.active.log = log;
        Ok(Reloaded { changed })
    }
}

/// A place in a [`Transaction`], from [`Transaction::at`]. Each operation
/// gives the transaction back.
#[derive(Debug)]
#[must_use = "a place changes nothing without an operation"]
pub struct Cursor<'d, B, T, V = String> {
    transaction: Transaction<'d, B, V>,
    steps: Vec<Step>,
    target: PhantomData<fn() -> T>,
}

impl<'d, B, T, V: VariantName> Cursor<'d, B, T, V> {
    /// Writes `^target: value`, a fixed value.
    pub fn set(mut self, value: T) -> Result<Transaction<'d, B, V>, Error>
    where
        T: Serialize,
    {
        let value = ser::to_value(&value, &PrettyConfig::default())?;
        let text = print::to_text(&value);
        self.transaction.state.write(
            self.steps,
            OperationKind::Replace(text),
            Written::Value(value),
        )?;
        Ok(self.transaction)
    }

    /// Writes `target: expression`. The expression is kept, not its result:
    /// `& + 2` stays two more than the layers below say.
    pub fn set_expr(
        mut self,
        expression: impl IntoExpression,
    ) -> Result<Transaction<'d, B, V>, Error> {
        let text = expression.into_expression()?.as_str().to_owned();
        self.transaction
            .state
            .write(self.steps, OperationKind::Set(text), Written::Expression)?;
        Ok(self.transaction)
    }

    /// Writes what a closure makes of the value at this point of the
    /// transaction, as [`set`](Self::set) does. An error ends the transaction.
    pub fn update(mut self, change: impl FnOnce(T) -> T) -> Result<Transaction<'d, B, V>, Error>
    where
        T: Serialize + DeserializeOwned,
    {
        let context = self.transaction.state.evaluate()?;
        let value = read(self.transaction.state.data(), &self.steps, &context)?;
        self.set(change(value))
    }

    /// Writes `-target`.
    pub fn remove(mut self) -> Result<Transaction<'d, B, V>, Error> {
        self.transaction.state.write(
            self.steps,
            OperationKind::Remove { settle: None },
            Written::Removed,
        )?;
        Ok(self.transaction)
    }
}
