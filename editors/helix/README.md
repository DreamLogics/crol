# CROL for Helix

Syntax highlighting, indentation, text objects and rainbow brackets for
[CROL](https://github.com/DreamLogics/crol) files (`.crod` and `.crol`), from the
tree-sitter grammar in `tree-sitter-crol/`, and the `crol lsp` language server
(`SPEC.md` 12.8).

Helix has no extensions, so this is configuration to add to your own. See
[the guide](../../docs/editors.md#helix) for the steps and the settings.

The queries in `queries/crol/` are not copies of those in
`tree-sitter-crol/queries/`, as Helix names captures differently and has its own
model of indentation. Where several highlight patterns match a node, Helix takes the
last, so the general ones come first; CI checks that the queries compile against the
grammar. The grammar is pinned to a commit in `languages.toml`: after changing the
grammar, update `rev` to a commit that contains the change.
