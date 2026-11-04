//! Module paths (`SPEC.md` 4.2): documents and code name files as Rust names
//! modules, so that paths read the same on every platform.

use std::path::{Path, PathBuf};

use crate::error::Error;

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

/// Resolves a module path to the file (`name.crol`) or directory (`name/`)
/// it names. `here` is the directory of the document the path is written
/// in; code passes `None`, and relative paths then start from `root`.
pub(crate) fn resolve(
    module_path: &str,
    root: &Path,
    here: Option<&Path>,
) -> Result<PathBuf, Error> {
    let parsed = parse(module_path)?;
    let mut base = match (parsed.rooted, here) {
        (false, Some(here)) => here.to_path_buf(),
        _ => root.to_path_buf(),
    };
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
    named(module_path, &base, last)
}

/// The file or directory `last` names in the directory `base`.
pub(crate) fn named(module_path: &str, base: &Path, last: &str) -> Result<PathBuf, Error> {
    let file = base.join(format!("{last}.crol"));
    let directory = base.join(last);
    match (file.is_file(), directory.is_dir()) {
        (true, true) => Err(Error::new(format!(
            "the module path `{module_path}` names both {} and {}",
            file.display(),
            directory.display()
        ))),
        (true, false) => Ok(file),
        (false, true) => Ok(directory),
        (false, false) => Err(Error::new(format!(
            "the module path `{module_path}` names nothing: there is no {} or {}",
            file.display(),
            directory.display()
        ))),
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
        std::fs::write(root.join("data/local.crol"), "").unwrap();
        std::fs::create_dir_all(root.join("both")).unwrap();
        std::fs::write(root.join("both.crol"), "").unwrap();

        assert_eq!(
            resolve("::data::base", &root, None).unwrap(),
            root.join("data/base")
        );
        assert_eq!(
            resolve("data::local", &root, None).unwrap(),
            root.join("data/local.crol")
        );
        let here = root.join("data/base");
        assert_eq!(
            resolve("super::local", &root, Some(&here)).unwrap(),
            root.join("data/local.crol")
        );
        assert!(
            resolve("super::super::super::x", &root, Some(&here))
                .unwrap_err()
                .to_string()
                .contains("above the root")
        );
        assert!(
            resolve("::both", &root, None)
                .unwrap_err()
                .to_string()
                .contains("names both")
        );
        assert!(
            resolve("::nothing", &root, None)
                .unwrap_err()
                .to_string()
                .contains("names nothing")
        );
        assert!(
            resolve("::my-file", &root, None)
                .unwrap_err()
                .to_string()
                .contains("not an identifier")
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
