//! The project file, `project.crol` (`SPEC.md` 12.4): what tools that are
//! not the application (the CLI, the language server) need to know to
//! evaluate its data.

use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::compose::Variant;
use crate::error::Error;

/// The file name tools look for.
pub const FILE_NAME: &str = "project.crol";

/// A project file (`SPEC.md` 12.4).
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
    /// Options, as `CrolConfig` has them.
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
    /// `CrolConfig::implicit_some`.
    pub implicit_some: bool,
    /// `CrolConfig::allow_unknown_fields`.
    pub allow_unknown_fields: bool,
    /// `CrolConfig::require_complete`.
    pub require_complete: bool,
    /// `CrolConfig::warn_cross_file_patches`.
    pub warn_cross_file_patches: bool,
    /// `CrolConfig::max_contexts`.
    pub max_contexts: usize,
    /// `CrolConfig::contexts`.
    pub contexts: Option<Vec<Vec<String>>>,
    /// `CrolConfig::variant_keys`: `Any`, `NoNewKeys` or `SameKeys`.
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

impl Project {
    /// The project file in `start` or the nearest directory above it.
    pub fn find(start: &Path) -> Option<PathBuf> {
        let mut dir = Some(start);
        while let Some(current) = dir {
            let candidate = current.join(FILE_NAME);
            if candidate.is_file() {
                return Some(candidate);
            }
            dir = current.parent();
        }
        None
    }

    /// Reads a project file.
    pub fn read(path: &Path) -> Result<Self, Error> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::new(format!("cannot read {}: {e}", path.display())))?;
        crate::from_str(&text).map_err(|e| Error::new(format!("{}: {e}", path.display())))
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
                    .current_dir(dir)
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
                crate::from_str(&text)
                    .map_err(|e| Error::new(format!("project: the layer command printed {e}")))
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

    /// The schema file, for a schema given as a module path; URLs and
    /// crates are not supported yet.
    pub fn schema_path(&self, dir: &Path) -> Result<Option<PathBuf>, Error> {
        let Some(schema) = &self.schema else {
            return Ok(None);
        };
        if schema.starts_with("https://") || schema.starts_with("crate:") {
            return Err(Error::new(format!(
                "project: schemas from URLs and crates are not supported yet: `{schema}`"
            )));
        }
        let module = if schema.starts_with("::") {
            schema.clone()
        } else {
            format!("::{schema}")
        };
        crate::module_path::resolve(&module, dir, None).map(Some)
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
    }
}
