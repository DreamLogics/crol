//! Document modes, and the file extensions that state them (`SPEC.md` 4).

use std::fmt;
use std::path::Path;

use crate::error::Error;

/// The extension that was used for both modes. It says nothing about a
/// file's behaviour, so it is refused (`SPEC.md` 4).
pub(crate) const RETIRED: &str = "crol";

/// How a document is read (`SPEC.md` 4). A file states it with its
/// extension, and for text the API called states it: [`from_str`] is value
/// mode, the composer is data mode.
///
/// [`from_str`]: crate::from_str()
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// A list of entries on the root shared by a composition's layers:
    /// `.crod` files.
    Data,
    /// One value, on its own: `.crov` files.
    Value,
}

impl Mode {
    /// The extension of files of this mode, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Data => "crod",
            Self::Value => "crov",
        }
    }

    /// The other mode.
    pub(crate) fn other(self) -> Self {
        match self {
            Self::Data => Self::Value,
            Self::Value => Self::Data,
        }
    }

    /// The mode a name's extension states: the name may be a file name, a
    /// path or a [`SourceId`](crate::SourceId)'s name. `None` for any other
    /// extension, and for none.
    pub fn of(name: &str) -> Option<Self> {
        match extension(name)? {
            "crod" => Some(Self::Data),
            "crov" => Some(Self::Value),
            _ => None,
        }
    }

    /// The mode a file system path's extension states.
    pub fn of_path(path: &Path) -> Option<Self> {
        Self::of(path.to_str()?)
    }

    /// Checks that `name` can be read in this mode: its extension must not
    /// state the other one or be the retired `.crol`. A name with any other
    /// extension, or none, states nothing, and the API it was given to
    /// decides (`SPEC.md` 4).
    pub fn check(self, name: &str) -> Result<(), Error> {
        if is_retired(name) {
            return Err(Error::new(self.retired_message(name)));
        }
        if Self::of(name) == Some(self.other()) {
            return Err(Error::new(self.mismatch_message(name)));
        }
        Ok(())
    }

    /// [`check`](Self::check) for a file system path.
    pub fn check_path(self, path: &Path) -> Result<(), Error> {
        self.check(&path.to_string_lossy())
    }

    fn retired_message(self, name: &str) -> String {
        format!(
            "{name}: the `.{RETIRED}` extension is retired; {} is `.{}`, so rename it to `{}`",
            self.description(),
            self.extension(),
            renamed(name, self)
        )
    }

    fn mismatch_message(self, name: &str) -> String {
        match self {
            Self::Data => format!(
                "{name}: a `.crov` file is a value document and cannot be a layer; \
                 move it out of the layer, or rename it to `{}` if it is data",
                renamed(name, self)
            ),
            Self::Value => format!(
                "{name}: a `.crod` file is a data document and cannot be read as a value \
                 document, such as a project or schema file; rename it to `{}` if it is one",
                renamed(name, self)
            ),
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Data => "a layer's document",
            Self::Value => "a value document",
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Data => "data mode",
            Self::Value => "value mode",
        })
    }
}

/// Whether a name has the retired extension, `.crol`.
pub fn is_retired(name: &str) -> bool {
    extension(name) == Some(RETIRED)
}

/// Whether a file a directory lists is a document of some mode, or one that
/// should be: anything else in a layer's directory is not CROL's business
/// (`SPEC.md` 7.1).
pub(crate) fn is_document(name: &str) -> bool {
    is_retired(name) || Mode::of(name).is_some()
}

/// The last component of `name`, whichever separator it uses.
fn file_name(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

/// What follows the last `.` of a name's last component, as
/// [`Path::extension`] finds it: a hidden file's name has no extension.
fn extension(name: &str) -> Option<&str> {
    let file = file_name(name);
    let (stem, extension) = file.rsplit_once('.')?;
    (!stem.is_empty()).then_some(extension)
}

/// A name's last component with the extension of `mode`.
fn renamed(name: &str, mode: Mode) -> String {
    let file = file_name(name);
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    format!("{stem}.{}", mode.extension())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_extension_states_the_mode() {
        assert_eq!(Mode::of("a.crod"), Some(Mode::Data));
        assert_eq!(Mode::of("data/a.b.crov"), Some(Mode::Value));
        assert_eq!(Mode::of(r"data\a.crov"), Some(Mode::Value));
        assert_eq!(Mode::of("embedded:data/base.crod"), Some(Mode::Data));
        assert_eq!(Mode::of("https://example.org/a.crov"), Some(Mode::Value));
        for name in ["a", "a.crol", "a.txt", "a.CROD", ".crod", "a.crod/b"] {
            assert_eq!(Mode::of(name), None, "{name}");
        }
    }

    #[test]
    fn a_name_that_states_nothing_passes() {
        for name in ["embedded:base", "a.txt", "memory:42"] {
            Mode::Data.check(name).unwrap();
            Mode::Value.check(name).unwrap();
        }
    }

    #[test]
    fn the_messages_name_the_file_to_rename_to() {
        let message = |mode: Mode, name| mode.check(name).unwrap_err().to_string();
        assert_eq!(
            message(Mode::Data, "data/a.crol"),
            "data/a.crol: the `.crol` extension is retired; \
             a layer's document is `.crod`, so rename it to `a.crod`"
        );
        assert_eq!(
            message(Mode::Value, "project.crol"),
            "project.crol: the `.crol` extension is retired; \
             a value document is `.crov`, so rename it to `project.crov`"
        );
        assert!(message(Mode::Data, "a.crov").contains("cannot be a layer"));
        assert!(message(Mode::Value, "a.crod").contains("cannot be read as a value document"));
    }
}
