# CROL for Zed

Syntax highlighting, brackets, indentation, the outline and text objects for
[CROL](https://github.com/DreamLogics/crol) files (`.crol`), from the
tree-sitter grammar in `tree-sitter-crol/`. The language server comes later
(`SPEC.md` 12.7).

To try it before it is published, in Zed run **zed: install dev extension**
and choose this directory.

The query files in `languages/crol/` are copies of those in
`tree-sitter-crol/queries/`; CI checks that they are the same. The grammar is
pinned to a commit in `extension.toml`: after changing the grammar, update
`rev` to a commit that contains the change.
