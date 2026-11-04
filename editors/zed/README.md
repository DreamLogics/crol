# CROL for Zed

Syntax highlighting, brackets, indentation, the outline and text objects for
[CROL](https://github.com/DreamLogics/crol) files (`.crol`), from the
tree-sitter grammar in `tree-sitter-crol/`, and the `crol lsp` language
server (`SPEC.md` 12.7).

The extension runs, in order: the `binary.path` from your settings, a `crol`
on the worktree's `PATH`, or the release binary for your platform from the
latest GitHub release, downloaded and cached. Settings go under
`lsp.crol` in Zed's settings:

```json
{
  "lsp": {
    "crol": {
      "binary": { "path": "/path/to/crol" },
      "initialization_options": { "context": ["prod"], "debounce_ms": 150 }
    }
  }
}
```

To try it before it is published, in Zed run **zed: install dev extension**
and choose this directory.

The query files in `languages/crol/` are copies of those in
`tree-sitter-crol/queries/`; CI checks that they are the same. The grammar is
pinned to a commit in `extension.toml`: after changing the grammar, update
`rev` to a commit that contains the change.
