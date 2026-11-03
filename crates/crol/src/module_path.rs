//! Module paths (`SPEC.md` 4.2): documents and code name files as Rust names
//! modules, so that paths read the same on every platform.

use std::path::{Path, PathBuf};

use crate::error::Error;

/// Resolves a module path to the file (`name.crol`) or directory (`name/`)
/// it names. `here` is the directory of the document the path is written
/// in; code passes `None`, and relative paths then start from `root`.
pub(crate) fn resolve(
    module_path: &str,
    root: &Path,
    here: Option<&Path>,
) -> Result<PathBuf, Error> {
    let invalid = |why: &str| Error::new(format!("invalid module path `{module_path}`: {why}"));
    let (mut base, rest) = match module_path.strip_prefix("::") {
        Some(rest) => (root.to_path_buf(), rest),
        None => (here.unwrap_or(root).to_path_buf(), module_path),
    };
    let mut segments: Vec<&str> = rest.split("::").collect();
    let supers = segments.iter().take_while(|s| **s == "super").count();
    if module_path.starts_with("::") && supers > 0 {
        return Err(invalid("`super` cannot follow `::`"));
    }
    for _ in 0..supers {
        if !base.pop() || !base.starts_with(root) {
            return Err(Error::new(format!(
                "the module path `{module_path}` goes above the root directory {}",
                root.display()
            )));
        }
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
    let (last, directories) = segments.split_last().expect("not empty");
    for directory in directories {
        base.push(directory);
    }
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
