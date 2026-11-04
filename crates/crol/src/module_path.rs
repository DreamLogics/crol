//! Module paths (`SPEC.md` 4.2): documents and code name files as Rust names
//! modules, so that paths read the same on every platform.

use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::loader::SourceId;
use crate::mode::Mode;

/// A module path taken apart: its form, checked, not yet applied to anything.
pub(crate) struct Parsed<'a> {
    /// Written with `::`: it starts from the root.
    pub(crate) rooted: bool,
    /// How many `super`s it starts with.
    pub(crate) supers: usize,
    /// The names after them; at least one.
    pub(crate) segments: Vec<&'a str>,
}

/// Checks the form of a module path. Every segment that is kept is an
/// identifier, so no path can hold a separator, `.` or `..`.
pub(crate) fn parse(module_path: &str) -> Result<Parsed<'_>, Error> {
    let invalid = |why: &str| Error::new(format!("invalid module path `{module_path}`: {why}"));
    let (rooted, rest) = match module_path.strip_prefix("::") {
        Some(rest) => (true, rest),
        None => (false, module_path),
    };
    let mut segments: Vec<&str> = rest.split("::").collect();
    let supers = segments.iter().take_while(|s| **s == "super").count();
    if rooted && supers > 0 {
        return Err(invalid("`super` cannot follow `::`"));
    }
    segments.drain(..supers);
    if segments.is_empty() {
        return Err(invalid("it names no file"));
    }
    for segment in &segments {
        if *segment == "super" {
            return Err(invalid("`super` is only allowed at the start"));
        }
        if !crol_eval::is_identifier(segment) {
            return Err(invalid(&format!("`{segment}` is not an identifier")));
        }
    }
    Ok(Parsed {
        rooted,
        supers,
        segments,
    })
}

/// The error for a module path with more `super`s than there are
/// directories below the root.
pub(crate) fn above_root(module_path: &str, root: &Path) -> Error {
    Error::new(format!(
        "the module path `{module_path}` goes above the root directory {}",
        root.display()
    ))
}

/// A directory as something to run or look in: the empty path, which is what
/// [`Path::parent`] gives for a bare file name, is the working directory.
pub(crate) fn current_if_empty(dir: &Path) -> &Path {
    if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    }
}

/// The directory `path` is in, also for a bare file name, whose parent is the
/// empty path, which [`std::process::Command::current_dir`] and
/// [`Path::ancestors`] do not take for the working directory.
pub fn directory_of(path: &Path) -> &Path {
    current_if_empty(path.parent().unwrap_or(Path::new("")))
}

/// Resolves a module path to the file (`name.crod`, or `name.crol` for a
/// value document) or directory (`name/`) it names. `here` is the directory
/// of the document the path is written in; code passes `None`, and relative
/// paths then start from `root`.
pub(crate) fn resolve(
    module_path: &str,
    root: &Path,
    here: Option<&Path>,
    mode: Mode,
) -> Result<PathBuf, Error> {
    let parsed = parse(module_path)?;
    let root = current_if_empty(root);
    let mut base = match (parsed.rooted, here) {
        (false, Some(here)) => here.to_path_buf(),
        _ => root.to_path_buf(),
    };
    // A relative path does not spell out the directories `super` goes up to.
    if parsed.supers > 0 && base.is_relative() {
        let dir = current_if_empty(&base);
        base = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    }
    // The same directory may be written two ways, as `\\?\C:\x` and `C:/x`.
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let within =
        |base: &Path| base.starts_with(root) || canonical(base).starts_with(canonical(root));
    for _ in 0..parsed.supers {
        if !base.pop() || !within(&base) {
            return Err(above_root(module_path, root));
        }
    }
    let (last, directories) = parsed.segments.split_last().expect("not empty");
    for directory in directories {
        base.push(directory);
    }
    named(module_path, &base, last, mode)
}

/// Whether `last` could name anything in the directory `base`, of either
/// mode: what [`named`] then accepts or refuses, rather than says is not
/// there.
pub(crate) fn exists(base: &Path, last: &str) -> bool {
    [Mode::Data.extension(), Mode::Value.extension(), "crob"]
        .iter()
        .any(|extension| base.join(format!("{last}.{extension}")).is_file())
        || base.join(last).is_dir()
}

/// The file or directory `last` names in the directory `base`. A file of the
/// other mode is not what the path names: an error that says so, rather
/// than a path that names nothing.
pub(crate) fn named(
    module_path: &str,
    base: &Path,
    last: &str,
    mode: Mode,
) -> Result<PathBuf, Error> {
    let refused = |path: &Path| {
        mode.check_path(path).map_err(|error| {
            let id = SourceId::from_path(path);
            Error::new(format!("the module path `{module_path}`: {error}")).with_source_id(id)
        })
    };
    let file = base.join(format!("{last}.{}", mode.extension()));
    let directory = base.join(last);
    match (file.is_file(), directory.is_dir()) {
        (true, true) => Err(Error::new(format!(
            "the module path `{module_path}` names both {} and {}",
            file.display(),
            directory.display()
        ))),
        (true, false) => Ok(file),
        (false, true) => Ok(directory),
        // Without a text form, a compiled layer (`SPEC.md` 7.8).
        (false, false) if mode == Mode::Data && base.join(format!("{last}.crob")).is_file() => {
            Ok(base.join(format!("{last}.crob")))
        }
        (false, false) => {
            let other = base.join(format!("{last}.{}", mode.other().extension()));
            if other.is_file() {
                refused(&other)?;
            }
            Err(Error::new(format!(
                "the module path `{module_path}` names nothing: there is no {} or {}",
                file.display(),
                directory.display()
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_files_directories_and_super() {
        let root = std::env::temp_dir().join(format!("crol-module-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("data/base")).unwrap();
        std::fs::write(root.join("data/local.crod"), "").unwrap();
        std::fs::create_dir_all(root.join("both")).unwrap();
        std::fs::write(root.join("both.crod"), "").unwrap();

        assert_eq!(
            resolve("::data::base", &root, None, Mode::Data).unwrap(),
            root.join("data/base")
        );
        assert_eq!(
            resolve("data::local", &root, None, Mode::Data).unwrap(),
            root.join("data/local.crod")
        );
        let here = root.join("data/base");
        assert_eq!(
            resolve("super::local", &root, Some(&here), Mode::Data).unwrap(),
            root.join("data/local.crod")
        );
        assert!(
            resolve("super::super::super::x", &root, Some(&here), Mode::Data)
                .unwrap_err()
                .to_string()
                .contains("above the root")
        );
        assert!(
            resolve("::both", &root, None, Mode::Data)
                .unwrap_err()
                .to_string()
                .contains("names both")
        );
        assert!(
            resolve("::nothing", &root, None, Mode::Data)
                .unwrap_err()
                .to_string()
                .contains("names nothing")
        );
        assert!(
            resolve("::my-file", &root, None, Mode::Data)
                .unwrap_err()
                .to_string()
                .contains("not an identifier")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_bare_file_name_is_in_the_working_directory() {
        assert_eq!(directory_of(Path::new("x.crod")), Path::new("."));
        assert_eq!(directory_of(Path::new("")), Path::new("."));
        assert_eq!(directory_of(Path::new("./x.crod")), Path::new("."));
        assert_eq!(directory_of(Path::new("a/x.crod")), Path::new("a"));
        assert_eq!(directory_of(Path::new("/x.crod")), Path::new("/"));
    }

    #[test]
    fn a_path_finds_the_extension_of_what_it_is_looked_for_as() {
        let root = std::env::temp_dir().join(format!("crol-module-mode-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("dir")).unwrap();
        for name in ["both.crod", "both.crol", "data.crod", "value.crol"] {
            std::fs::write(root.join(name), "").unwrap();
        }
        let error = |path, mode| resolve(path, &root, None, mode).unwrap_err().to_string();

        // One name may have a file of each mode.
        let found = resolve("::both", &root, None, Mode::Value).unwrap();
        assert_eq!(found, root.join("both.crol"));
        let found = resolve("::both", &root, None, Mode::Data).unwrap();
        assert_eq!(found, root.join("both.crod"));

        assert!(error("::value", Mode::Data).contains("cannot be a layer"));
        assert!(error("::data", Mode::Value).contains("cannot be read as a value document"));
        assert!(error("::value", Mode::Data).contains("rename it to `value.crod`"));
        assert!(error("::data", Mode::Value).contains("rename it to `data.crol`"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
