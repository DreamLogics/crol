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
  keys after `&root.`, `&self.` and the like. It knows the type at the cursor at
  any depth: through nested blocks, lists, `Some(...)`, dotted keys and variant
  blocks.
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
evaluates every context. Open files count even when they are not saved. Diagnostics are published for all files of the
writable layers, not only the ones you have open, and `read_only` layers get none.

**Other open files**, and all open files if there is no project, are evaluated on
their own, and checked against their `#![schema(...)]` if they have one. The
[extension](modes.md) gives the mode: a `.crov` file is a value document, and a
`.crod` file is data, as a layer of its own, with its variants and patches.

A `.crod` file is evaluated on top of what its `#![extends(...)]` names, as with
`crol check`, and open files among those count even when they are not saved. Hover,
go to definition and completion see the extended keys, and changing an extended
file evaluates the document again. Errors in an extended file are not shown as if
they were yours: the first one, an `extends` that names nothing and a cycle appear
on the `extends` itself. A `.crod` file with no `extends` only knows itself, so a
patch to a key from another file has nothing below it. Name what it builds on, or
list your layers in `project.crov`.

While a file does not evaluate, as in the middle of an edit, hover and completion
use the last evaluation that did. Hover says so.

A layer that does not load does not stop the others, as it does for `crol check`.
A wrong file in a layer, such as a `.crov` or a `.crol` file, is reported on that
file and left out, and the rest of the layer loads. A layer that cannot be read at
all, such as one whose `dir` names nothing, is reported on the file to blame or on
`project.crov`, and adds nothing: its keys have no values, and the layers after it
are evaluated without it. Errors in those layers are then warnings that say so,
since what is missing may be why. Hover and completion keep working, from the last
evaluation that succeeded or else from the layers below.

The server shows a `.crol` file only an error that says to rename it.

`project.crov` itself is checked against the schema of project files.

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
- use it for files ending in `.crod` and `.crov`;
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
