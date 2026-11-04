# Editor setup

CROL comes with a language server, so your editor can show evaluated values,
report errors while you type, and complete field names from your Rust types.
This page explains what it does and how to set it up.

## What you get

The server is part of the `crol` tool, as `crol lsp`. It works in any editor
that speaks the Language Server Protocol, and needs nothing else to start: it
finds your [project file](projects.md#how-it-is-found) by itself.

- **Diagnostics**: everything `crol check` reports, as you type. Errors that
  occur only in some contexts say which ones.
- **Hover**: on a key, its type and docs from the schema, the value below, the
  final value, and where they came from. On a reference, what it refers to.
- **Completion**: field names, enum variants, type names, ids, variants, and
  keys after `&root.`, `&self.` and the like.
- **Go to definition**: on keys, references, binds and ids. On a bare `&`, it
  takes you to the value below.
- **Find references**: every operation on a key, in every layer, and every
  reference and id that names it.
- **Rename** of binds.
- **Outline and workspace symbols**.
- **Code actions**: add missing fields, or `..`, write the values below as
  comments, and quick fixes for misspelt names.
- **Formatting**, the same as `crol fmt`.
- **Semantic highlighting**, which tells keys, types and references apart.
- **Inlay hints** (off by default): the result next to an expression that
  computes something, such as `& * 4`.

Most of this works better with a [schema](schemas.md). Without one, you still get
syntax errors, evaluation errors and references, but no type checks and no
field names from your types.

## How the server sees your files

Knowing this explains most of what you will see in your editor.

**With a project file**, the server loads every layer and the schema, and
evaluates every context. Files in a layer are read as data mode. Open files count
even when they are not saved. Diagnostics are published for all files of the
writable layers, not only the ones you have open, and `read_only` layers get none.

**Other open files**, and all open files if there is no project, are evaluated on
their own, as [value-mode](modes.md) documents, and checked against their
`#![schema(...)]` if they have one.

That means layered data needs a project file to work well in your editor. Without
one, a file with variants or `#![extends(...)]` reports that it needs layers, and
a patch has nothing below it. The server does not follow `#![extends(...)]` yet,
so list your layers in `project.crol`.

`project.crol` itself is checked against the schema of project files.

## Zed

The extension for [Zed](https://zed.dev) lives in
[`editors/zed`](../editors/zed), and gives you highlighting, brackets,
indentation, the outline and text objects, along with the language server.

It is not in Zed's extension registry yet, so install it from the repository:

1. Install `crol`, and make sure it is on your `PATH`.
2. Get a copy of the repository.
3. In Zed, run **zed: install dev extension**, and choose the `editors/zed`
   directory.

The extension uses the first of these it finds:

1. `binary.path` from your settings;
2. `crol` on the `PATH`;
3. the binary from the latest GitHub release, which it downloads. There are no
   releases yet, so for now one of the first two is required.

Settings go in Zed's `settings.json`, under `lsp.crol`:

```json
{
  "lsp": {
    "crol": {
      "binary": { "path": "/path/to/crol" },
      "initialization_options": {
        "context": ["prod"],
        "inlay_hints": true
      }
    }
  }
}
```

## Other editors

Any LSP client can use the server. Configure it to:

- run `crol lsp`, which talks over standard input and output;
- use it for files ending in `.crol`;
- pass the [settings](#settings) below as `initializationOptions`.

For highlighting, indentation and folding, there is a
[tree-sitter grammar](../tree-sitter-crol), with queries in
[`tree-sitter-crol/queries`](../tree-sitter-crol/queries).

## Settings

| Setting                 | Default | Means                                                                 |
| ----------------------- | ------- | --------------------------------------------------------------------- |
| `context`               | `[]`    | the context for inlay hints, and the first one on hover, as a list of variants |
| `inlay_hints`           | `false` | show inlay hints                                                      |
| `debounce_ms`           | `150`   | the pause in typing before evaluating                                 |
| `max_contexts_in_hover` | `8`     | how many contexts a hover lists values for                            |
| `remote_schemas`        | `true`  | fetch [schemas from URLs and crates](schemas.md#remote-schemas)       |
| `project`               | none    | a project file, instead of searching for one                          |

## Your own checks

CROL leaves checks that only your application can do, like "this port is free",
to your application. To see their results in the editor too, build your own server
from the [`crol-lsp`](../crates/crol-lsp) library, which takes a function that
gets the evaluated data and returns diagnostics.

## When it does not work

See [troubleshooting](troubleshooting.md#in-your-editor).

## Next

[Troubleshooting](troubleshooting.md) explains the most common errors.
