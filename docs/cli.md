# Command-line reference

`crol` checks, evaluates, formats and explains CROL data. Install it with:

```console
$ cargo install --git https://github.com/DreamLogics/crol crol-cli
```

| Command                             | Does                                                 |
| ----------------------------------- | ---------------------------------------------------- |
| [`crol init`](#crol-init)           | set up a project                                     |
| [`crol check`](#crol-check)         | read and evaluate layers or a value document, and report problems |
| [`crol eval`](#crol-eval)           | print evaluated data                                 |
| [`crol explain`](#crol-explain)     | print the operations behind a value                  |
| [`crol fingerprint`](#crol-fingerprint) | print a hash of the evaluated data               |
| [`crol fmt`](#crol-fmt)             | format files                                         |
| [`crol from-ron`](#crol-from-ron)   | convert a RON file to CROL                           |
| [`crol lsp`](#crol-lsp)             | run the language server                              |

`crol help <command>` lists a command's options. The examples on this page use
these two layers:

```crol
// data/base/services.crod
+web: { replicas: 2, port: 8080 }
<prod> {
    web.replicas: 8,
}
```

```crol
// data/local/overrides.crod
web.port: 9090
```

## `crol init`

Sets up the Cargo project around the current directory, and takes no other
arguments. `crol --init` is the same thing.

```console
$ crol init
Created /path/to/project/project.crov
Created /path/to/project/data/base/main.crod

Next:
  - add `crol = { git = "https://github.com/DreamLogics/crol" }` to [dependencies] in Cargo.toml, if it is not there yet
  - write your data in data/base/main.crod
  - run `crol check`
```

It looks for `Cargo.toml` in the current directory and then in each directory
above, and writes next to the nearest one:

- `project.crov`, a commented [project file](projects.md#starting-a-project) with
  one layer, `base`;
- `data/base/main.crod`, holding only comments, unless that file exists.

It changes no manifest or source file. It exits with status 1, writing nothing, if
there is no `Cargo.toml`, if `project.crov` exists there, if a leftover
`project.crol` does, or if `data/base.crod` exists, since the module path
`data::base` would then be ambiguous.

## Naming layers

`check`, `eval`, `explain` and `fingerprint` take layers, lowest first. Each
layer is a source, or `name=source` to give it a name. A source is a
[module path](projects.md#module-paths) or a file system path, naming a file or
a directory. If something exists under both readings, the module path is used.

```console
$ crol eval data/base data/local
web: { replicas: 2, port: 9090 }
$ crol eval data::base data::local
web: { replicas: 2, port: 9090 }
$ crol eval base=data/base local=data/local
web: { replicas: 2, port: 9090 }
```

With no layers at all, `crol` looks for a
[project file](projects.md#how-it-is-found), and takes the layers, variants,
options and schema from it. Given layers, it ignores the project file unless you
pass `--project`, in which case the layers are yours and the rest comes from the
project.

## Value documents

[The extension says how a file is read](modes.md). One `.crov` file, on its own,
is read by `check`, `eval` and `fingerprint` as a value document instead of as a
layer:

```console
$ printf 'workers: 2 * 2\n..\n' > settings.crov
$ crol check settings.crov
$ crol eval settings.crov
workers: 4
..
$ crol eval settings.crov workers
4
```

It is checked against `--schema`, else its own
[`#![schema(...)]`](schemas.md#the-schema-attribute). `project.crov` gets the
schema of project files, so `crol check project.crov` checks your project file. A
file that the project's [`files`](projects.md#value-documents-in-a-project) match
is checked as the type they give it.
A value document has no variants, so `--context`, `--all-contexts` and the options
that declare variants are errors.

`crol explain` takes layers only. A `.crov` file among layers is an error, and so
is a `.crol` file anywhere.

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
web: { replicas: 2, port: 9090 }
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
web: { replicas: 8, port: 9090 }
$ crol eval --group dev,prod --all-contexts data/base data/local
web: { replicas: 2, port: 9090 }

<!dev & prod> {
    web: { replicas: 8, port: 9090 },
}
```

The output of `--all-contexts` is valid CROL that evaluates back to the same
data.

In a project, the key can come alone: the layers are the project's.

```crol
// project.crov
Project {
    layers: List([{ name: "base", dir: "data::base", .. }]),
    ..,
}
```

```console
$ crol eval web.port
8080
```

To name other layers and keep the project's variants and schema, add `--project`:

```console
$ crol eval --project project.crov data::base data::local web.port
9090
```

`crol` tells a key from a layer by how the last argument is written, not by what
is on disk: anything that reads as a key or path, such as `web`, `web.port` or
`web.tags[0]`, is a key, unless it ends in `.crod`, `.crov` or `.crol`. So a layer
in the last place cannot be a bare name: write `crol eval ./base` or
`crol eval ::base`, as `crol eval base` asks for the key `base`. Before the last
argument, a bare name is a layer as usual.

## `crol explain`

Prints the operations that produced a value, oldest first, with the value after
each:

```console
$ crol explain data/base data/local web.port
web.port:
  data/base/services.crod:2:2    +web      definition  8080
  data/local/overrides.crod:2:1  web.port  patch       9090
$ crol explain --context prod data/base data/local web.replicas
web.replicas in {prod}:
  data/base/services.crod:2:2  +web                definition  2
  data/base/services.crod:4:5  web<prod>.replicas  patch       8
```

`--context a,b` chooses the context. In a project the layers can be left out, as
for `crol eval`: `crol explain web.port`. The key is required.

## `crol fingerprint`

Prints a hash of all evaluated data, in every context:

```console
$ crol fingerprint data/base data/local
cc4e8c56505c9e470938b39dcaaacd82ef878e5d4ee574d8df4abbdde80aa544
```

It covers values, not text, so it does not change when you reformat a file. Two
machines that print the same fingerprint have the same data.

## `crol fmt`

Formats files in place. Unlike the commands above, it takes files, not layers or
directories, and formats `.crod` and `.crov` [files](modes.md) the same way. A
`.crol` file is an error, with both names to choose from.

```console
$ crol fmt data/base/services.crod
$ crol fmt --check data/base/services.crod
```

With `--check`, it changes nothing and exits with status 1 if a file is not
formatted. `--max-width <n>` sets the maximum line width, by default 100.

The formatter keeps your comments, and at most one blank line between entries. A
block that fits stays on one line, unless you put a line break right after its
`{`. It moves `..` to the end of its block, and removes outer braces that have no
type name.

It writes top-level entries one per line, without commas, and keeps the commas in
nested blocks, including top-level variant blocks and a typed outer block. A comma
stays where a [line break would not separate](language.md#line-breaks-and-commas)
two entries. Binary operators stay at the end of a line, so that wrapping cannot
split an entry in two.

## `crol from-ron`

Converts a RON file to a CROL value document, on standard output, so give the
file you keep it in a `.crov` name. `--max-width <n>` sets the line width. See
[moving from RON](recipes.md#moving-from-ron).

## `crol lsp`

Runs the [language server](editors.md) on standard input and output. You do not
run this by hand: your editor does.

## Machine-readable diagnostics

With `--message-format json`, diagnostics are printed as JSON, one per line, on
standard output:

```console
$ printf 'nope.x: 1,\n' > bad.crod
$ crol check --message-format json data/base bad.crod
{"severity":"error","message":"nope.x: `nope` does not exist, so it cannot be patched","file":"bad.crod","line":1,"column":1,"end_line":1,"end_column":7,"context":null,"related":[]}
```

A problem that is about no place in a file, such as a key that is not a path, has
`null` for `file`, `line` and the columns.

## Exit status

`crol check`, `crol fmt --check` and all other commands exit with status 1 when
they fail. Any error counts, also one that a schema finds: `crol eval` and
`crol fingerprint` then print the problems, but no data and no fingerprint.
Warnings do not count.

## Next

[Editor setup](editors.md) brings all of this into your editor.
