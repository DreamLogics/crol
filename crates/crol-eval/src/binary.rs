//! `.crob` files: a layer lowered ahead of time (`SPEC.md` 7.8).
//!
//! The file holds the layer's documents, as text, and what lowering them
//! gives, so that loading skips parsing and lowering. The text stays: errors
//! and `explain` point into it, and tools that want a syntax tree parse it.
//! All of it is compressed with LZ4, which costs a few milliseconds to read.
//!
//! Files come from elsewhere, so reading checks everything evaluation relies
//! on: a hash against damage, and for what a hash cannot catch, that every id
//! is in range, that expressions only hold earlier ones, so that there are no
//! cycles, and how deep they nest.

use std::collections::HashMap;
use std::sync::Arc;

use crol_syntax::{TextRange, TextSize};
use indexmap::IndexSet;

use crate::cond::Cond;
use crate::ir::*;
use crate::lower::LoweredData;
use crate::source::{FileId, Sources, Span};
use crate::value::{IdPath, Key};

const MAGIC: &[u8; 4] = b"CROB";

/// The version of the format: files of another are refused, and made again.
pub const FORMAT: u32 = 7;

/// The magic, the format, the hash of the rest of the file, the hash of the
/// documents and the size of what is compressed.
const HEADER: usize = 4 + 4 + 32 + 32 + 8;

/// The most LZ4 can expand, a little over the true 255.
const MAX_EXPANSION: usize = 256;

/// How deep expressions may nest: what the parser allows gives less.
const MAX_DEPTH: usize = crol_syntax::MAX_NESTING * 3;

/// A document of a `.crob` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryDocument {
    /// Its path in the layer.
    pub relative: String,
    /// Its name, for messages.
    pub display: String,
    /// Its text.
    pub text: String,
}

/// What a `.crob` file holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryLayer {
    /// The documents, in the order of their paths.
    pub documents: Vec<BinaryDocument>,
    /// What lowering them gave, for [`crate::compile_binary`].
    pub lowered: Arc<[u8]>,
}

// ---- Writing ----

struct Writer<'a> {
    out: Vec<u8>,
    strings: IndexSet<Arc<str>>,
    files: &'a HashMap<FileId, u64>,
}

impl Writer<'_> {
    fn uint(&mut self, mut n: u64) {
        loop {
            let byte = (n & 0x7F) as u8;
            n >>= 7;
            if n == 0 {
                self.out.push(byte);
                return;
            }
            self.out.push(byte | 0x80);
        }
    }

    fn int(&mut self, n: i128) {
        self.out.extend_from_slice(&n.to_le_bytes());
    }

    fn id(&mut self, id: usize) {
        self.uint(id as u64);
    }

    fn flag(&mut self, b: bool) {
        self.out.push(u8::from(b));
    }

    fn string(&mut self, s: &Arc<str>) {
        let (index, _) = self.strings.insert_full(s.clone());
        self.uint(index as u64);
    }

    fn span(&mut self, span: Span) {
        // 0 is a span in no document.
        let file = self.files.get(&span.file).map_or(0, |index| index + 1);
        self.uint(file);
        self.uint(u32::from(span.range.start()).into());
        self.uint(u32::from(span.range.end()).into());
    }

    fn list<T>(&mut self, items: &[T], mut write: impl FnMut(&mut Self, &T)) {
        self.uint(items.len() as u64);
        for item in items {
            write(self, item);
        }
    }

    fn option<T>(&mut self, item: Option<&T>, write: impl FnOnce(&mut Self, &T)) {
        self.flag(item.is_some());
        if let Some(item) = item {
            write(self, item);
        }
    }

    fn key(&mut self, key: &Key) {
        match key {
            Key::Int(i) => {
                self.out.push(0);
                self.int(*i);
            }
            Key::Str(s) => {
                self.out.push(1);
                self.string(s);
            }
        }
    }

    fn selector(&mut self, selector: &Selector) {
        self.flag(selector.optional);
        match &selector.kind {
            SelectorKind::Field(name) => {
                self.out.push(0);
                self.string(name);
            }
            SelectorKind::Element(i) => {
                self.out.push(1);
                self.id(*i);
            }
            SelectorKind::Accessor(i) => {
                self.out.push(2);
                self.int(*i);
            }
            SelectorKind::Context(changes) => {
                self.out.push(3);
                self.list(changes, |w, (name, on, span)| {
                    w.string(name);
                    w.flag(*on);
                    w.span(*span);
                });
            }
        }
        self.span(selector.span);
    }

    fn selectors(&mut self, selectors: &[Selector]) {
        self.list(selectors, Self::selector);
    }

    fn ids(&mut self, ids: &[usize]) {
        self.list(ids, |w, id| w.id(*id));
    }

    fn set_op(&mut self, op: Option<SetOp>) {
        self.out.push(match op {
            None => 0,
            Some(SetOp::Set) => 1,
            Some(SetOp::Declare) => 2,
            Some(SetOp::Ensure) => 3,
        });
    }

    fn reference(&mut self, kind: &RefKind) {
        match kind {
            RefKind::Below => self.out.push(0),
            RefKind::SelfValue => self.out.push(1),
            RefKind::Parent(n) => {
                self.out.push(2);
                self.id(*n);
            }
            RefKind::Root => self.out.push(3),
            RefKind::Default(target) => {
                self.out.push(4);
                self.option(target.type_name.as_ref(), |w, s| w.string(s));
                self.option(target.root_key.as_ref(), |w, s| w.string(s));
                self.list(&target.path, |w, s| w.string(s));
            }
            RefKind::Bind { up, target } => {
                self.out.push(5);
                self.id(*up);
                self.selectors(target);
            }
            RefKind::ValueBind(id) => {
                self.out.push(6);
                self.id(*id);
            }
            RefKind::Top => self.out.push(7),
        }
    }

    fn expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Unit => self.out.push(0),
            ExprKind::Bool(b) => {
                self.out.push(1);
                self.flag(*b);
            }
            ExprKind::Int(i) => {
                self.out.push(2);
                self.int(i.0);
            }
            ExprKind::Float(f) => {
                self.out.push(3);
                self.out.extend_from_slice(&f.to_le_bytes());
            }
            ExprKind::Char(c) => {
                self.out.push(4);
                self.uint((*c).into());
            }
            ExprKind::Str(s) => {
                self.out.push(5);
                self.string(s);
            }
            ExprKind::Id(id) => {
                self.out.push(6);
                self.flag(id.is_rooted());
                self.uint(id.keys().len() as u64);
                for key in id.keys() {
                    self.string(key);
                }
            }
            ExprKind::Ref(kind) => {
                self.out.push(7);
                self.reference(kind);
            }
            ExprKind::Block(id) => {
                self.out.push(8);
                self.id(*id);
            }
            ExprKind::List(items) => {
                self.out.push(9);
                self.ids(items);
            }
            ExprKind::Tuple(items) => {
                self.out.push(10);
                self.ids(items);
            }
            ExprKind::Named(named) => {
                self.out.push(11);
                self.string(&named.0);
                self.ids(&named.1);
            }
            ExprKind::UnitName(name) => {
                self.out.push(12);
                self.string(name);
            }
            ExprKind::Path(base, selectors) => {
                self.out.push(13);
                self.id(*base);
                self.selectors(selectors);
            }
            ExprKind::Neg(operand) => {
                self.out.push(14);
                self.id(*operand);
            }
            ExprKind::Not(operand) => {
                self.out.push(18);
                self.id(*operand);
            }
            ExprKind::Scope(operand) => {
                self.out.push(19);
                self.id(*operand);
            }
            ExprKind::Binary(op, lhs, rhs) => {
                self.out.push(15);
                self.out.push(match op {
                    BinOp::Add => 0,
                    BinOp::Sub => 1,
                    BinOp::Mul => 2,
                    BinOp::Div => 3,
                    BinOp::Rem => 4,
                    BinOp::Merge => 5,
                    BinOp::MergeShallow => 6,
                    BinOp::Fallback => 7,
                    BinOp::Union => 8,
                });
                self.id(*lhs);
                self.id(*rhs);
            }
            ExprKind::Macro(kind, args) => {
                self.out.push(16);
                self.out.push(match kind {
                    MacroKind::Round => 0,
                    MacroKind::Floor => 1,
                    MacroKind::Ceil => 2,
                    MacroKind::Clamp => 3,
                });
                self.ids(args);
            }
            ExprKind::Error => self.out.push(17),
        }
        self.span(expr.span);
    }

    fn entry(&mut self, entry: &Entry) {
        self.set_op(entry.op);
        self.flag(entry.replace);
        self.flag(entry.if_set);
        self.key(&entry.key);
        self.flag(entry.key_optional);
        self.selectors(&entry.path);
        self.option(entry.value.as_ref(), |w, id| w.id(*id));
        self.span(entry.span);
    }

    fn block(&mut self, block: &Block) {
        self.option(block.name.as_ref(), |w, s| w.string(s));
        self.ids(&block.spreads);
        self.list(&block.entries, Self::entry);
        self.flag(block.defaults);
        self.flag(block.scope);
        self.ids(&block.value_binds);
    }

    fn condition(&mut self, condition: &Cond) {
        match condition {
            Cond::Name(name, span) => {
                self.out.push(0);
                self.string(name);
                self.span(*span);
            }
            Cond::Not(c) => {
                self.out.push(1);
                self.condition(c);
            }
            Cond::And(a, b) => self.pair(2, a, b),
            Cond::Or(a, b) => self.pair(3, a, b),
            Cond::Xor(a, b) => self.pair(4, a, b),
        }
    }

    fn pair(&mut self, tag: u8, a: &Cond, b: &Cond) {
        self.out.push(tag);
        self.condition(a);
        self.condition(b);
    }

    fn op(&mut self, op: &RootOp) {
        self.string(&op.key);
        self.set_op(op.op);
        self.flag(op.replace);
        self.flag(op.if_set);
        self.flag(op.key_optional);
        self.selectors(&op.path);
        self.option(op.value.as_ref(), |w, id| w.id(*id));
        self.span(op.span);
        self.flag(op.uses_below);
        self.id(op.document);
        self.option(op.condition.as_ref(), Self::condition);
        self.list(&op.blocks, |w, path| w.selectors(path));
    }
}

/// What lowering a layer gave, as bytes.
pub(crate) fn write_lowered(lowered: &LoweredData, files: &[FileId]) -> Vec<u8> {
    let index: HashMap<FileId, u64> = files
        .iter()
        .enumerate()
        .map(|(i, file)| (*file, i as u64))
        .collect();
    let mut w = Writer {
        out: Vec::new(),
        strings: IndexSet::new(),
        files: &index,
    };
    let exprs: Vec<&Expr> = lowered.ir.exprs.iter().collect();
    w.list(&exprs, |w, expr| w.expr(expr));
    let blocks: Vec<&Block> = lowered.ir.blocks.iter().collect();
    w.list(&blocks, |w, block| w.block(block));
    let binds: Vec<&ValueBind> = lowered.ir.value_binds.iter().collect();
    w.list(&binds, |w, bind| {
        w.string(&bind.name);
        w.id(bind.expr);
    });
    w.ids(&lowered.templates);
    w.list(&lowered.ops, Writer::op);
    w.list(&lowered.root_defaults, |w, defaults| {
        w.option(defaults.condition.as_ref(), Writer::condition);
        w.span(defaults.span);
    });
    w.list(&lowered.declared, |w, declared| {
        w.string(&declared.group);
        w.string(&declared.name);
        w.span(declared.span);
    });
    let body = std::mem::take(&mut w.out);
    let strings: Vec<Arc<str>> = w.strings.iter().cloned().collect();
    w.list(&strings, |w, s| {
        w.uint(s.len() as u64);
        w.out.extend_from_slice(s.as_bytes());
    });
    w.out.extend_from_slice(&body);
    w.out
}

fn text_field(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(&(text.len() as u64).to_le_bytes());
    out.extend_from_slice(text.as_bytes());
}

/// A whole `.crob` file.
pub(crate) fn write_file(documents: &[BinaryDocument], lowered: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&(documents.len() as u64).to_le_bytes());
    for document in documents {
        text_field(&mut body, &document.relative);
        text_field(&mut body, &document.display);
        text_field(&mut body, &document.text);
    }
    body.extend_from_slice(lowered);
    let packed = lz4_flex::block::compress(&body);
    let mut out = Vec::with_capacity(packed.len() + HEADER);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT.to_le_bytes());
    out.extend_from_slice(&[0; 32]);
    let documents = documents
        .iter()
        .map(|d| (d.relative.as_str(), d.text.as_str()));
    out.extend_from_slice(&hash_documents(documents));
    out.extend_from_slice(&(body.len() as u64).to_le_bytes());
    out.extend_from_slice(&packed);
    let hash = blake3::hash(&out[40..]);
    out[8..40].copy_from_slice(hash.as_bytes());
    out
}

/// A hash of a layer's documents, by path and text, in any order: what a
/// `.crob` file says it was made from.
pub fn hash_documents<'a>(documents: impl Iterator<Item = (&'a str, &'a str)>) -> [u8; 32] {
    let mut documents: Vec<(&str, &str)> = documents.collect();
    documents.sort_unstable();
    let mut hasher = blake3::Hasher::new();
    for (path, text) in documents {
        hasher.update(&(path.len() as u64).to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update(&(text.len() as u64).to_le_bytes());
        hasher.update(text.as_bytes());
    }
    *hasher.finalize().as_bytes()
}

/// What [`hash_documents`] gave for the documents a `.crob` file was made
/// from, without reading the rest; `None` for bytes that are no file of this
/// format.
pub fn source_hash(bytes: &[u8]) -> Option<[u8; 32]> {
    if bytes.len() < HEADER || &bytes[..4] != MAGIC || bytes[4..8] != FORMAT.to_le_bytes() {
        return None;
    }
    bytes[40..72].try_into().ok()
}

// ---- Reading ----

/// Reads a `.crob` file: its header and documents. What was lowered is read
/// when the layer is compiled.
pub fn read_layer(bytes: &[u8]) -> Result<BinaryLayer, String> {
    if bytes.len() < HEADER || &bytes[..4] != MAGIC {
        return Err("this is no `.crob` file".to_owned());
    }
    let format = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    if format != FORMAT {
        return Err(format!(
            "this `.crob` file has format {format}, and this version of crol reads format {FORMAT}; compile the layer again"
        ));
    }
    let damaged = || "this `.crob` file is damaged".to_owned();
    let packed = &bytes[HEADER..];
    if blake3::hash(&bytes[40..]).as_bytes() != &bytes[8..40] {
        return Err(damaged());
    }
    let size: [u8; 8] = bytes[72..HEADER].try_into().map_err(|_| damaged())?;
    let size = usize::try_from(u64::from_le_bytes(size)).map_err(|_| damaged())?;
    // LZ4 cannot expand by more than this, so a size that says otherwise
    // does not get to ask for the memory.
    if size / MAX_EXPANSION > packed.len() {
        return Err(damaged());
    }
    let body = lz4_flex::block::decompress(packed, size).map_err(|_| damaged())?;
    let body = body.as_slice();
    let mut at = 0;
    let size = |at: &mut usize| -> Option<usize> {
        let end = at.checked_add(8)?;
        let raw: [u8; 8] = body.get(*at..end)?.try_into().ok()?;
        *at = end;
        usize::try_from(u64::from_le_bytes(raw)).ok()
    };
    let count = size(&mut at).ok_or_else(damaged)?;
    let mut documents = Vec::new();
    for _ in 0..count {
        let field = |at: &mut usize| -> Option<String> {
            let length = size(at)?;
            let end = at.checked_add(length)?;
            let text = std::str::from_utf8(body.get(*at..end)?).ok()?;
            *at = end;
            Some(text.to_owned())
        };
        let (relative, display, text) = (
            field(&mut at).ok_or_else(damaged)?,
            field(&mut at).ok_or_else(damaged)?,
            field(&mut at).ok_or_else(damaged)?,
        );
        documents.push(BinaryDocument {
            relative,
            display,
            text,
        });
    }
    Ok(BinaryLayer {
        documents,
        lowered: Arc::from(&body[at..]),
    })
}

/// What a layer lowered ahead of time adds to the IR.
#[derive(Debug, Clone)]
pub(crate) struct Part {
    pub(crate) ops: Vec<RootOp>,
    pub(crate) templates: Vec<BindId>,
    pub(crate) root_defaults: Vec<RootDefaults>,
    pub(crate) declared: Vec<DeclaredVariant>,
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
    strings: Vec<Arc<str>>,
    files: &'a [FileId],
    sources: &'a Sources,
    exprs: usize,
    blocks: usize,
    binds: usize,
    layer: usize,
}

/// Something is wrong with the file; the caller says what.
struct Corrupt;

type Read<T> = Result<T, Corrupt>;

impl Reader<'_> {
    fn byte(&mut self) -> Read<u8> {
        let byte = *self.data.get(self.at).ok_or(Corrupt)?;
        self.at += 1;
        Ok(byte)
    }

    fn bytes(&mut self, n: usize) -> Read<&[u8]> {
        let end = self.at.checked_add(n).ok_or(Corrupt)?;
        let bytes = self.data.get(self.at..end).ok_or(Corrupt)?;
        self.at = end;
        Ok(bytes)
    }

    fn uint(&mut self) -> Read<u64> {
        let mut n = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = self.byte()?;
            n |= u64::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                return Ok(n);
            }
        }
        Err(Corrupt)
    }

    fn id(&mut self) -> Read<usize> {
        usize::try_from(self.uint()?).map_err(|_| Corrupt)
    }

    /// A count of items, each of at least one byte, so that a damaged one
    /// cannot ask for more memory than the file has.
    fn count(&mut self) -> Read<usize> {
        let n = self.id()?;
        if n > self.data.len() - self.at {
            return Err(Corrupt);
        }
        Ok(n)
    }

    fn int(&mut self) -> Read<i128> {
        let raw: [u8; 16] = self.bytes(16)?.try_into().map_err(|_| Corrupt)?;
        Ok(i128::from_le_bytes(raw))
    }

    fn flag(&mut self) -> Read<bool> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Corrupt),
        }
    }

    fn string(&mut self) -> Read<Arc<str>> {
        let index = self.id()?;
        self.strings.get(index).cloned().ok_or(Corrupt)
    }

    fn span(&mut self) -> Read<Span> {
        let file = self.id()?;
        let (start, end) = (self.uint()?, self.uint()?);
        let (start, end) = (
            u32::try_from(start).map_err(|_| Corrupt)?,
            u32::try_from(end).map_err(|_| Corrupt)?,
        );
        if file == 0 {
            return Ok(Span::detached());
        }
        let file = *self.files.get(file - 1).ok_or(Corrupt)?;
        let text = self.sources.file(file).text();
        let fits = |n: u32| text.is_char_boundary(n as usize);
        if start > end || !fits(start) || !fits(end) {
            return Err(Corrupt);
        }
        Ok(Span::new(
            file,
            TextRange::new(TextSize::from(start), TextSize::from(end)),
        ))
    }

    fn list<T>(&mut self, mut read: impl FnMut(&mut Self) -> Read<T>) -> Read<Vec<T>> {
        let n = self.count()?;
        let mut items = Vec::with_capacity(n);
        for _ in 0..n {
            items.push(read(self)?);
        }
        Ok(items)
    }

    fn option<T>(&mut self, read: impl FnOnce(&mut Self) -> Read<T>) -> Read<Option<T>> {
        if self.flag()? {
            read(self).map(Some)
        } else {
            Ok(None)
        }
    }

    fn key(&mut self) -> Read<Key> {
        match self.byte()? {
            0 => self.int().map(Key::Int),
            1 => self.string().map(Key::Str),
            _ => Err(Corrupt),
        }
    }

    fn selector(&mut self) -> Read<Selector> {
        let optional = self.flag()?;
        let kind = match self.byte()? {
            0 => SelectorKind::Field(self.string()?),
            1 => SelectorKind::Element(self.id()?),
            2 => SelectorKind::Accessor(self.int()?),
            3 => SelectorKind::Context(self.list(|r| Ok((r.string()?, r.flag()?, r.span()?)))?),
            _ => return Err(Corrupt),
        };
        Ok(Selector {
            optional,
            kind,
            span: self.span()?,
        })
    }

    fn selectors(&mut self) -> Read<Vec<Selector>> {
        self.list(Self::selector)
    }

    /// An expression's id, as it is in the IR being read into.
    fn expr_id(&mut self) -> Read<ExprId> {
        Ok(self.id()?.saturating_add(self.exprs))
    }

    fn expr_ids(&mut self) -> Read<Vec<ExprId>> {
        self.list(Self::expr_id)
    }

    fn bind_id(&mut self) -> Read<BindId> {
        Ok(self.id()?.saturating_add(self.binds))
    }

    fn set_op(&mut self) -> Read<Option<SetOp>> {
        Ok(match self.byte()? {
            0 => None,
            1 => Some(SetOp::Set),
            2 => Some(SetOp::Declare),
            3 => Some(SetOp::Ensure),
            _ => return Err(Corrupt),
        })
    }

    fn reference(&mut self) -> Read<RefKind> {
        Ok(match self.byte()? {
            0 => RefKind::Below,
            1 => RefKind::SelfValue,
            2 => RefKind::Parent(self.id()?),
            3 => RefKind::Root,
            4 => RefKind::Default(Box::new(DefaultTarget {
                type_name: self.option(Self::string)?,
                root_key: self.option(Self::string)?,
                path: self.list(Self::string)?,
            })),
            5 => RefKind::Bind {
                up: self.id()?,
                target: self.selectors()?.into(),
            },
            6 => RefKind::ValueBind(self.bind_id()?),
            7 => RefKind::Top,
            _ => return Err(Corrupt),
        })
    }

    fn expr(&mut self) -> Read<Expr> {
        let kind = match self.byte()? {
            0 => ExprKind::Unit,
            1 => ExprKind::Bool(self.flag()?),
            2 => ExprKind::Int(Wide(self.int()?)),
            3 => {
                let raw: [u8; 8] = self.bytes(8)?.try_into().map_err(|_| Corrupt)?;
                ExprKind::Float(f64::from_le_bytes(raw))
            }
            4 => {
                let c = u32::try_from(self.uint()?).map_err(|_| Corrupt)?;
                ExprKind::Char(char::from_u32(c).ok_or(Corrupt)?)
            }
            5 => ExprKind::Str(self.string()?),
            6 => {
                let rooted = self.flag()?;
                let count = self.count()?;
                let keys = (0..count)
                    .map(|_| self.string())
                    .collect::<Read<Vec<_>>>()?;
                ExprKind::Id(IdPath::new(rooted, keys).ok_or(Corrupt)?)
            }
            7 => ExprKind::Ref(self.reference()?),
            8 => ExprKind::Block(self.id()?.saturating_add(self.blocks)),
            9 => ExprKind::List(self.expr_ids()?),
            10 => ExprKind::Tuple(self.expr_ids()?),
            11 => ExprKind::Named(Box::new((self.string()?, self.expr_ids()?))),
            12 => ExprKind::UnitName(self.string()?),
            13 => ExprKind::Path(self.expr_id()?, self.selectors()?.into()),
            14 => ExprKind::Neg(self.expr_id()?),
            15 => {
                let op = match self.byte()? {
                    0 => BinOp::Add,
                    1 => BinOp::Sub,
                    2 => BinOp::Mul,
                    3 => BinOp::Div,
                    4 => BinOp::Rem,
                    5 => BinOp::Merge,
                    6 => BinOp::MergeShallow,
                    7 => BinOp::Fallback,
                    8 => BinOp::Union,
                    _ => return Err(Corrupt),
                };
                ExprKind::Binary(op, self.expr_id()?, self.expr_id()?)
            }
            16 => {
                let kind = match self.byte()? {
                    0 => MacroKind::Round,
                    1 => MacroKind::Floor,
                    2 => MacroKind::Ceil,
                    3 => MacroKind::Clamp,
                    _ => return Err(Corrupt),
                };
                let args = self.expr_ids()?;
                if args.len() != kind.arity() {
                    return Err(Corrupt);
                }
                ExprKind::Macro(kind, args.into())
            }
            17 => ExprKind::Error,
            18 => ExprKind::Not(self.expr_id()?),
            19 => ExprKind::Scope(self.expr_id()?),
            _ => return Err(Corrupt),
        };
        Ok(Expr {
            kind,
            span: self.span()?,
        })
    }

    fn entry(&mut self) -> Read<Entry> {
        let op = self.set_op()?;
        let replace = self.flag()?;
        let if_set = self.flag()?;
        let key = self.key()?;
        let key_optional = self.flag()?;
        let path = self.selectors()?;
        let value = self.option(Self::expr_id)?;
        if op.is_some() != value.is_some() || op.is_some() && if_set {
            return Err(Corrupt);
        }
        Ok(Entry {
            op,
            replace,
            if_set,
            key,
            key_optional,
            path,
            value,
            span: self.span()?,
        })
    }

    fn block(&mut self) -> Read<Block> {
        Ok(Block {
            name: self.option(Self::string)?,
            spreads: self.expr_ids()?,
            entries: self.list(Self::entry)?,
            defaults: self.flag()?,
            scope: self.flag()?,
            value_binds: self.list(Self::bind_id)?,
        })
    }

    /// A condition, nesting no deeper than one can be written.
    fn condition(&mut self, depth: usize) -> Read<Cond> {
        if depth > crol_syntax::MAX_NESTING {
            return Err(Corrupt);
        }
        let sub = |r: &mut Self| r.condition(depth + 1).map(Box::new);
        Ok(match self.byte()? {
            0 => {
                let name = self.string()?;
                Cond::Name(name, self.span()?)
            }
            1 => Cond::Not(sub(self)?),
            2 => Cond::And(sub(self)?, sub(self)?),
            3 => Cond::Or(sub(self)?, sub(self)?),
            4 => Cond::Xor(sub(self)?, sub(self)?),
            _ => return Err(Corrupt),
        })
    }

    fn op(&mut self) -> Read<RootOp> {
        let key = self.string()?;
        let op = self.set_op()?;
        let replace = self.flag()?;
        let if_set = self.flag()?;
        let key_optional = self.flag()?;
        let path = self.selectors()?;
        let value = self.option(Self::expr_id)?;
        if op.is_some() != value.is_some() || op.is_some() && if_set {
            return Err(Corrupt);
        }
        Ok(RootOp {
            key,
            op,
            replace,
            if_set,
            key_optional,
            path,
            value,
            span: self.span()?,
            uses_below: self.flag()?,
            layer: self.layer,
            document: self.id()?,
            condition: self.option(|r| r.condition(0))?,
            blocks: self.list(Self::selectors)?,
        })
    }
}

/// Reads what was lowered into `ir`, for the layer at index `layer`, whose
/// documents are `files`, in order.
pub(crate) fn read_lowered(
    bytes: &[u8],
    files: &[FileId],
    sources: &Sources,
    layer: usize,
    ir: &mut Ir,
) -> Result<Part, String> {
    let mut r = Reader {
        data: bytes,
        at: 0,
        strings: Vec::new(),
        files,
        sources,
        exprs: ir.exprs.len(),
        blocks: ir.blocks.len(),
        binds: ir.value_binds.len(),
        layer,
    };
    read_part(&mut r, ir).map_err(|Corrupt| "this `.crob` file is damaged".to_owned())
}

fn read_part(r: &mut Reader<'_>, ir: &mut Ir) -> Read<Part> {
    r.strings = r.list(|r| {
        let length = r.count()?;
        let text = std::str::from_utf8(r.bytes(length)?).map_err(|_| Corrupt)?;
        Ok(Arc::from(text))
    })?;
    let exprs = r.list(Reader::expr)?;
    let blocks = r.list(Reader::block)?;
    let binds = r.list(|r| {
        let name = r.string()?;
        let expr = r.expr_id()?;
        Ok(ValueBind { name, expr })
    })?;
    let templates = r.list(Reader::bind_id)?;
    let ops = r.list(Reader::op)?;
    let root_defaults = r.list(|r| {
        let condition = r.option(|r| r.condition(0))?;
        let span = r.span()?;
        Ok(RootDefaults { condition, span })
    })?;
    let declared = r.list(|r| {
        let group = r.string()?;
        let name = r.string()?;
        let span = r.span()?;
        Ok(DeclaredVariant { group, name, span })
    })?;
    if r.at != r.data.len() {
        return Err(Corrupt);
    }
    let (first_expr, first_block, first_bind) = (r.exprs, r.blocks, r.binds);
    ir.exprs.extend(exprs);
    ir.blocks.extend(blocks);
    ir.value_binds.extend(binds);
    check(ir, first_expr, first_block, first_bind)?;
    let in_exprs = first_expr..ir.exprs.len();
    let in_binds = first_bind..ir.value_binds.len();
    let expr_ok = |id: &Option<ExprId>| id.is_none_or(|id| in_exprs.contains(&id));
    if !templates.iter().all(|id| in_binds.contains(id)) || !ops.iter().all(|op| expr_ok(&op.value))
    {
        return Err(Corrupt);
    }
    Ok(Part {
        ops,
        templates,
        root_defaults,
        declared,
    })
}

/// Checks what evaluation takes for granted: an expression holds only
/// earlier ones, so none holds itself, and none nests too deeply. A value bind
/// may be later, since a reference to one is found at evaluation, where a
/// cycle is an error.
fn check(ir: &Ir, first_expr: usize, first_block: usize, first_bind: usize) -> Read<()> {
    let mut depths = vec![0usize; ir.exprs.len() - first_expr];
    for id in first_expr..ir.exprs.len() {
        let mut deepest = 0;
        let mut holds = |child: ExprId| -> Read<()> {
            if child < first_expr || child >= id {
                return Err(Corrupt);
            }
            deepest = deepest.max(depths[child - first_expr]);
            Ok(())
        };
        match &ir.exprs[id].kind {
            ExprKind::Block(block) => {
                if *block < first_block {
                    return Err(Corrupt);
                }
                let block = ir.blocks.get(*block).ok_or(Corrupt)?;
                for &spread in &block.spreads {
                    holds(spread)?;
                }
                for entry in &block.entries {
                    if let Some(value) = entry.value {
                        holds(value)?;
                    }
                }
                let in_range = |bind: &BindId| (first_bind..ir.value_binds.len()).contains(bind);
                if !block.value_binds.iter().all(in_range) {
                    return Err(Corrupt);
                }
            }
            ExprKind::List(items) | ExprKind::Tuple(items) => {
                for &item in items {
                    holds(item)?;
                }
            }
            ExprKind::Macro(_, items) => {
                for &item in items {
                    holds(item)?;
                }
            }
            ExprKind::Named(named) => {
                for &item in &named.1 {
                    holds(item)?;
                }
            }
            ExprKind::Path(child, _)
            | ExprKind::Neg(child)
            | ExprKind::Not(child)
            | ExprKind::Scope(child) => holds(*child)?,
            ExprKind::Binary(_, lhs, rhs) => {
                holds(*lhs)?;
                holds(*rhs)?;
            }
            ExprKind::Ref(RefKind::ValueBind(bind))
                if !(first_bind..ir.value_binds.len()).contains(bind) =>
            {
                return Err(Corrupt);
            }
            _ => {}
        }
        let depth = deepest + 1;
        if depth > MAX_DEPTH {
            return Err(Corrupt);
        }
        depths[id - first_expr] = depth;
    }
    for bind in ir.value_binds.iter().skip(first_bind) {
        if !(first_expr..ir.exprs.len()).contains(&bind.expr) {
            return Err(Corrupt);
        }
    }
    Ok(())
}
