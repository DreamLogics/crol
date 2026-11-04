//! `crol --init` (`SPEC.md` 11): set up a Cargo project for CROL.

use std::io::{ErrorKind, Write};
use std::path::Path;

use crol::Mode;
use crol::project::{FILE_NAME, Project};
use crol_syntax::format::{FormatConfig, format};

use crate::diagnostics::Reporter;

/// The project file. The formatter lays it out, so that it stays formatted
/// when the formatter's rules change.
const PROJECT: &str = r#"
// What `crol` and the language server need to know about this project's data.
Project {
    // The data, lowest layer first: each layer patches the ones before it.
    // `dir` is a module path from this file: `data::base` is the directory `data/base`.
    layers: List([{ name: "base", dir: "data::base", .. }]),

    // Variants of the data, such as `[Grouped(["dev", "prod"]), Discrete("cache")]`.
    variants: [],

    // The schema file your application writes, to check the data's types:
    // `Some("schema")` is `schema.crov` next to this file.
    schema: None,
    ..
}
"#;

/// The directory of the layer `PROJECT` names, from the project's directory.
const LAYER_DIR: &str = "data/base";

/// The name of the first document of the layer.
const DOCUMENT_NAME: &str = "main.crod";

/// The first document of the layer. A layer's directory has to exist, and
/// an empty directory would not survive version control.
const DOCUMENT: &str = r#"
// The `base` layer: every `.crod` file below this directory belongs to it.
// Define your data here, for instance:
//
//     +server: { port: 8080 }
"#;

/// Sets up the Cargo project around the current directory. Returns whether
/// it did.
pub(crate) fn run(reporter: &mut Reporter) -> bool {
    match init(reporter) {
        Ok(()) => true,
        Err(message) => {
            reporter.error(&message);
            false
        }
    }
}

fn init(reporter: &mut Reporter) -> Result<(), String> {
    let current =
        std::env::current_dir().map_err(|e| format!("cannot read the current directory: {e}"))?;
    let root = current
        .ancestors()
        .find(|dir| dir.join("Cargo.toml").is_file())
        .ok_or_else(|| {
            format!(
                "no Cargo.toml in {} or above; run this in a Rust project",
                current.display()
            )
        })?;
    // A `project.crol` here or above has to be renamed first.
    let other_project = Project::find(&current).map_err(|e| e.to_string())?;
    let project_file = root.join(FILE_NAME);
    // `symlink_metadata` also sees a dangling link.
    if project_file.symlink_metadata().is_ok() {
        return Err(refusal(&project_file));
    }
    let layer_dir = root.join(LAYER_DIR);
    // Beside this file, the layer's module path would be ambiguous
    // (`SPEC.md` 4.2).
    let rival = layer_dir.with_extension(Mode::Data.extension());
    if rival.exists() {
        return Err(format!(
            "{} is in the way: the layer `data::base` would name both it and the directory {}",
            rival.display(),
            layer_dir.display()
        ));
    }
    let project_text = formatted(PROJECT)?;
    let document_text = formatted(DOCUMENT)?;

    // The project file comes last, so that it never names a directory that
    // is not there.
    std::fs::create_dir_all(&layer_dir)
        .map_err(|e| format!("cannot create {}: {e}", layer_dir.display()))?;
    let document = layer_dir.join(DOCUMENT_NAME);
    let created_document = match create_new(&document, &document_text) {
        Ok(()) => true,
        Err(error) if error.kind() == ErrorKind::AlreadyExists => false,
        Err(error) => return Err(format!("cannot write {}: {error}", document.display())),
    };
    create_new(&project_file, &project_text).map_err(|error| {
        if error.kind() == ErrorKind::AlreadyExists {
            refusal(&project_file)
        } else {
            format!("cannot write {}: {error}", project_file.display())
        }
    })?;

    reporter.note(&format!("Created {}", project_file.display()));
    if created_document {
        reporter.note(&format!("Created {}", document.display()));
    }
    if let Some(other) = other_project {
        reporter.note(&format!(
            "Note: {} exists too; tools use the project file nearest to them",
            other.display()
        ));
    }
    // The application's manifest and code are the user's to change.
    reporter.note(&format!(
        "\nNext:\n  \
         - add `crol = \"{}\"` to [dependencies] in Cargo.toml, if it is not there yet\n  \
         - write your data in {LAYER_DIR}/{DOCUMENT_NAME}\n  \
         - run `crol check`",
        env!("CARGO_PKG_VERSION"),
    ));
    Ok(())
}

fn refusal(project_file: &Path) -> String {
    format!(
        "{} already exists; it was left as it is",
        project_file.display()
    )
}

fn formatted(text: &str) -> Result<String, String> {
    format(text, &FormatConfig::default())
        .map_err(|_| "the generated file does not parse; this is a bug in crol".to_owned())
}

/// Writes a file that must not exist yet.
fn create_new(path: &Path, text: &str) -> std::io::Result<()> {
    std::fs::File::create_new(path)?.write_all(text.as_bytes())
}
