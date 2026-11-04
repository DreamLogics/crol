//! The project file, `project.crov` (`SPEC.md` 12.4): what tools that are
//! not the application (the CLI, the language server) need to know to
//! evaluate its data.

use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::compose::Variant;
use crate::error::Error;
use crate::loader::SourceId;
use crate::mode::{Mode, RETIRED};
use crate::schema::{
    FieldDefault, Registry, Repr, Schema, TypeDef, TypeExpr, TypeRef, VariantDef, VariantShape,
    field, recorded_default, struct_def,
};

/// The file name tools look for.
pub const FILE_NAME: &str = "project.crov";

/// A project file: what tools that are not the application need to know to
/// evaluate its data (`SPEC.md` 12.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    /// The layers, lowest first.
    pub layers: ProjectLayers,
    /// The variants (`SPEC.md` 8.1).
    #[serde(default)]
    pub variants: Vec<Variant>,
    /// Variant parents: child to parent.
    #[serde(default)]
    pub parents: IndexMap<String, String>,
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
    /// Warn about patches to a key from several documents of one layer.
    pub warn_cross_file_patches: bool,
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
            warn_cross_file_patches: true,
            max_contexts: 4096,
            contexts: None,
            variant_keys: "Any".to_owned(),
        }
    }
}

impl ProjectOptions {
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

    /// Whether the glob matches the `.crov` file `stem` in `directories`
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
    /// The project file in `start` or the nearest directory above it. A
    /// `project.crol` where it is sought is an error that says to rename it,
    /// also next to a `project.crov` (`SPEC.md` 12.4).
    pub fn find(start: &Path) -> Result<Option<PathBuf>, Error> {
        for dir in start.ancestors() {
            let retired = dir.join(format!("project.{RETIRED}"));
            if retired.is_file() {
                Mode::Value
                    .check_path(&retired)
                    .map_err(|error| error.with_source_id(SourceId::from_path(&retired)))?;
            }
            let candidate = dir.join(FILE_NAME);
            if candidate.is_file() {
                return Ok(Some(candidate));
            }
        }
        Ok(None)
    }

    /// Reads a project file. It is a value document, so a `.crod` or `.crol`
    /// file is an error (`SPEC.md` 4).
    pub fn read(path: &Path) -> Result<Self, Error> {
        Mode::Value
            .check_path(path)
            .map_err(|error| error.with_source_id(SourceId::from_path(path)))?;
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::new(format!("cannot read {}: {e}", path.display())))?;
        let name = path.display().to_string();
        let in_file = |e: Error| e.in_file(&name);
        let project: Self = crate::from_named_str(&name, &text).map_err(in_file)?;
        for pattern in &project.files {
            pattern.check().map_err(in_file)?;
        }
        Ok(project)
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
        let stem = file.strip_suffix(".crov")?;
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

    /// The declared variants.
    pub fn variants(&self) -> Result<crol_eval::Variants, Error> {
        let mut variants = crol_eval::Variants::new();
        let project = |message: String| Error::new(format!("project: {message}"));
        for variant in &self.variants {
            match variant {
                Variant::Discrete(name) => variants.add_discrete(name).map_err(project)?,
                Variant::Grouped(names) => variants.add_grouped(names).map_err(project)?,
            }
        }
        for (child, parent) in &self.parents {
            variants.add_parent(child, parent).map_err(project)?;
        }
        Ok(variants)
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

// ---- The project file's schema (`SPEC.md` 12.4) ----

impl Schema for Project {
    fn describe(registry: &mut Registry) -> TypeRef {
        registry.add("crol::project::Project", |registry| {
            let layers = registry.describe::<ProjectLayers>();
            let variants = registry.describe::<Vec<Variant>>();
            let parents = registry.describe::<IndexMap<String, String>>();
            let options = registry.describe::<ProjectOptions>();
            let files = registry.describe::<Vec<FilePattern>>();
            let none = || crate::schema::none_value();
            struct_def(
                "Project",
                "A project file: what tools that are not the application need to know to evaluate its data (`SPEC.md` 12.4).",
                vec![
                    ("layers", field(layers, "The layers, lowest first.", FieldDefault::Required)),
                    ("variants", field(variants, "The variants (`SPEC.md` 8.1).", recorded_default(&Vec::<Variant>::new()))),
                    ("parents", field(parents, "Variant parents: child to parent.", recorded_default(&IndexMap::<String, String>::new()))),
                    ("options", field(options, "How the data is evaluated and checked.", recorded_default(&ProjectOptions::default()))),
                    ("schema", field(TypeExpr::Option(Box::new(TypeExpr::String)), "The schema: a module path written as a string, a URL, or a crate.", none())),
                    ("root", field(TypeExpr::Option(Box::new(TypeExpr::String)), "The path of the type of root values; default: the schema's root.", none())),
                    ("files", field(files, "Value-mode documents and their types.", recorded_default(&Vec::<FilePattern>::new()))),
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
                        "warn_cross_file_patches",
                        field(
                            TypeExpr::Bool,
                            "Warn about patches to a key from several documents of one layer.",
                            recorded_default(&d.warn_cross_file_patches),
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
            }
        "#;
        let project: Project = crate::from_str(text).unwrap();
        let ProjectLayers::List(layers) = &project.layers else {
            panic!()
        };
        assert_eq!(layers.len(), 3);
        assert!(layers[0].read_only && !layers[1].read_only);
        assert_eq!(project.variants().unwrap().context_count(), 8);
        assert_eq!(project.files[0].type_path, "app::Config");
        assert_eq!(
            project.options.variant_keys().unwrap(),
            crol_eval::VariantKeys::Any
        );

        // The schema `crol` provides accepts it.
        let schema = crate::schema::build::<Project>();
        let value: crate::RawValue = crate::from_str(text).unwrap();
        let errors = crate::check_value(&value.0, &schema, None, &Default::default());
        assert!(errors.is_empty(), "{errors:?}");
    }

    fn project_with(globs: &[&str]) -> Project {
        Project {
            layers: ProjectLayers::List(Vec::new()),
            variants: Vec::new(),
            parents: IndexMap::new(),
            options: ProjectOptions::default(),
            schema: None,
            root: None,
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
            ("config::*", "config/app.crov"),
            ("::config::*", "config/app.crov"),
            ("config::app", "config/app.crov"),
            ("config::a*p", "config/app.crov"),
            ("config::*p*", "config/app.crov"),
            ("*", "app.crov"),
            ("**::*", "app.crov"),
            ("**::*", "a/b/c/app.crov"),
            ("a::**::*", "a/app.crov"),
            ("a::**::c::*", "a/b/b/c/app.crov"),
            ("**::b::**::*", "a/b/c/app.crov"),
            ("c*::*", "config/app.crov"),
            // Matched by name, not resolved: this cannot be a module path.
            ("config::*", "config/my-app.crov"),
        ] {
            assert!(matched(glob, file), "{glob} {file}");
        }
        for (glob, file) in [
            ("config::*", "app.crov"),
            ("config::*", "config/deep/app.crov"),
            ("config::*", "other/app.crov"),
            ("config::*", "config/app.crod"),
            ("config::*", "config/app.crol"),
            ("config::*", "config/app.txt"),
            ("config::app", "config/app2.crov"),
            ("config::App", "config/app.crov"),
            ("*", "../app.crov"),
            ("**::*", "/elsewhere/app.crov"),
            ("*", ".hidden.crov"),
            ("**::*", ".git/app.crov"),
            ("a::**::c::*", "a/app.crov"),
        ] {
            assert!(!matched(glob, file), "{glob} {file}");
        }
    }

    #[test]
    fn the_first_glob_that_matches_decides() {
        let project = project_with(&["config::**::*", "config::app"]);
        let dir = Path::new("/p");
        assert_eq!(
            project.file_type(dir, &dir.join("config/app.crov")),
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
            "a::b.crov",
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
        let file = format!("{}.crov", "a".repeat(4000));
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
