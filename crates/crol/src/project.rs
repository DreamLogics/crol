//! The project file, `project.crol` (`SPEC.md` 12.4): what tools that are
//! not the application (the CLI, the language server) need to know to
//! evaluate its data.

use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::compose::Variant;
use crate::error::Error;
use crate::loader::SourceId;
use crate::mode::Mode;
use crate::schema::{
    FieldDefault, Registry, Repr, Schema, TypeDef, TypeExpr, TypeRef, VariantDef, VariantShape,
    field, recorded_default, struct_def,
};

/// The file name tools look for.
pub const FILE_NAME: &str = "project.crol";

/// A project file: what tools that are not the application need to know to
/// evaluate its data (`SPEC.md` 12.4).
///
/// A project has one or more layer stacks, each with its own layers,
/// schema, root, options and `files`. It is written either with `stacks`,
/// or with `layers` and the rest at the top, as a project with one stack.
/// [`Project::read`] moves the second form into `stacks` too, under
/// [`DEFAULT_STACK`], so that what reads a project need only look there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    /// The layers, lowest first, of a project with one stack.
    #[serde(default)]
    pub layers: Option<ProjectLayers>,
    /// The variants (`SPEC.md` 8.1), of every stack.
    #[serde(default)]
    pub variants: Vec<Variant>,
    /// Variant parents: child to parent.
    #[serde(default)]
    pub parents: IndexMap<String, String>,
    /// Stand-in values of the constants the program gives (`SPEC.md`
    /// 6.1.4), which tools evaluate the data of a project with one stack
    /// with.
    #[serde(default)]
    pub constants: IndexMap<String, crate::RawValue>,
    /// How the data of a project with one stack is evaluated and checked.
    #[serde(default)]
    pub options: ProjectOptions,
    /// The schema of a project with one stack: a module path written as a
    /// string, a URL, or a crate.
    #[serde(default)]
    pub schema: Option<String>,
    /// The path of the type of root values of a project with one stack;
    /// default: the schema's root.
    #[serde(default)]
    pub root: Option<String>,
    /// Value-mode documents of a project with one stack, and their types.
    #[serde(default)]
    pub files: Vec<FilePattern>,
    /// The layer stacks, by name, each checked and composed on its own.
    #[serde(default)]
    pub stacks: IndexMap<String, ProjectStack>,
}

/// The name [`Project::read`] gives the one stack of a project written
/// without `stacks`.
pub const DEFAULT_STACK: &str = "default";

/// A layer stack: layers with their own schema, root, options and value
/// documents, checked and composed on their own (`SPEC.md` 12.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectStack {
    /// The layers, lowest first.
    pub layers: ProjectLayers,
    /// How the data is evaluated and checked.
    #[serde(default)]
    pub options: ProjectOptions,
    /// The schema: a module path written as a string, a URL, or a crate.
    #[serde(default)]
    pub schema: Option<String>,
    /// The path of the type of root values; default: the schema's root.
    #[serde(default)]
    pub root: Option<String>,
    /// Value-mode documents and their types.
    #[serde(default)]
    pub files: Vec<FilePattern>,
    /// Stand-in values of the constants the program gives (`SPEC.md`
    /// 6.1.4), which tools evaluate the stack's data with.
    #[serde(default)]
    pub constants: IndexMap<String, crate::RawValue>,
}

/// Where the layers come from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProjectLayers {
    /// The layers, listed.
    List(Vec<ProjectLayer>),
    /// A command that prints the layer list as a CROL value, run in the
    /// project's directory.
    Command(Vec<String>),
}

/// A layer of a project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectLayer {
    /// The layer's name.
    pub name: String,
    /// Its module path (`SPEC.md` 4.2), from the project's directory.
    pub dir: String,
    /// Generated or vendored data: loaded, but without diagnostics or code
    /// actions in the editor.
    #[serde(default)]
    pub read_only: bool,
}

/// Options of a project, as `CrolConfig` has them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectOptions {
    /// Any value is accepted where an `Option` is expected, as `Some`.
    pub implicit_some: bool,
    /// Keys a struct does not have are ignored.
    pub allow_unknown_fields: bool,
    /// Every field must be written unless the value has `..`.
    pub require_complete: bool,
    /// Let a path go through a field left out, from its default.
    pub init_defaults: bool,
    /// Evaluate the base and one context for each variant, not every
    /// combination.
    pub each_variant: bool,
    /// How many contexts may be evaluated.
    pub max_contexts: usize,
    /// The contexts to evaluate; default: every one.
    pub contexts: Option<Vec<Vec<String>>>,
    /// The variant keys rule: `Any`, `NoNewKeys` or `SameKeys`.
    pub variant_keys: String,
}

impl Default for ProjectOptions {
    fn default() -> Self {
        Self {
            implicit_some: true,
            allow_unknown_fields: false,
            require_complete: true,
            init_defaults: false,
            each_variant: false,
            max_contexts: 4096,
            contexts: None,
            variant_keys: "Any".to_owned(),
        }
    }
}

impl ProjectOptions {
    /// What checking against the schema takes of them.
    pub fn check_options(&self) -> crate::CheckOptions {
        crate::CheckOptions {
            implicit_some: self.implicit_some,
            allow_unknown_fields: self.allow_unknown_fields,
            require_complete: self.require_complete,
        }
    }

    /// The variant keys rule.
    pub fn variant_keys(&self) -> Result<crol_eval::VariantKeys, Error> {
        match self.variant_keys.as_str() {
            "Any" => Ok(crol_eval::VariantKeys::Any),
            "NoNewKeys" => Ok(crol_eval::VariantKeys::NoNewKeys),
            "SameKeys" => Ok(crol_eval::VariantKeys::SameKeys),
            other => Err(Error::new(format!(
                "project: `variant_keys` is `Any`, `NoNewKeys` or `SameKeys`, not `{other}`"
            ))),
        }
    }
}

/// Value-mode documents of a type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FilePattern {
    /// A module path whose segments may hold `*`, or be `**`.
    pub glob: String,
    /// The schema path of their type.
    #[serde(rename = "type")]
    pub type_path: String,
}

impl FilePattern {
    /// The glob's segments, from the project's directory.
    fn segments(&self) -> Vec<&str> {
        let glob = self.glob.strip_prefix("::").unwrap_or(&self.glob);
        glob.split("::").collect()
    }

    /// Checks the form of the glob (`SPEC.md` 12.4).
    fn check(&self) -> Result<(), Error> {
        let invalid =
            |why: &str| Error::new(format!("invalid glob `{}` in `files`: {why}", self.glob));
        let segments = self.segments();
        if segments.last() == Some(&"**") {
            return Err(invalid(
                "its last segment names files, and `**` directories",
            ));
        }
        for segment in segments {
            if segment.is_empty() {
                return Err(invalid("a segment is empty"));
            }
            if segment == "super" {
                return Err(invalid("it starts from the project's directory"));
            }
            if segment == "**" {
                continue;
            }
            if segment.contains("**") {
                return Err(invalid("`**` is a whole segment"));
            }
            if !segment
                .chars()
                .all(|c| c == '*' || c == '_' || c.is_alphanumeric())
            {
                return Err(invalid(&format!("`{segment}` is not a name")));
            }
        }
        Ok(())
    }

    /// Whether the glob matches the `.crol` file `stem` in `directories`
    /// below the project's directory (`SPEC.md` 12.4).
    fn matches(&self, directories: &[String], stem: &str) -> bool {
        let segments = self.segments();
        let Some((last, above)) = segments.split_last() else {
            return false;
        };
        name_matches(last, stem)
            && wildcard(
                above,
                directories,
                |segment| *segment == "**",
                |segment, directory| name_matches(segment, directory),
            )
    }

    /// Whether some file matches both globs, by their form alone.
    fn overlaps(&self, other: &Self) -> bool {
        let (a, b) = (self.segments(), other.segments());
        let (Some((a_last, a_above)), Some((b_last, b_above))) = (a.split_last(), b.split_last())
        else {
            return false;
        };
        let chars = |s: &str| s.chars().collect::<Vec<char>>();
        let names_meet =
            |x: &&str, y: &&str| both_match(&chars(x), &chars(y), |c| *c == '*', |a, b| a == b);
        names_meet(a_last, b_last) && both_match(a_above, b_above, |s| *s == "**", names_meet)
    }
}

/// Whether some sequence of items matches both patterns, where what
/// `is_star` says of stands for any number of items, and `meet` says
/// whether two others can stand for the same item.
fn both_match<P>(
    a: &[P],
    b: &[P],
    is_star: impl Fn(&P) -> bool,
    meet: impl Fn(&P, &P) -> bool,
) -> bool {
    // `reached[i][j]`: the first `i` of `a` and `j` of `b` can match the
    // same items.
    let mut reached = vec![vec![false; b.len() + 1]; a.len() + 1];
    reached[0][0] = true;
    for i in 0..=a.len() {
        for j in 0..=b.len() {
            if !reached[i][j] {
                continue;
            }
            let a_star = a.get(i).is_some_and(&is_star);
            let b_star = b.get(j).is_some_and(&is_star);
            // A star that stands for nothing more.
            if a_star {
                reached[i + 1][j] = true;
            }
            if b_star {
                reached[i][j + 1] = true;
            }
            if let (Some(x), Some(y)) = (a.get(i), b.get(j)) {
                // A star that stands for what the other stands for, and
                // goes on.
                if a_star {
                    reached[i][j + 1] = true;
                }
                if b_star {
                    reached[i + 1][j] = true;
                }
                if !a_star && !b_star && meet(x, y) {
                    reached[i + 1][j + 1] = true;
                }
            }
        }
    }
    reached[a.len()][b.len()]
}

/// Whether a glob segment matches a name: `*` is any part of it.
fn name_matches(segment: &str, name: &str) -> bool {
    let segment: Vec<char> = segment.chars().collect();
    let name: Vec<char> = name.chars().collect();
    wildcard(&segment, &name, |c| *c == '*', |a, b| a == b)
}

/// Whether `pattern` matches all of `items`, where what `is_star` says of
/// stands for any number of items, and `fits` says of the rest which item
/// each stands for. It only goes back to the last star, so it takes no more
/// than the two lengths multiplied, whatever a glob holds.
fn wildcard<P, T>(
    pattern: &[P],
    items: &[T],
    is_star: impl Fn(&P) -> bool,
    fits: impl Fn(&P, &T) -> bool,
) -> bool {
    let (mut at, mut item) = (0, 0);
    let mut last_star = None;
    while item < items.len() {
        match pattern.get(at) {
            Some(star) if is_star(star) => {
                last_star = Some((at, item));
                at += 1;
            }
            Some(one) if fits(one, &items[item]) => {
                at += 1;
                item += 1;
            }
            _ => {
                let Some((star, from)) = last_star else {
                    return false;
                };
                last_star = Some((star, from + 1));
                at = star + 1;
                item = from + 1;
            }
        }
    }
    pattern[at..].iter().all(is_star)
}

impl Project {
    /// The project file in `start` or the nearest directory above it
    /// (`SPEC.md` 12.4).
    pub fn find(start: &Path) -> Option<PathBuf> {
        start
            .ancestors()
            .map(|dir| dir.join(FILE_NAME))
            .find(|candidate| candidate.is_file())
    }

    /// Reads a project file. It is a value document, so a `.crod` file is an
    /// error (`SPEC.md` 4). A project written without `stacks` gets one,
    /// [`DEFAULT_STACK`]. A file that two stacks claim is an error at both
    /// places that claim it (`SPEC.md` 12.4).
    pub fn read(path: &Path) -> Result<Self, Error> {
        Mode::Value
            .check_path(path)
            .map_err(|error| error.with_source_id(SourceId::from_path(path)))?;
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::new(format!("cannot read {}: {e}", path.display())))?;
        Self::from_text(&path.display().to_string(), &text)
    }

    /// [`Project::read`] of the text of a project file called `name`.
    pub fn from_text(name: &str, text: &str) -> Result<Self, Error> {
        let in_file = |e: Error| e.in_file(name);
        let mut project: Self = crate::from_named_str(name, text).map_err(in_file)?;
        project.gather_stacks().map_err(in_file)?;
        for stack in project.stacks.values() {
            for pattern in &stack.files {
                pattern.check().map_err(in_file)?;
            }
        }
        if let Some(error) = project.claimed_twice(name, text) {
            return Err(error);
        }
        Ok(project)
    }

    /// Moves a project with one stack, written at the top, into `stacks`.
    fn gather_stacks(&mut self) -> Result<(), Error> {
        let at_top = self.layers.is_some()
            || self.schema.is_some()
            || self.root.is_some()
            || !self.files.is_empty()
            || !self.constants.is_empty()
            || self.options != ProjectOptions::default();
        if !self.stacks.is_empty() {
            if at_top {
                return Err(Error::new(
                    "project: with `stacks`, `layers`, `options`, `schema`, `root`, `files` and `constants` belong in each stack",
                ));
            }
            for (name, stack) in &self.stacks {
                if !crol_eval::is_identifier(name) {
                    return Err(Error::new(format!(
                        "project: the stack name `{name}` is not an identifier"
                    )));
                }
                check_constant_names(&stack.constants)?;
            }
            return Ok(());
        }
        let Some(layers) = self.layers.take() else {
            return Err(Error::new("project: it has neither `layers` nor `stacks`"));
        };
        let stack = ProjectStack {
            layers,
            options: std::mem::take(&mut self.options),
            schema: self.schema.take(),
            root: self.root.take(),
            files: std::mem::take(&mut self.files),
            constants: std::mem::take(&mut self.constants),
        };
        check_constant_names(&stack.constants)?;
        self.stacks.insert(DEFAULT_STACK.to_owned(), stack);
        Ok(())
    }

    /// The error for files that two stacks claim, at each glob or layer
    /// directory that claims them, if any are (`SPEC.md` 12.4). Globs are
    /// compared by form, so this needs no directory listed; so are layer
    /// directories, those listed.
    fn claimed_twice(&self, name: &str, text: &str) -> Option<Error> {
        let stacks: Vec<(&String, &ProjectStack)> = self.stacks.iter().collect();
        let mut found = Vec::new();
        for (i, (a_name, a)) in stacks.iter().enumerate() {
            for (b_name, b) in &stacks[i + 1..] {
                for (a_at, a_glob) in a.files.iter().enumerate() {
                    for (b_at, b_glob) in b.files.iter().enumerate() {
                        if a_glob.overlaps(b_glob) {
                            found.push((
                                [
                                    (*a_name, "files", a_at, "glob"),
                                    (*b_name, "files", b_at, "glob"),
                                ],
                                [&a_glob.glob, &b_glob.glob],
                                "matches files that",
                            ));
                        }
                    }
                }
                let (ProjectLayers::List(a_layers), ProjectLayers::List(b_layers)) =
                    (&a.layers, &b.layers)
                else {
                    continue;
                };
                for (a_at, a_layer) in a_layers.iter().enumerate() {
                    for (b_at, b_layer) in b_layers.iter().enumerate() {
                        if layer_dirs_overlap(&a_layer.dir, &b_layer.dir) {
                            found.push((
                                [
                                    (*a_name, "layers", a_at, "dir"),
                                    (*b_name, "layers", b_at, "dir"),
                                ],
                                [&a_layer.dir, &b_layer.dir],
                                "holds files that",
                            ));
                        }
                    }
                }
            }
        }
        if found.is_empty() {
            return None;
        }
        let mut sources = crol_eval::Sources::new();
        let file = sources.add(name, text);
        let value = crol_eval::evaluate_value(&sources, file).value;
        let mut diagnostics = Vec::new();
        for (places, written, verb) in found {
            for (this, other) in [(0, 1), (1, 0)] {
                let (stack, list, index, field) = places[this];
                let span = value
                    .as_ref()
                    .and_then(|v| claim_span(v, stack, list, index, field))
                    .unwrap_or(crol_eval::Span::new(file, Default::default()));
                let message = format!(
                    "project: `{}` of stack `{stack}` {verb} `{}` of stack `{}` claims too; a file belongs to one stack",
                    written[this], written[other], places[other].0,
                );
                diagnostics.push(crol_eval::Diagnostic::error(span, message));
            }
        }
        Some(Error::from_diagnostics(&sources, &diagnostics))
    }

    /// The names of the stacks, in the order written.
    pub fn stack_names(&self) -> Vec<&str> {
        self.stacks.keys().map(String::as_str).collect()
    }

    /// The stack called `name`; an error that lists the stacks if there is
    /// none.
    pub fn stack(&self, name: &str) -> Result<&ProjectStack, Error> {
        self.stacks.get(name).ok_or_else(|| {
            Error::new(format!(
                "project: there is no stack `{name}`; the stacks are {}",
                self.listed_stacks()
            ))
        })
    }

    /// The stacks, for a message: `` `theme`, `ui` ``.
    pub fn listed_stacks(&self) -> String {
        let names: Vec<String> = self.stacks.keys().map(|n| format!("`{n}`")).collect();
        names.join(", ")
    }

    /// The stack whose `files` match the value document at `path`, with the
    /// type its first glob that matches gives it, for a project in `dir`
    /// (`SPEC.md` 12.4). At most one stack's do, as [`Project::read`]
    /// checks.
    pub fn file_stack(&self, dir: &Path, path: &Path) -> Option<(&str, &ProjectStack, &str)> {
        self.stacks.iter().find_map(|(name, stack)| {
            let type_path = stack.file_type(dir, path)?;
            Some((name.as_str(), stack, type_path))
        })
    }

    /// The stack whose layers hold `path`, a file or directory, for a project
    /// in `dir`: the one with a layer directory that is `path` or above it,
    /// or a layer file that is it, with or without `.crod`, as a module path
    /// names it. Listed layers are tried first; the layer commands of stacks
    /// that have one run only if none of those hold it, and a command that
    /// fails is an error only if no other stack holds it.
    pub fn layer_stack(&self, dir: &Path, path: &Path) -> Result<Option<&str>, Error> {
        // Canonical on both sides, also where a file does not exist, so that
        // a relative project directory, a symbolic link above the project
        // (`/var` on macOS) or Windows' `\\?\` form compare alike.
        let canonical = |p: &Path| canonical_lenient(p);
        let path = canonical(path);
        let as_module_file = canonical(&path.with_extension("crod"));
        let base = crate::module_path::current_if_empty(dir);
        let holds = |layers: &[ProjectLayer]| {
            layers.iter().any(|layer| {
                let module = layer.dir.strip_prefix("::").unwrap_or(&layer.dir);
                let mut at = base.to_path_buf();
                at.extend(module.split("::"));
                let file = canonical(&at.with_extension("crod"));
                path == file || as_module_file == file || path.starts_with(canonical(&at))
            })
        };
        for (name, stack) in &self.stacks {
            if let ProjectLayers::List(layers) = &stack.layers
                && holds(layers)
            {
                return Ok(Some(name));
            }
        }
        let mut failed = None;
        for (name, stack) in &self.stacks {
            if let ProjectLayers::Command(_) = &stack.layers {
                match stack.layer_list(dir) {
                    Ok(layers) if holds(&layers) => return Ok(Some(name)),
                    Ok(_) => {}
                    Err(error) => {
                        failed.get_or_insert(error);
                    }
                }
            }
        }
        match failed {
            Some(error) => Err(error),
            None => Ok(None),
        }
    }

    /// The errors for the `#![extends(...)]` of the document at `document`,
    /// with `text`, that name a file of another stack's layers, if the
    /// document is in the layers of `stack`, for a project in `dir`: each
    /// with where its module path is written. A stack's layers extend only
    /// files of their own or of no stack (`SPEC.md` 12.4).
    pub fn cross_stack_extends(
        &self,
        dir: &Path,
        stack: &str,
        document: &Path,
        text: &str,
    ) -> Vec<(crol_syntax::TextRange, String)> {
        if self.stacks.len() < 2 {
            return Vec::new();
        }
        let attributes = crate::compose::extends_attributes(text);
        if attributes.is_empty() || self.layer_stack(dir, document).ok().flatten() != Some(stack) {
            return Vec::new();
        }
        let base = crate::module_path::current_if_empty(dir);
        let mut found = Vec::new();
        for (written, range) in attributes {
            let (mut at, rest) = match written.strip_prefix("::") {
                Some(rest) => (base.to_path_buf(), rest),
                None => (
                    document
                        .parent()
                        .map_or_else(|| base.to_path_buf(), Path::to_path_buf),
                    written.as_str(),
                ),
            };
            for segment in rest.split("::") {
                match segment {
                    "super" => {
                        at.pop();
                    }
                    name => at.push(name),
                }
            }
            if let Ok(Some(other)) = self.layer_stack(dir, &at)
                && other != stack
            {
                found.push((
                    range,
                    format!(
                        "extends: `{written}` is in the layers of the stack `{other}`; the layers of `{stack}` extend only files of their own stack"
                    ),
                ));
            }
        }
        found
    }

    /// The declared variants.
    pub fn variants(&self) -> Result<crol_eval::Variants, Error> {
        let mut variants = crol_eval::Variants::new();
        let project = |message: String| Error::new(format!("project: {message}"));
        for variant in &self.variants {
            match variant {
                Variant::Discrete(name) => variants.add_discrete(name).map_err(project)?,
                Variant::Grouped(names) => variants.add_grouped(names).map_err(project)?,
                Variant::Open(group) => variants.add_open_group(group).map_err(project)?,
            }
        }
        for (child, parent) in &self.parents {
            variants.add_parent(child, parent).map_err(project)?;
        }
        Ok(variants)
    }
}

/// The span of `field` of the `index`th entry of `list` of the stack
/// `stack` in a project file's value.
fn claim_span(
    project: &crol_eval::Value,
    stack: &str,
    list: &str,
    index: usize,
    field: &str,
) -> Option<crol_eval::Span> {
    use crol_eval::{Key, ValueKind};
    let get = |value: &crol_eval::Value, key: &str| -> Option<crol_eval::Value> {
        let ValueKind::Keyed(keyed) = &value.kind else {
            return None;
        };
        keyed.entries.get(&Key::Str(key.into())).cloned()
    };
    let stacks = get(project, "stacks")?;
    let entries = get(&get(&stacks, stack)?, list)?;
    let items = match &entries.kind {
        ValueKind::List(items) => items.clone(),
        // `List([...])`
        ValueKind::NamedTuple(_, inner) => match inner.first().map(|v| &v.kind) {
            Some(ValueKind::List(items)) => items.clone(),
            _ => return None,
        },
        _ => return None,
    };
    Some(get(items.get(index)?, field)?.span)
}

/// A path made absolute and canonical as far as it exists: the nearest
/// ancestor that exists, canonical, with the rest of the path after it.
fn canonical_lenient(path: &Path) -> PathBuf {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut rest = Vec::new();
    let mut at = path.as_path();
    loop {
        if let Ok(found) = at.canonicalize() {
            return rest.into_iter().rev().fold(found, |p, name| p.join(name));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                at = parent;
            }
            _ => return path,
        }
    }
}

/// Whether two layer directories, module paths from the project's
/// directory, can hold the same file: one is the other, or above it.
fn layer_dirs_overlap(a: &str, b: &str) -> bool {
    let segments = |dir: &str| -> Vec<String> {
        let dir = dir.strip_prefix("::").unwrap_or(dir);
        dir.split("::").map(str::to_owned).collect()
    };
    let (a, b) = (segments(a), segments(b));
    a.starts_with(&b) || b.starts_with(&a)
}

impl ProjectStack {
    /// The stand-in values of the stack's constants (`SPEC.md` 6.1.4,
    /// 12.4).
    pub fn constants(&self) -> crol_eval::Constants {
        let mut constants = crol_eval::Constants::new();
        for (name, value) in &self.constants {
            constants.insert(name.as_str(), value.0.clone());
        }
        constants
    }

    /// The type `files` gives the value document at `path`, for a project in
    /// `dir`: that of the first glob that matches (`SPEC.md` 12.4). The file
    /// need not exist: this only looks at its name, so that nothing depends
    /// on what a directory holds.
    pub fn file_type(&self, dir: &Path, path: &Path) -> Option<&str> {
        let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        let path = canonical(path);
        let relative = path.strip_prefix(canonical(dir)).ok()?;
        let mut names: Vec<String> = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let file = names.pop()?;
        let stem = file.strip_suffix(".crol")?;
        let mut hidden = names.iter().map(String::as_str).chain([stem]);
        if stem.is_empty() || hidden.any(|name| name.starts_with('.')) {
            return None;
        }
        self.files
            .iter()
            .find(|pattern| pattern.matches(&names, stem))
            .map(|pattern| pattern.type_path.as_str())
    }

    /// The layers, running the layer command if there is one. `dir` is the
    /// project file's directory.
    pub fn layer_list(&self, dir: &Path) -> Result<Vec<ProjectLayer>, Error> {
        match &self.layers {
            ProjectLayers::List(layers) => Ok(layers.clone()),
            ProjectLayers::Command(command) => {
                let (program, arguments) = command
                    .split_first()
                    .ok_or_else(|| Error::new("project: the layer command is empty"))?;
                let output = std::process::Command::new(program)
                    .args(arguments)
                    .current_dir(crate::module_path::current_if_empty(dir))
                    .output()
                    .map_err(|e| Error::new(format!("project: cannot run `{program}`: {e}")))?;
                if !output.status.success() {
                    return Err(Error::new(format!(
                        "project: `{}` failed: {}",
                        command.join(" "),
                        String::from_utf8_lossy(&output.stderr).trim()
                    )));
                }
                let text = String::from_utf8_lossy(&output.stdout);
                crate::from_named_str("<output>", &text).map_err(|e| {
                    Error::new(format!(
                        "project: the output of the layer command `{}`: {e}",
                        command.join(" ")
                    ))
                })
            }
        }
    }

    /// The schema file, for a project in `dir`. A URL or crate is resolved
    /// as [`crate::schema::resolve_remote`] does, with `fetch`.
    pub fn schema_path(&self, dir: &Path, fetch: bool) -> Result<Option<PathBuf>, Error> {
        let Some(schema) = &self.schema else {
            return Ok(None);
        };
        if schema.starts_with("https://") || schema.starts_with("crate:") {
            return crate::schema::resolve_remote(schema, dir, fetch)
                .map(Some)
                .map_err(|e| Error::new(format!("project: {e}")));
        }
        let module = if schema.starts_with("::") {
            schema.clone()
        } else {
            format!("::{schema}")
        };
        crate::module_path::resolve(&module, dir, None, Mode::Value).map(Some)
    }
}

/// Every constant a stack gives a stand-in value must be one data can name.
fn check_constant_names(constants: &IndexMap<String, crate::RawValue>) -> Result<(), Error> {
    match constants
        .keys()
        .find(|name| !crol_eval::is_identifier(name))
    {
        Some(name) => Err(Error::new(format!(
            "project: the constant name `{name}` is not an identifier"
        ))),
        None => Ok(()),
    }
}

// ---- The project file's schema (`SPEC.md` 12.4) ----

impl Schema for Project {
    fn describe(registry: &mut Registry) -> TypeRef {
        registry.add("crol::project::Project", |registry| {
            let layers = registry.describe::<ProjectLayers>();
            let variants = registry.describe::<Vec<Variant>>();
            let parents = registry.describe::<IndexMap<String, String>>();
            let constants = registry.describe::<IndexMap<String, crate::RawValue>>();
            let options = registry.describe::<ProjectOptions>();
            let files = registry.describe::<Vec<FilePattern>>();
            let stacks = registry.describe::<IndexMap<String, ProjectStack>>();
            let none = || crate::schema::none_value();
            struct_def(
                "Project",
                "A project file: what tools that are not the application need to know to evaluate its data (`SPEC.md` 12.4).",
                vec![
                    ("layers", field(TypeExpr::Option(Box::new(layers)), "The layers, lowest first, of a project with one stack.", none())),
                    ("variants", field(variants, "The variants (`SPEC.md` 8.1), of every stack.", recorded_default(&Vec::<Variant>::new()))),
                    ("parents", field(parents, "Variant parents: child to parent.", recorded_default(&IndexMap::<String, String>::new()))),
                    ("constants", field(constants, "Stand-in values of the constants the program gives (`SPEC.md` 6.1.4), which tools evaluate the data of a project with one stack with.", recorded_default(&IndexMap::<String, String>::new()))),
                    ("options", field(options, "How the data of a project with one stack is evaluated and checked.", recorded_default(&ProjectOptions::default()))),
                    ("schema", field(TypeExpr::Option(Box::new(TypeExpr::String)), "The schema of a project with one stack: a module path written as a string, a URL, or a crate.", none())),
                    ("root", field(TypeExpr::Option(Box::new(TypeExpr::String)), "The path of the type of root values of a project with one stack; default: the schema's root.", none())),
                    ("files", field(files, "Value-mode documents of a project with one stack, and their types.", recorded_default(&Vec::<FilePattern>::new()))),
                    ("stacks", field(stacks, "The layer stacks, by name, each checked and composed on its own.", recorded_default(&IndexMap::<String, ProjectStack>::new()))),
                ],
                None,
            )
        })
    }
}

impl Schema for ProjectStack {
    fn describe(registry: &mut Registry) -> TypeRef {
        registry.add("crol::project::ProjectStack", |registry| {
            let layers = registry.describe::<ProjectLayers>();
            let constants = registry.describe::<IndexMap<String, crate::RawValue>>();
            let options = registry.describe::<ProjectOptions>();
            let files = registry.describe::<Vec<FilePattern>>();
            let none = || crate::schema::none_value();
            struct_def(
                "ProjectStack",
                "A layer stack: layers with their own schema, root, options and value documents, checked and composed on their own (`SPEC.md` 12.4).",
                vec![
                    ("layers", field(layers, "The layers, lowest first.", FieldDefault::Required)),
                    ("options", field(options, "How the data is evaluated and checked.", recorded_default(&ProjectOptions::default()))),
                    ("schema", field(TypeExpr::Option(Box::new(TypeExpr::String)), "The schema: a module path written as a string, a URL, or a crate.", none())),
                    ("root", field(TypeExpr::Option(Box::new(TypeExpr::String)), "The path of the type of root values; default: the schema's root.", none())),
                    ("files", field(files, "Value-mode documents and their types.", recorded_default(&Vec::<FilePattern>::new()))),
                    ("constants", field(constants, "Stand-in values of the constants the program gives (`SPEC.md` 6.1.4), which tools evaluate the stack's data with.", recorded_default(&IndexMap::<String, String>::new()))),
                ],
                None,
            )
        })
    }
}

impl Schema for ProjectLayers {
    fn describe(registry: &mut Registry) -> TypeRef {
        registry.add("crol::project::ProjectLayers", |registry| {
            let list = registry.describe::<Vec<ProjectLayer>>();
            let variant = |shape, doc: &str| VariantDef {
                shape,
                doc: doc.to_owned(),
                aliases: Vec::new(),
                explicit_defaults: None,
                scope: false,
            };
            TypeDef::Enum {
                name: "ProjectLayers".to_owned(),
                doc: "Where the layers come from.".to_owned(),
                source: (file!().replace('\\', "/"), line!()),
                repr: Repr::External,
                variants: [
                    ("List".to_owned(), variant(VariantShape::Newtype(list), "The layers, listed.")),
                    (
                        "Command".to_owned(),
                        variant(
                            VariantShape::Newtype(TypeExpr::List(Box::new(TypeExpr::String))),
                            "A command that prints the layer list as a CROL value, run in the project's directory.",
                        ),
                    ),
                ]
                .into_iter()
                .collect(),
            }
        })
    }
}

impl Schema for ProjectLayer {
    fn describe(registry: &mut Registry) -> TypeRef {
        registry.add("crol::project::ProjectLayer", |_| {
            struct_def(
                "ProjectLayer",
                "A layer of a project.",
                vec![
                    ("name", field(TypeExpr::String, "The layer's name.", FieldDefault::Required)),
                    ("dir", field(TypeExpr::String, "Its module path (`SPEC.md` 4.2), from the project's directory.", FieldDefault::Required)),
                    ("read_only", field(TypeExpr::Bool, "Generated or vendored data: loaded, but without diagnostics or code actions in the editor.", recorded_default(&false))),
                ],
                None,
            )
        })
    }
}

impl Schema for ProjectOptions {
    fn describe(registry: &mut Registry) -> TypeRef {
        registry.add("crol::project::ProjectOptions", |registry| {
            let contexts = registry.describe::<Option<Vec<Vec<String>>>>();
            let d = ProjectOptions::default();
            struct_def(
                "ProjectOptions",
                "Options of a project, as `CrolConfig` has them.",
                vec![
                    (
                        "implicit_some",
                        field(
                            TypeExpr::Bool,
                            "Any value is accepted where an `Option` is expected, as `Some`.",
                            recorded_default(&d.implicit_some),
                        ),
                    ),
                    (
                        "allow_unknown_fields",
                        field(
                            TypeExpr::Bool,
                            "Keys a struct does not have are ignored.",
                            recorded_default(&d.allow_unknown_fields),
                        ),
                    ),
                    (
                        "require_complete",
                        field(
                            TypeExpr::Bool,
                            "Every field must be written unless the value has `..`.",
                            recorded_default(&d.require_complete),
                        ),
                    ),
                    (
                        "init_defaults",
                        field(
                            TypeExpr::Bool,
                            "Let a path go through a field left out, from its default.",
                            recorded_default(&d.init_defaults),
                        ),
                    ),
                    (
                        "each_variant",
                        field(
                            TypeExpr::Bool,
                            "Evaluate the base and one context for each variant, not every combination.",
                            recorded_default(&d.each_variant),
                        ),
                    ),
                    (
                        "max_contexts",
                        field(
                            TypeExpr::U64,
                            "How many contexts may be evaluated.",
                            recorded_default(&d.max_contexts),
                        ),
                    ),
                    (
                        "contexts",
                        field(
                            contexts,
                            "The contexts to evaluate; default: every one.",
                            recorded_default(&d.contexts),
                        ),
                    ),
                    (
                        "variant_keys",
                        field(
                            TypeExpr::String,
                            "The variant keys rule: `Any`, `NoNewKeys` or `SameKeys`.",
                            recorded_default(&d.variant_keys),
                        ),
                    ),
                ],
                crate::schema::default_value(&d),
            )
        })
    }
}

impl Schema for FilePattern {
    fn describe(registry: &mut Registry) -> TypeRef {
        registry.add("crol::project::FilePattern", |_| {
            struct_def(
                "FilePattern",
                "Value-mode documents of a type.",
                vec![
                    (
                        "glob",
                        field(
                            TypeExpr::String,
                            "A module path whose segments may hold `*`, or be `**`.",
                            FieldDefault::Required,
                        ),
                    ),
                    (
                        "type",
                        field(
                            TypeExpr::String,
                            "The schema path of their type.",
                            FieldDefault::Required,
                        ),
                    ),
                ],
                None,
            )
        })
    }
}

impl<T: Schema> Schema for Variant<T> {
    fn describe(registry: &mut Registry) -> TypeRef {
        let arg = registry.describe::<T>();
        let path = format!("crol::compose::Variant<{}>", crate::schema::type_name(&arg));
        registry.add(&path, |_| {
            let variant = |shape, doc: &str| VariantDef {
                shape,
                doc: doc.to_owned(),
                aliases: Vec::new(),
                explicit_defaults: None,
                scope: false,
            };
            TypeDef::Enum {
                name: "Variant".to_owned(),
                doc: "A variant declaration (`SPEC.md` 8.1).".to_owned(),
                source: (file!().replace('\\', "/"), line!()),
                repr: Repr::External,
                variants: [
                    (
                        "Discrete".to_owned(),
                        variant(
                            VariantShape::Newtype(arg.clone()),
                            "A flag: on or off, independent of the rest.",
                        ),
                    ),
                    (
                        "Grouped".to_owned(),
                        variant(
                            VariantShape::Newtype(TypeExpr::List(Box::new(arg.clone()))),
                            "Alternatives: at most one of them is active.",
                        ),
                    ),
                    (
                        "Open".to_owned(),
                        variant(
                            VariantShape::Newtype(TypeExpr::String),
                            "A group of alternatives that data declares, with `#![variant(group: name)]`.",
                        ),
                    ),
                ]
                .into_iter()
                .collect(),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_example_of_the_specification() {
        let text = r#"
            Project {
                layers: List([
                    { name: "base", dir: "data::base", read_only: true },
                    { name: "extension", dir: "data::extension", .. },
                    { name: "local", dir: "data::local", .. },
                ]),
                variants: [Grouped(["dev", "prod", "prod_eu"]), Discrete("cache")],
                parents: { prod_eu: "prod" },
                options: { implicit_some: true, allow_unknown_fields: false, .. },
                schema: Some("crol::schema"),
                root: Some("app::Definition"),
                files: [
                    { glob: "config::*", type: "app::Config" },
                ],
                ..
            }
        "#;
        let project = Project::from_text("project.crol", text).unwrap();
        assert_eq!(project.stack_names(), [DEFAULT_STACK]);
        let stack = &project.stacks[DEFAULT_STACK];
        let ProjectLayers::List(layers) = &stack.layers else {
            panic!()
        };
        assert_eq!(layers.len(), 3);
        assert!(layers[0].read_only && !layers[1].read_only);
        assert_eq!(project.variants().unwrap().context_count(), 8);
        assert_eq!(stack.files[0].type_path, "app::Config");
        assert_eq!(stack.root.as_deref(), Some("app::Definition"));
        assert_eq!(
            stack.options.variant_keys().unwrap(),
            crol_eval::VariantKeys::Any
        );
        assert!(project.layers.is_none() && project.files.is_empty());

        // The schema `crol` provides accepts it.
        let schema = crate::schema::build::<Project>();
        let value: crate::RawValue = crate::from_str(text).unwrap();
        let errors = crate::check_value(&value.0, &schema, None, &Default::default());
        assert!(errors.is_empty(), "{errors:?}");
    }

    const STACKS: &str = r#"
        Project {
            stacks: {
                theme: {
                    layers: List([
                        { name: "base", dir: "themes::base", read_only: true },
                        { name: "user", dir: "themes::user", .. },
                    ]),
                    schema: Some("crol::theme_schema"),
                    files: [
                        { glob: "config::theme_*", type: "Theme" },
                    ],
                    ..
                },
                ui: {
                    layers: List([{ name: "base", dir: "ui", .. }]),
                    schema: Some("crate:fleet-ui@2#ui"),
                    files: [
                        { glob: "config::screens::**::*", type: "Screen" },
                    ],
                    ..
                },
            },
            ..
        }
    "#;

    #[test]
    fn reads_stacks() {
        let project = Project::from_text("project.crol", STACKS).unwrap();
        assert_eq!(project.stack_names(), ["theme", "ui"]);
        let dir = Path::new("/p");
        let (name, _, type_path) = project
            .file_stack(dir, &dir.join("config/screens/main/start.crol"))
            .unwrap();
        assert_eq!((name, type_path), ("ui", "Screen"));
        let (name, _, _) = project
            .file_stack(dir, &dir.join("config/theme_dark.crol"))
            .unwrap();
        assert_eq!(name, "theme");
        assert!(
            project
                .file_stack(dir, &dir.join("config/app.crol"))
                .is_none()
        );
        assert_eq!(
            project.stack("menu").unwrap_err().message(),
            "project: there is no stack `menu`; the stacks are `theme`, `ui`"
        );

        let schema = crate::schema::build::<Project>();
        let value: crate::RawValue = crate::from_str(STACKS).unwrap();
        let errors = crate::check_value(&value.0, &schema, None, &Default::default());
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn layer_directories_say_which_stack_a_file_is_in() {
        let project = Project::from_text("project.crol", STACKS).unwrap();
        let dir = Path::new("/p");
        let stack = |path: &str| project.layer_stack(dir, &dir.join(path)).unwrap();
        assert_eq!(stack("themes/user/dark.crod"), Some("theme"));
        assert_eq!(stack("themes/base"), Some("theme"));
        assert_eq!(stack("ui.crod"), Some("ui"));
        assert_eq!(stack("ui/start.crod"), Some("ui"));
        assert_eq!(stack("other/start.crod"), None);
        assert_eq!(stack("themes/other.crod"), None);
    }

    #[test]
    fn a_relative_project_directory_and_a_module_path_find_the_stack() {
        let project = Project::from_text("project.crol", STACKS).unwrap();
        for dir in [Path::new(""), Path::new(".")] {
            let stack = |path: &str| project.layer_stack(dir, Path::new(path)).unwrap();
            // `ui` names the layer file `ui.crod`, as a module path does.
            assert_eq!(stack("ui"), Some("ui"), "{dir:?}");
            assert_eq!(stack("./ui"), Some("ui"), "{dir:?}");
            assert_eq!(stack("ui.crod"), Some("ui"), "{dir:?}");
            assert_eq!(stack("themes/user"), Some("theme"), "{dir:?}");
            assert_eq!(stack("other"), None, "{dir:?}");
        }
    }

    #[test]
    fn a_stack_extends_only_its_own_files() {
        let project = Project::from_text("project.crol", STACKS).unwrap();
        let dir = Path::new("/p");
        let dark = dir.join("themes/user/dark.crod");
        let found = |text: &str| -> Vec<String> {
            (project
                .cross_stack_extends(dir, "theme", &dark, text)
                .into_iter())
            .map(|(_, message)| message)
            .collect()
        };
        let wanted = |written: &str| {
            format!(
                "extends: `{written}` is in the layers of the stack `ui`; the layers of `theme` extend only files of their own stack"
            )
        };
        for written in ["::ui", "::ui::screens::start", "super::super::ui"] {
            let text = format!("#![extends({written})]\n");
            assert_eq!(found(&text), [wanted(written)], "{written}");
        }
        // Its own stack's files, and those of no stack.
        for written in ["light", "super::base::colors", "::shared::colors"] {
            let text = format!("#![extends({written})]\n");
            assert_eq!(found(&text), Vec::<String>::new(), "{written}");
        }
        // Only a document of that stack is asked about.
        let text = "#![extends(::ui)]\n";
        let other = dir.join("shared/a.crod");
        assert!(
            project
                .cross_stack_extends(dir, "theme", &other, text)
                .is_empty()
        );
        // The range is that of the module path.
        let (range, _) = project.cross_stack_extends(dir, "theme", &dark, text)[0].clone();
        assert_eq!(&text[range], "::ui");
    }

    /// As on macOS, where the temporary directory is below the link `/var`:
    /// the project's directory as written goes through a link, the
    /// document's path is canonical, and what an `extends` names may not
    /// exist as written.
    #[cfg(unix)]
    #[test]
    fn stacks_are_found_through_a_link_above_the_project() {
        let root = std::env::temp_dir().join(format!("crol-stack-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["real/themes/user", "real/ui"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("real/themes/user/dark.crod"), "").unwrap();
        std::fs::write(root.join("real/ui/start.crod"), "").unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        let project = Project::from_text("project.crol", STACKS).unwrap();
        let dir = root.join("link");
        let dark = root
            .join("real/themes/user/dark.crod")
            .canonicalize()
            .unwrap();
        assert_eq!(project.layer_stack(&dir, &dark).unwrap(), Some("theme"));
        let found = project.cross_stack_extends(&dir, "theme", &dark, "#![extends(::ui::start)]\n");
        assert_eq!(found.len(), 1, "{found:?}");
        let found =
            project.cross_stack_extends(&dir, "theme", &dark, "#![extends(super::super::ui)]\n");
        assert_eq!(found.len(), 1, "{found:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_failing_layer_command_matters_only_if_nothing_else_holds_the_path() {
        let text = STACKS.replace(
            r#"List([{ name: "base", dir: "ui", .. }])"#,
            r#"Command(["crol-test-no-such-program"])"#,
        );
        let project = Project::from_text("project.crol", &text).unwrap();
        let dir = Path::new("/p");
        assert_eq!(
            project
                .layer_stack(dir, &dir.join("themes/user/a.crod"))
                .unwrap(),
            Some("theme")
        );
        let error = project
            .layer_stack(dir, &dir.join("ui/a.crod"))
            .unwrap_err();
        assert!(error.message().contains("cannot run"), "{error}");
    }

    #[test]
    fn a_file_belongs_to_one_stack() {
        let text = STACKS.replace("config::screens::**::*", "config::**::*");
        let error = Project::from_text("project.crol", &text).unwrap_err();
        assert_eq!(
            error.diagnostics(),
            [
                "project.crol:11:33: project: `config::theme_*` of stack `theme` matches files that `config::**::*` of stack `ui` claims too; a file belongs to one stack",
                "project.crol:19:33: project: `config::**::*` of stack `ui` matches files that `config::theme_*` of stack `theme` claims too; a file belongs to one stack",
            ]
        );
        let text = STACKS.replace(r#"dir: "ui""#, r#"dir: "themes""#);
        let error = Project::from_text("project.crol", &text).unwrap_err();
        assert_eq!(error.diagnostics().len(), 4, "{error}");
        assert!(
            error.diagnostics()[0].contains(
                "`themes::base` of stack `theme` holds files that `themes` of stack `ui` claims too"
            ),
            "{error}"
        );
    }

    #[test]
    fn globs_overlap_by_form() {
        let overlap = |a: &str, b: &str| {
            let [a, b] = [a, b].map(|glob| FilePattern {
                glob: glob.to_owned(),
                type_path: String::new(),
            });
            a.overlaps(&b)
        };
        for (a, b) in [
            ("config::*", "config::app"),
            ("config::app_*", "config::*_dark"),
            ("config::**::*", "config::a::b::c"),
            ("**::*", "x"),
            ("a::**::*", "**::c::*"),
            ("a::**::b::*", "**::b::*"),
            ("a*::x", "*b::x"),
            ("config::*", "::config::*"),
        ] {
            assert!(overlap(a, b), "{a} {b}");
            assert!(overlap(b, a), "{b} {a}");
        }
        for (a, b) in [
            ("config::*", "other::*"),
            ("config::app_*", "config::theme_*"),
            ("config::*", "config::a::*"),
            ("a::**::*", "b::**::*"),
            ("config::a*", "config::b*"),
            ("x", "y"),
        ] {
            assert!(!overlap(a, b), "{a} {b}");
            assert!(!overlap(b, a), "{b} {a}");
        }
    }

    #[test]
    fn stacks_are_written_one_way() {
        for (text, message) in [
            (
                r#"Project { stacks: { a: { layers: List([]), .. } }, schema: Some("x"), .. }"#,
                "with `stacks`, `layers`, `options`, `schema`, `root`, `files` and `constants` belong in each stack",
            ),
            (
                r#"Project { stacks: { a: { layers: List([]), .. } }, constants: { n: 1 }, .. }"#,
                "`files` and `constants` belong in each stack",
            ),
            (
                r#"Project { stacks: { a: { layers: List([]), constants: { "a b": 1 }, .. } }, .. }"#,
                "the constant name `a b` is not an identifier",
            ),
            (
                r#"Project { stacks: { "a b": { layers: List([]), .. } }, .. }"#,
                "the stack name `a b` is not an identifier",
            ),
            (r#"Project { .. }"#, "it has neither `layers` nor `stacks`"),
        ] {
            let error = Project::from_text("p.crol", text).unwrap_err();
            assert!(error.message().contains(message), "{text}: {error}");
        }
    }

    #[test]
    fn each_stack_has_its_own_constants() {
        let text = r#"Project {
            stacks: {
                a: { layers: List([]), constants: { n: 1 }, .. },
                b: { layers: List([]), .. },
            },
            ..
        }"#;
        let project = Project::from_text("p.crol", text).unwrap();
        let n = |stack: &str| {
            project.stacks[stack]
                .constants()
                .get("n")
                .map(|v| v.kind_name())
        };
        assert_eq!(n("a"), Some("an integer"));
        assert_eq!(n("b"), None);
        // Written at the top, they are the one stack's.
        let text = r#"Project { layers: List([]), constants: { n: 1 }, .. }"#;
        let project = Project::from_text("p.crol", text).unwrap();
        assert!(project.stacks[DEFAULT_STACK].constants().get("n").is_some());
        assert!(project.constants.is_empty());
    }

    fn project_with(globs: &[&str]) -> ProjectStack {
        ProjectStack {
            layers: ProjectLayers::List(Vec::new()),
            options: ProjectOptions::default(),
            schema: None,
            root: None,
            constants: IndexMap::new(),
            files: globs
                .iter()
                .map(|glob| FilePattern {
                    glob: (*glob).to_owned(),
                    type_path: (*glob).to_owned(),
                })
                .collect(),
        }
    }

    #[test]
    fn globs_match_by_segments() {
        let dir = Path::new("/p");
        let matched = |glob: &str, file: &str| {
            project_with(&[glob])
                .file_type(dir, &dir.join(file))
                .is_some()
        };
        for (glob, file) in [
            ("config::*", "config/app.crol"),
            ("::config::*", "config/app.crol"),
            ("config::app", "config/app.crol"),
            ("config::a*p", "config/app.crol"),
            ("config::*p*", "config/app.crol"),
            ("*", "app.crol"),
            ("**::*", "app.crol"),
            ("**::*", "a/b/c/app.crol"),
            ("a::**::*", "a/app.crol"),
            ("a::**::c::*", "a/b/b/c/app.crol"),
            ("**::b::**::*", "a/b/c/app.crol"),
            ("c*::*", "config/app.crol"),
            // Matched by name, not resolved: this cannot be a module path.
            ("config::*", "config/my-app.crol"),
        ] {
            assert!(matched(glob, file), "{glob} {file}");
        }
        for (glob, file) in [
            ("config::*", "app.crol"),
            ("config::*", "config/deep/app.crol"),
            ("config::*", "other/app.crol"),
            ("config::*", "config/app.crod"),
            ("config::*", "config/app.txt"),
            ("config::app", "config/app2.crol"),
            ("config::App", "config/app.crol"),
            ("*", "../app.crol"),
            ("**::*", "/elsewhere/app.crol"),
            ("*", ".hidden.crol"),
            ("**::*", ".git/app.crol"),
            ("a::**::c::*", "a/app.crol"),
        ] {
            assert!(!matched(glob, file), "{glob} {file}");
        }
    }

    #[test]
    fn the_first_glob_that_matches_decides() {
        let project = project_with(&["config::**::*", "config::app"]);
        let dir = Path::new("/p");
        assert_eq!(
            project.file_type(dir, &dir.join("config/app.crol")),
            Some("config::**::*")
        );
    }

    #[test]
    fn a_glob_has_a_form() {
        for glob in [
            "a::**",
            "**",
            "a::",
            "::",
            "",
            "a::::b",
            "super::a",
            "a::super::b",
            "a**::b",
            "a::***",
            "a/b",
            "a.b",
            "a::b.crol",
        ] {
            let error = project_with(&[glob]).files[0].check().unwrap_err();
            assert!(error.to_string().contains("invalid glob"), "{glob}");
        }
        for glob in ["a", "::a", "a::*", "a::b*c*", "**::*", "_a1::**::*"] {
            project_with(&[glob]).files[0].check().unwrap();
        }
    }

    #[test]
    fn a_glob_with_many_stars_stays_quick() {
        let glob = format!("{}x", "*a".repeat(40));
        let file = format!("{}.crol", "a".repeat(4000));
        let dir = Path::new("/p");
        let project = project_with(&[&glob]);
        assert_eq!(project.file_type(dir, &dir.join(file)), None);
    }

    #[test]
    fn the_schema_and_the_deserializer_agree_on_mistakes() {
        let schema = crate::schema::build::<Project>();
        for text in [
            r#"Project { layers: Lists([]), .. }"#,
            r#"Project { layers: List([{ name: "a", dir: 1, .. }]), .. }"#,
            r#"Project { layers: List([]), variants: [Flag("x")], .. }"#,
            r#"Project { layers: List([]), options: { max_contexts: -1, .. }, .. }"#,
            r#"Project { layers: List([]), schema: 3, .. }"#,
        ] {
            let value: crate::RawValue = crate::from_str(text).unwrap();
            let checked = crate::check_value(&value.0, &schema, None, &Default::default());
            let error = crate::from_str::<Project>(text).unwrap_err();
            assert_eq!(checked.len(), 1, "{text}: {checked:?}");
            assert_eq!(checked[0].message, error.message(), "{text}");
        }
    }
}
