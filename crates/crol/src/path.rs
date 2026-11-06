//! Typed paths into data (`SPEC.md` 10.14).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::marker::PhantomData;

use crol_eval::{Key, Value, ValueKind};
use indexmap::IndexMap;

/// One step of a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    /// A field or a map's key.
    Key(Key),
    /// An element of a list.
    Index(usize),
    /// An element of a tuple.
    Element(usize),
    /// Into an `Option`, which has no selector.
    Some,
}

/// Where a value is, from a type `B` to a type `T`. It holds no data, so it
/// can be kept and used again. `#[derive(Navigable)]` makes them:
/// `Config::path().options().port()`.
pub struct Path<B, T> {
    steps: Vec<Step>,
    types: PhantomData<fn() -> (B, T)>,
}

impl<B, T> Clone for Path<B, T> {
    fn clone(&self) -> Self {
        Self::new(self.steps.clone())
    }
}

impl<B, T> fmt::Debug for Path<B, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&selectors(&self.steps).unwrap_or_default())
    }
}

impl<B, T> Path<B, T> {
    fn new(steps: Vec<Step>) -> Self {
        Self {
            steps,
            types: PhantomData,
        }
    }

    pub(crate) fn steps(&self) -> &[Step] {
        &self.steps
    }

    pub(crate) fn into_steps(self) -> Vec<Step> {
        self.steps
    }

    fn then<U>(mut self, step: Step) -> Path<B, U> {
        self.steps.push(step);
        Path::new(self.steps)
    }

    /// The path of a field, for `#[derive(Navigable)]`.
    #[doc(hidden)]
    pub fn __field<U>(self, name: &str) -> Path<B, U> {
        self.then(Step::Key(Key::Str(name.into())))
    }

    /// The same place as another type, for `#[serde(flatten)]` and `Box`.
    #[doc(hidden)]
    pub fn __retype<U>(self) -> Path<B, U> {
        Path::new(self.steps)
    }
}

/// The path of a type itself, for [`Navigable::path`].
impl<T> Path<T, T> {
    fn root() -> Self {
        Self::new(Vec::new())
    }
}

/// What a path can be given as: a [`Path`], or where a generated path type
/// has got to.
pub trait IntoPath<B> {
    /// The type the path ends at.
    type Target;

    /// The path.
    fn into_path(self) -> Path<B, Self::Target>;
}

impl<B, T> IntoPath<B> for Path<B, T> {
    type Target = T;

    fn into_path(self) -> Path<B, T> {
        self
    }
}

impl<B, T> IntoPath<B> for &Path<B, T> {
    type Target = T;

    fn into_path(self) -> Path<B, T> {
        self.clone()
    }
}

/// A type that paths can go into. `#[derive(Navigable)]` implements this for
/// a struct with named fields, and for anything else as a type paths end at,
/// so a path does not go into an enum's variants. Lists, maps, tuples and
/// `Option`s have their own steps, and a `Box` is walked through.
pub trait Navigable: Sized {
    /// What a path that has reached this type offers next.
    type Nav<B>;

    /// Goes on from a path that has reached this type.
    fn nav<B>(path: Path<B, Self>) -> Self::Nav<B>;

    /// A path that starts at this type.
    fn path() -> Self::Nav<Self> {
        Self::nav(Path::root())
    }
}

macro_rules! leaves {
    ($($ty:ty),* $(,)?) => {$(
        impl Navigable for $ty {
            type Nav<B> = Path<B, Self>;

            fn nav<B>(path: Path<B, Self>) -> Path<B, Self> {
                path
            }
        }
    )*};
}

leaves!(
    (),
    bool,
    char,
    String,
    f32,
    f64,
    i8,
    i16,
    i32,
    i64,
    i128,
    isize,
    u8,
    u16,
    u32,
    u64,
    u128,
    usize,
);

macro_rules! leaf_sets {
    ($($ty:ident),*) => {$(
        impl<T> Navigable for $ty<T> {
            type Nav<B> = Path<B, Self>;

            fn nav<B>(path: Path<B, Self>) -> Path<B, Self> {
                path
            }
        }
    )*};
}

leaf_sets!(BTreeSet, HashSet);

/// A key of a map, for [`MapPath::key`].
pub trait PathKey {
    /// The key.
    fn key(self) -> Key;
}

impl PathKey for &str {
    fn key(self) -> Key {
        Key::Str(self.into())
    }
}

impl PathKey for String {
    fn key(self) -> Key {
        Key::Str(self.into())
    }
}

impl PathKey for &String {
    fn key(self) -> Key {
        Key::Str(self.as_str().into())
    }
}

macro_rules! integer_keys {
    ($($ty:ty),*) => {$(
        impl PathKey for $ty {
            fn key(self) -> Key {
                Key::Int(i128::from(self))
            }
        }
    )*};
}

integer_keys!(i8, i16, i32, i64, i128, u8, u16, u32, u64);

/// Implements the traits of a path type that wraps a [`Path`].
macro_rules! path_type {
    ($name:ident<$($param:ident),*> => $target:ty) => {
        impl<B, $($param),*> Clone for $name<B, $($param),*> {
            fn clone(&self) -> Self {
                Self(self.0.clone())
            }
        }

        impl<B, $($param),*> IntoPath<B> for $name<B, $($param),*> {
            type Target = $target;

            fn into_path(self) -> Path<B, $target> {
                self.0
            }
        }

        impl<B, $($param),*> IntoPath<B> for &$name<B, $($param),*> {
            type Target = $target;

            fn into_path(self) -> Path<B, $target> {
                self.0.clone()
            }
        }
    };
}

/// A path that has reached a list.
pub struct ListPath<B, I>(Path<B, Vec<I>>);

path_type!(ListPath<I> => Vec<I>);

impl<B, I: Navigable> ListPath<B, I> {
    /// The element at an index. This is positional: a saved write goes to
    /// the index, whatever element is there by then.
    pub fn at(self, index: usize) -> I::Nav<B> {
        I::nav(self.0.then(Step::Index(index)))
    }
}

impl<I> Navigable for Vec<I> {
    type Nav<B> = ListPath<B, I>;

    fn nav<B>(path: Path<B, Self>) -> ListPath<B, I> {
        ListPath(path)
    }
}

/// A path that has reached an `Option`.
pub struct OptionPath<B, I>(Path<B, Option<I>>);

path_type!(OptionPath<I> => Option<I>);

impl<B, I: Navigable> OptionPath<B, I> {
    /// What the `Option` holds. Where it is `None`, the path is not set for a
    /// read, and a write goes through it, as `?` does (`SPEC.md` 5.1).
    pub fn some(self) -> I::Nav<B> {
        I::nav(self.0.then(Step::Some))
    }
}

impl<I> Navigable for Option<I> {
    type Nav<B> = OptionPath<B, I>;

    fn nav<B>(path: Path<B, Self>) -> OptionPath<B, I> {
        OptionPath(path)
    }
}

impl<T: Navigable> Navigable for Box<T> {
    type Nav<B> = T::Nav<B>;

    fn nav<B>(path: Path<B, Self>) -> T::Nav<B> {
        T::nav(path.__retype())
    }
}

/// A path that has reached a map.
pub struct MapPath<B, M, I>(Path<B, M>, PhantomData<fn() -> I>);

impl<B, M, I> Clone for MapPath<B, M, I> {
    fn clone(&self) -> Self {
        Self(self.0.clone(), PhantomData)
    }
}

impl<B, M, I> IntoPath<B> for MapPath<B, M, I> {
    type Target = M;

    fn into_path(self) -> Path<B, M> {
        self.0
    }
}

impl<B, M, I> IntoPath<B> for &MapPath<B, M, I> {
    type Target = M;

    fn into_path(self) -> Path<B, M> {
        self.0.clone()
    }
}

impl<B, M, I: Navigable> MapPath<B, M, I> {
    /// The value at a key.
    pub fn key(self, key: impl PathKey) -> I::Nav<B> {
        I::nav(self.0.then(Step::Key(key.key())))
    }
}

impl<K, I> Navigable for BTreeMap<K, I> {
    type Nav<B> = MapPath<B, Self, I>;

    fn nav<B>(path: Path<B, Self>) -> MapPath<B, Self, I> {
        MapPath(path, PhantomData)
    }
}

impl<K, I, S> Navigable for HashMap<K, I, S> {
    type Nav<B> = MapPath<B, Self, I>;

    fn nav<B>(path: Path<B, Self>) -> MapPath<B, Self, I> {
        MapPath(path, PhantomData)
    }
}

impl<K, I, S> Navigable for IndexMap<K, I, S> {
    type Nav<B> = MapPath<B, Self, I>;

    fn nav<B>(path: Path<B, Self>) -> MapPath<B, Self, I> {
        MapPath(path, PhantomData)
    }
}

macro_rules! tuples {
    ($($name:ident<$($item:ident . $method:ident . $index:tt),*>),* $(,)?) => {$(
        /// A path that has reached a tuple.
        pub struct $name<B, $($item),*>(Path<B, ($($item,)*)>);

        path_type!($name<$($item),*> => ($($item,)*));

        impl<B, $($item: Navigable),*> $name<B, $($item),*> {
            $(
                /// An element of the tuple.
                pub fn $method(self) -> $item::Nav<B> {
                    $item::nav(self.0.then(Step::Element($index)))
                }
            )*
        }

        impl<$($item),*> Navigable for ($($item,)*) {
            type Nav<B> = $name<B, $($item),*>;

            fn nav<B>(path: Path<B, Self>) -> $name<B, $($item),*> {
                $name(path)
            }
        }
    )*};
}

tuples!(
    Tuple1Path<A.t0.0>,
    Tuple2Path<A.t0.0, C.t1.1>,
    Tuple3Path<A.t0.0, C.t1.1, D.t2.2>,
    Tuple4Path<A.t0.0, C.t1.1, D.t2.2, E.t3.3>,
);

/// Steps as selectors, the first as a key if `root` is set. `None` if the
/// first step is no string key, which no root key is without.
fn write_steps(steps: &[Step], mut root: bool) -> Option<String> {
    let mut text = String::new();
    let mut through_option = false;
    for step in steps {
        if *step == Step::Some {
            through_option = true;
            continue;
        }
        // An `Option` may be `None`, or a plain value (`SPEC.md` 5.1).
        if std::mem::take(&mut through_option) {
            text.push('?');
        }
        match step {
            Step::Some => {}
            Step::Key(key @ Key::Str(_)) if root => text.push_str(&key.to_string()),
            Step::Key(key @ Key::Str(name)) => {
                if crol_eval::is_identifier(name) {
                    text.push('.');
                    text.push_str(name);
                } else {
                    text.push_str(&format!("[{key}]"));
                }
            }
            _ if root => return None,
            Step::Key(Key::Int(i)) => text.push_str(&format!("[{i}]")),
            Step::Index(i) => text.push_str(&format!("[{i}]")),
            Step::Element(i) => text.push_str(&format!(".{i}")),
        }
        root = false;
    }
    Some(text)
}

fn selectors(steps: &[Step]) -> Option<String> {
    write_steps(steps, false)
}

/// The target a path's steps are written as (`SPEC.md` 5.1); `None` for the
/// root, or a path that does not start at a root key.
pub(crate) fn target(steps: &[Step]) -> Option<String> {
    write_steps(steps, true).filter(|text| !text.is_empty())
}

/// The value a step leads to, through `Some(...)`.
fn step_into<'v>(value: &'v Value, step: &Step) -> Option<&'v Value> {
    let inner = unwrap_some(value);
    match (&inner.kind, step) {
        (ValueKind::UnitName(name), Step::Some) if &**name == "None" => None,
        (_, Step::Some) => Some(inner),
        (ValueKind::Keyed(keyed), Step::Key(key)) => keyed.entries.get(key),
        (ValueKind::List(items), Step::Index(i)) => items.get(*i),
        (ValueKind::Tuple(items) | ValueKind::NamedTuple(_, items), Step::Element(i)) => {
            items.get(*i)
        }
        _ => None,
    }
}

fn unwrap_some(value: &Value) -> &Value {
    match &value.kind {
        ValueKind::NamedTuple(name, items) if &**name == "Some" && items.len() == 1 => &items[0],
        _ => value,
    }
}

/// The value at the steps after a root key.
pub(crate) fn value_at<'v>(value: &'v Value, steps: &[Step]) -> Option<&'v Value> {
    steps.iter().try_fold(value, step_into)
}
