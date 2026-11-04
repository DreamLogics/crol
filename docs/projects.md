# Project structure

Your program knows which layers to load, which variants exist and which schema
applies, because you wrote that in Rust. The `crol` tool and your editor do not
run your program, so they need to be told the same things. That is what a
_project file_ is for. This page explains the project file, how files name each
other, and how to lay out a project.

## Starting a project

In a Cargo project, `crol init` writes a project file and a first layer:

```console
$ crol init
Created /path/to/project/project.crov
Created /path/to/project/data/base/main.crod

Next:
  - add `crol = { git = "https://github.com/DreamLogics/crol" }` to [dependencies] in Cargo.toml, if it is not there yet
  - write your data in data/base/main.crod
  - run `crol check`
```

It looks for the nearest `Cargo.toml`, from the current directory upwards, and
writes next to that. The result is ready to use, and explains itself in comments:

```console
$ cat project.crov
// What `crol` and the language server need to know about this project's data.
Project {
    // The data, lowest layer first: each layer patches the ones before it.
    // `dir` is a module path from this file: `data::base` is the directory `data/base`.
    layers: List([{ name: "base", dir: "data::base", .. }]),

    // Variants of the data, such as `[Grouped(["dev", "prod"]), Discrete("cache")]`.
    variants: [],

    // The schema file your application writes, to check the data's types:
    // `Some("crol::schema")` is `crol/schema.crov` next to this file.
    schema: None,
    ..,
}
$ crol check
```

`data/base/main.crod` holds only comments. It exists because a layer's directory
has to exist, and an empty directory would not survive version control.

`crol init` never overwrites anything, and leaves `Cargo.toml` and your code alone:

```console
$ crol init
error: /path/to/project/project.crov already exists; it was left as it is
```

The dependency on `crol` is yours to add. Until the crate is on crates.io, use the
[Git dependency](getting-started.md#install-the-tools) rather than the version the
command prints.

## The project file

`project.crov` lives at the top of your project:

```crol
// project.crov
Project {
    layers: List(
        [
            { name: "base", dir: "data::base", read_only: true },
            { name: "extension", dir: "data::extension", .. },
            { name: "local", dir: "data::local", .. },
        ],
    ),
    variants: [Grouped(["dev", "prod", "prod_eu"]), Discrete("cache")],
    parents: { prod_eu: "prod" },
    ..,
}
```

```crol
// data/base/services.crod
+web: { port: 80 }
<prod> {
    web.port: 443,
}
```

```crol
// data/extension/cache.crod
web.cached: true
```

```crol
// data/local/overrides.crod
web.port: & + 8000
```

This project has three layers and two kinds of variant. Only `layers` is
required, and `..` at the end leaves out the rest. Once you have a
[schema](schemas.md), add `schema: Some("crol::schema"),` to check against it.

| Field      | Holds                                                                                        |
| ---------- | -------------------------------------------------------------------------------------------- |
| `layers`   | the layers, lowest first: `List([...])`, or a [command](#layers-from-a-command)                |
| `variants` | `Grouped([...])` and `Discrete("...")` entries, as you declare in [Rust](variants.md#declaring-variants) |
| `parents`  | variant parents, from child to parent                                                        |
| `schema`   | the [schema](schemas.md#in-a-project-file): a module path as a string, a URL, or a crate     |
| `root`     | the [root type](schemas.md#which-type-a-document-has), if it is not the schema's own         |
| `options`  | the same settings as [`CrolConfig`](rust.md#configuration)                                   |

Each layer has a `name`, a `dir` and `read_only`. `dir` is a
[module path](#module-paths) written as a string, starting at the project's
directory, and names a directory or a file. `read_only` is for generated or
vendored data: the layer is loaded, but your editor does not report problems in
it.

`options` takes `implicit_some`, `allow_unknown_fields`, `require_complete`,
`warn_cross_file_patches`, `max_contexts`, `contexts`, and `variant_keys`, which
is written as a string here: `"NoNewKeys"`.

A project file also has a `files` field, meant to give value-mode documents a
type. It is accepted, but no tool reads it yet. Use
[`#![schema(...)]`](schemas.md#the-schema-attribute) in those documents instead.

### How it is found

`crol` looks for `project.crov` in the current directory and then in each
directory above it. The language server starts from your workspace folder. You
can also be explicit, with `--project <file>`, or the `project`
[setting](editors.md#settings) of the language server.

Once it is found, plain commands use it, from any directory of the project:

```console
$ crol check
$ crol eval --context prod_eu
web: { port: 443, cached: true }
```

Variants and options come from the project file too, so you do not have to
repeat them.

A project file is used only when you do not name layers yourself. With layers on
the command line, the project file is ignored, unless you pass `--project`.

The project file itself is never read as data, even if it lies inside a layer's
directory. It is the only `.crov` file that may be there: any other one, also one
called `project.crov` that is not _the_ project file, is [an
error](modes.md#when-the-extension-does-not-fit).

A leftover `project.crol`, where `project.crov` is looked for, is an error that
says to rename it, rather than a project that is not found.

### Layers from a command

If the load order is itself data, such as extensions that the user enabled, a
project can get its layers from a command. It runs in the project's directory,
and prints a list of layers as CROL:

```crol
Project {
    layers: Command(["my-app", "list-layers"]),
    ..,
}
```

```crol
[
    { name: "base", dir: "data::base", read_only: true },
    { name: "extension", dir: "extensions::shop", read_only: false },
]
```

## Keeping your program and tools in step

**Your program does not read `project.crov` by itself**, and tools do not know
about [file loaders](recipes.md#loading-documents-from-somewhere-else) in your
program. `CrolComposer` loads what you tell it to. If the two disagree, your editor will show you one thing
and your program will load another.

For a small project, it is easy enough to repeat four lines. If you would rather
have one source of truth, read the project file in your program. The
`crol::project` module is public for this reason:

```rust
// src/main.rs
use std::collections::BTreeMap;

use crol::project::Project;
use crol::{CrolComposer, CrolConfig, CrolVariants, FileLoaders, Variant};
use serde::Deserialize;

#[derive(Deserialize)]
struct Web {
    port: u16,
    cached: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let file = Project::find(&std::env::current_dir()?)?.ok_or("no project.crov")?;
    let project = Project::read(&file)?;
    let dir = file.parent().ok_or("no parent directory")?;

    let mut variants = CrolVariants::new();
    for variant in project.variants.clone() {
        variants = match variant {
            Variant::Discrete(name) => variants.add_discrete(name),
            Variant::Grouped(names) => variants.add_grouped(names),
        };
    }
    for (child, parent) in project.parents.clone() {
        variants = variants.add_parent(child, parent);
    }

    // The project file may lie in a layer's directory.
    let loaders = FileLoaders::new().with_project_file(&file);
    let mut composer = CrolComposer::with_cfg(CrolConfig {
        variants,
        root_path: dir.to_path_buf(),
        ..Default::default()
    });
    for layer in project.layer_list(dir)? {
        let source = loaders.resolve(&layer.dir, dir)?;
        composer = composer.load_layer(source.load(layer.name)?);
    }

    let root: BTreeMap<String, Web> = composer.with_variant("prod")?.compose()?;
    println!("port {}, cached: {}", root["web"].port, root["web"].cached);
    Ok(())
}
```

```console
$ cargo run
port 443, cached: true
```

## Module paths

CROL files and code name other files with _module paths_, as Rust names modules.
They use `::` rather than `/`, so that they read the same on every platform.

| Path                        | Starts from                                         |
| --------------------------- | --------------------------------------------------- |
| `::defaults::services`      | the root directory                                  |
| `defaults::services`        | the directory of the document; in code, the root directory |
| `super::defaults::services` | the parent of that directory; `super` can repeat    |

The last segment names either the file `services.crod` or the directory
`services/`. If both exist, it is an error. Every other segment is a directory.

A module path has no extension, because what it is used for already says which
one it has: a layer or an `extends` is `.crod`, and a `schema` is the file
`services.crov`. A file of the other kind is an error that says so.

The **root directory** depends on who is asking:

| Who                       | Root directory                                                     |
| ------------------------- | ------------------------------------------------------------------ |
| your program              | `CrolConfig::root_path`, by default the working directory          |
| `crol`                    | the project file's directory, else the working directory           |
| the language server       | the project file's directory, else the workspace folder            |

Module paths can only name files and directories whose names are identifiers
and not hidden. So `my-file.crod` cannot be reached this way, although your
program can still load it by file system path. Prefer `my_file.crod`.

## Recommended layouts

### One settings file

If your program reads one file, you need no project file. Make it a
[`.crov` file](modes.md), and if you have a schema, name it in the file with
`#![schema(...)]`.

```text
my-app/
├── Cargo.toml
├── config.crov
└── src/
    └── main.rs
```

### An application with layered data

This is the layout to start from for most applications with real data.
[`crol init`](#starting-a-project) gives you `project.crov` and `data/base`:

```text
my-app/
├── Cargo.toml
├── project.crov            what tools need to know
├── crol/
│   └── schema.crov         generated by the test below; commit it
├── data/
│   ├── base/               one directory per layer
│   │   ├── templates.crod
│   │   ├── services.crod
│   │   └── volumes.crod
│   └── local/              overrides for one machine
│       └── overrides.crod
├── src/
│   ├── lib.rs              the types, with #[derive(crol::Schema)]
│   └── main.rs             CrolComposer
└── tests/
    └── schema.rs           crol::schema::check_file
```

A few things to keep in mind:

- **One directory per layer.** Every `.crod` file below a layer directory is
  part of it, and a `.crov` file there is an error, so keep `crol/schema.crov`
  out of the layers. A directory that is not listed in the project is not loaded.
- **Split a layer by subject.** Each key can be defined only once per layer, so
  put a key's definition and its patches in the same file, and avoid the
  [warning about patches across files](layers.md#definitions-and-patches).
- **Keep templates in the lowest layer.** Templates are visible everywhere, but
  each one can be defined only once.
- **A layer has to exist.** `crol` stops with an error if a directory in the
  project file is missing. If `data/local` is ignored by Git, commit an empty
  directory for it (with a `.gitkeep` file, for example), or see
  [optional layers](recipes.md#an-optional-local-layer).

### An application that users extend

If others add to your data, ship your data as a `read_only` layer, and let each
extension be a layer of its own, perhaps through a
[layer command](#layers-from-a-command). Ship `crol/schema.crov` as well, or
publish it, so that people editing the data get the same checks without your
source.

```text
my-game/
├── project.crov
├── crol/schema.crov
├── data/
│   └── base/               read_only: true
└── extensions/
    ├── more_maps/
    └── harder_enemies/
```

### A library whose types appear in other programs

If your crate's types show up in other programs' configuration, publish the
schema with the crate, and let users name it:
[`#![schema("crate:your-crate@1")]`](schemas.md#remote-schemas).

## Next

[Recipes](recipes.md) has solutions to common tasks.
