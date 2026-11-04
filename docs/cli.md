# Command-line reference

`crol` checks, evaluates, formats and explains CROL data. Install it with:

```console
$ cargo install --git https://github.com/DreamLogics/crol crol-cli
```

| Command                             | Does                                                 |
| ----------------------------------- | ---------------------------------------------------- |
| [`crol check`](#crol-check)         | read and evaluate layers, and report problems        |
| [`crol eval`](#crol-eval)           | print evaluated data                                 |
| [`crol explain`](#crol-explain)     | print the operations behind a value                  |
| [`crol fingerprint`](#crol-fingerprint) | print a hash of the evaluated data               |
| [`crol fmt`](#crol-fmt)             | format files                                         |
| [`crol from-ron`](#crol-from-ron)   | convert a RON file to CROL                           |
| [`crol lsp`](#crol-lsp)             | run the language server                              |

`crol help <command>` lists a command's options. The examples on this page use
these two layers:

```crol
// data/base/services.crol
+web: { replicas: 2, port: 8080 },
<prod> {
    web.replicas: 8,
},
```

```crol
// data/local/overrides.crol
web.port: 9090,
```

## Naming layers

`check`, `eval`, `explain` and `fingerprint` take layers, lowest first. Each
layer is a source, or `name=source` to give it a name. A source is a
[module path](projects.md#module-paths) or a file system path, naming a file or
a directory. If something exists under both readings, the module path is used.

```console
$ crol eval data/base data/local
web: { replicas: 2, port: 9090 },
$ crol eval data::base data::local
web: { replicas: 2, port: 9090 },
$ crol eval base=data/base local=data/local
web: { replicas: 2, port: 9090 },
```

With no layers at all, `crol` looks for a
[project file](projects.md#how-it-is-found), and takes the layers, variants,
options and schema from it. Given layers, it ignores the project file unless you
pass `--project`, in which case the layers are yours and the rest comes from the
project.

## Common options

These work on `check`, `eval`, `explain` and `fingerprint`:

| Option                    | Means                                                             |
| ------------------------- | ----------------------------------------------------------------- |
| `--group a,b,c`           | declare a [group of alternatives](variants.md#declaring-variants); repeatable |
| `--discrete a,b`          | declare flags                                                     |
| `--parent child=parent`   | give a variant a parent; repeatable                               |
| `--project <file>`        | use this project file                                             |
| `--schema <file>`         | check against this [schema](schemas.md) file                      |
| `--offline`               | fetch no remote schemas; use cached copies and local crates       |

Variants you declare on the command line replace those of the project file.
Without any declaration, every name the data uses counts as a discrete variant.

Every command also takes these:

| Option                                  | Means                                                  |
| --------------------------------------- | ------------------------------------------------------ |
| `--color auto\|always\|never`           | when to colour diagnostics                             |
| `--message-format human\|json`          | how to print diagnostics, see [below](#machine-readable-diagnostics) |

## `crol check`

Reads and evaluates the layers in every context, checks them against the schema
if there is one, and prints every problem. Exits with status 1 if there are
errors; warnings do not count.

```console
$ crol check data/base data/local
```

No output means all is well. This is the command for CI.

## `crol eval`

Prints the data as CROL. Give a key or path after the layers to print one value:

```console
$ crol eval data/base data/local
web: { replicas: 2, port: 9090 },
$ crol eval data/base data/local web.port
9090
```

| Option              | Means                                                                  |
| ------------------- | ---------------------------------------------------------------------- |
| `--context a,b`     | evaluate in the context with these variants; default: the base         |
| `--all-contexts`    | print the base, then what differs in every other context               |
| `--no-defaults`     | with a schema, do not write out [default values](schemas.md#checking-data-against-it) |
| `--max-width <n>`   | the maximum line width, by default 100                                 |

```console
$ crol eval --context prod data/base data/local
web: { replicas: 8, port: 9090 },
$ crol eval --group dev,prod --all-contexts data/base data/local
web: { replicas: 2, port: 9090 },

<!dev & prod> {
    web: { replicas: 8, port: 9090 },
},
```

The output of `--all-contexts` is valid CROL that evaluates back to the same
data.

The last argument counts as a key only if there is at least one layer before it
and it does not name a layer. So in a project, `crol eval web.port` does not work
yet: `web.port` is read as a layer. Name the layers, with `--project` to keep
the project's variants and schema:

```crol
// project.crol
Project {
    layers: List([{ name: "base", dir: "data::base", .. }]),
    ..,
}
```

```console
$ crol eval --project project.crol data::base data::local web.port
9090
```

## `crol explain`

Prints the operations that produced a value, oldest first, with the value after
each:

```console
$ crol explain data/base data/local web.port
web.port:
  data/base/services.crol:2:2    +web      definition  8080
  data/local/overrides.crol:2:1  web.port  patch       9090
$ crol explain --context prod data/base data/local web.replicas
web.replicas in {prod}:
  data/base/services.crol:2:2  +web                definition  2
  data/base/services.crol:4:5  web<prod>.replicas  patch       8
```

`--context a,b` chooses the context. The layers are required here, even in a
project: `crol explain web.port` is an error.

## `crol fingerprint`

Prints a hash of all evaluated data, in every context:

```console
$ crol fingerprint data/base data/local
cc4e8c56505c9e470938b39dcaaacd82ef878e5d4ee574d8df4abbdde80aa544
```

It covers values, not text, so it does not change when you reformat a file. Two
machines that print the same fingerprint have the same data.

## `crol fmt`

Formats files in place. Unlike the commands above, it takes files, not layers,
and handles [both modes](modes.md).

```console
$ crol fmt data/base/services.crol
$ crol fmt --check data/base/services.crol
```

With `--check`, it changes nothing and exits with status 1 if a file is not
formatted. `--max-width <n>` sets the maximum line width, by default 100.

The formatter keeps your comments, and at most one blank line between entries. A
block that fits stays on one line, unless you put a line break right after its
`{`. It moves `..` to the end of its block, and removes the outer braces of a
data-mode document.

## `crol from-ron`

Converts a RON file to CROL, on standard output. `--max-width <n>` sets the line
width. See [moving from RON](recipes.md#moving-from-ron).

## `crol lsp`

Runs the [language server](editors.md) on standard input and output. You do not
run this by hand: your editor does.

## Machine-readable diagnostics

With `--message-format json`, diagnostics are printed as JSON, one per line, on
standard output:

```console
$ printf 'nope.x: 1,\n' > bad.crol
$ crol check --message-format json data/base bad.crol
{"severity":"error","message":"nope.x: `nope` does not exist, so it cannot be patched","file":"bad.crol","line":1,"column":1,"end_line":1,"end_column":7,"context":null,"related":[]}
```

## Exit status

`crol check`, `crol fmt --check` and all other commands exit with status 1 when
they fail. One case to watch for: `crol eval` reports problems that a schema
finds, but still prints the data and exits with status 0, so use `crol check` in
scripts.

## Next

[Editor setup](editors.md) brings all of this into your editor.
