# Data mode and value mode

A CROL file is one of two things, and its extension tells you which:

- **`.crol`** is a _value document_: the whole file is one value, like a JSON or
  RON file. This is the default, and where you start.
- **`.crod`** is a _data document_: the file holds operations on data shared by
  every file of a composition, in layers. You move to it when you want layers and
  the more advanced features that come with them.

Look at a file's name and you know how it behaves. This page explains both, and
when to use each. The other pages link here rather than repeating it.

## The short version

|                         | `.crol`, value mode              | `.crod`, data mode                        |
| ----------------------- | -------------------------------- | ----------------------------------------- |
| You read it with        | `crol::from_str`                 | `CrolComposer`                            |
| A file is               | one value                        | a list of operations on shared data       |
| How many files          | one                              | any number, in layers                     |
| Good for                | a single settings file           | data split over files, sources or environments |
| The `crol` command line | [one file at a time](#tools-and-modes) | all commands                        |

Start with a `.crol` file: unless you specifically need more, it is all you need.
When you want overrides, environments or more than one file, move to `.crod`
files.

Project files and schema files are single values, so they are `.crol` files:
`project.crol`, `crol/schema.crol`.

There are two modes because they serve different purposes, and not everything
fits one shape: a settings file wants to be a value with a type, and layered data
wants operations that files can stack. The extensions make sure you never have to
wonder which you are looking at.

## Value mode: a file is a value

In value mode, the whole file is a single expression, like a JSON or RON file.
It does not have to be a struct:

```rust
// src/main.rs
fn main() -> Result<(), crol::Error> {
    let total: i32 = crol::from_str("40 + 2")?;
    let list: Vec<i32> = crol::from_str("[1, 2] + [3]")?;
    assert_eq!(total, 42);
    assert_eq!(list, [1, 2, 3]);
    Ok(())
}
```

A file of entries builds a keyed value, and it can carry a type name, either
on outer braces or with `#![type(...)]` when you leave the braces out. Both of
these deserialize to the same `Config`, and the name is checked against your
Rust type:

```crol
// config.crol
Config {
    port: 8080,
    workers: 4,
}
```

```crol
// config.crol
#![type(Config)]

port: 8080
workers: 4
```

Because the file is one value, it can use everything a block can, such as `..`
to leave fields out, and a spread at the top:

```crol
// config.crol
#![type(Config)]

defaults := { workers: 4 }

..&defaults
port: 8080
..
```

Entries apply in order, as they do in any block, so a later `port: 80`
replaces an earlier one.

## Data mode: a file is operations

In a `.crod` file, the file does not describe a value. It holds operations on
the _root_, a set of keys shared by every file. `+web: ...` means "add `web` to
the data". The composer gathers the operations of all files, orders them by
[layer](layers.md), and evaluates the result.

```crol
// services.crod
+web: Service { port: 8080, .. }
+database: Service { port: 5432, .. }
```

The root is not a value that the file builds, which explains the rules that
differ from value mode:

- There is no type name at the top, and no `#![type(...)]`.
- There is no spread at the top level. To share fields between root keys, spread
  a [template](language.md#templates-and-spreads) inside each. `..` alone is allowed,
  and lets the fields of [a struct at the root](rust.md#a-struct-at-the-root) take
  their defaults.
- A file must be entries. A bare `[1, 2]` is an error.
- One layer may define a key only once, so that the result does not depend on
  which file happens to be read first. A second definition is an error, even
  in the same file. Changing a value with a [patch](layers.md#definitions-and-patches)
  is fine.

In return, you get everything value mode cannot do: [layers](layers.md),
[variants](variants.md), [templates](language.md#templates-and-spreads) shared
across files, [`#![extends(...)]`](layers.md#declaring-what-a-file-builds-on),
and [`&default`](schemas.md#using-default).

The result is still ordinary Rust data. `compose` deserializes the root
either as a struct whose fields are the root keys, or as a map:

```rust
// src/main.rs
use std::collections::BTreeMap;

use crol::{CrolComposer, CrolLayer};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Service {
    port: u16,
}

fn main() -> Result<(), crol::Error> {
    let layer = CrolLayer::from_sources(
        "base",
        [("services.crod", "+web: Service { port: 80 }, +db: Service { port: 5432 }")],
    );
    let services: BTreeMap<String, Service> =
        CrolComposer::new().load_layer(layer).compose()?;
    for (name, service) in &services {
        println!("{name}: port {}", service.port);
    }
    Ok(())
}
```

```console
$ cargo run
db: port 5432
web: port 80
```

## Side by side

| You write                      | `.crol`, value mode        | `.crod`, data mode                        |
| ------------------------------ | -------------------------- | ----------------------------------------- |
| `port: 80, name: "x"`          | works                      | works                                     |
| `{ port: 80 }`                 | works                      | works; the braces are dropped             |
| `Config { port: 80 }`          | works                      | error: the root has no type               |
| `#![type(Config)]`             | works                      | error                                     |
| `[1, 2]`, `42`                 | works                      | error: a file holds entries               |
| `..` at the top                | works                      | works, in any layer                       |
| `..&x` at the top              | works                      | error: the root is not a value            |
| `x := 1` at the top            | visible in this file       | a template, visible in every file         |
| `x = port: 80` at the top      | works                      | error: write `&root.port`                 |
| `port: 1, port: 2`             | the last one wins          | error: `port` is defined twice            |
| `<prod> { ... }`               | error: there are no contexts | works                                   |
| `#![extends(...)]`             | error: there are no layers | works                                     |
| `&default`                     | error: there is no schema  | works, with `CrolComposer::schema`        |

`&self`, `&root` and `&` work the same way in both. At the top level, `&self`
and `&root` are the same thing, and `&` has nothing below it.

## When the extension does not fit

A file whose extension is wrong for how it is loaded is an error, never a silent
skip:

- A `.crol` file cannot be a layer. This includes a `.crol` file in a layer's
  directory, so a layer never quietly lacks a file. The one exception is the
  [project file](projects.md#how-it-is-found) itself, which may lie there.
- A `.crod` file cannot be a project or schema file.

```console
$ printf '+port: 80\n' > base.crod
$ printf '40 + 2\n' > answer.crol
$ crol check base.crod answer.crol
error: answer.crol: a `.crol` file is a value document and cannot be a layer; move it out of the layer, or rename it to `answer.crod` if it is data
```

Other files in a layer's directory, such as a `README.md`, are not documents and
are ignored. Given by path, a file with any other extension follows what loads it:
`crol check notes.txt` reads it as a layer.

## Text has no extension

Text does not come with a name, so the function you call states the mode.
`crol::from_str` is always value mode, and the composer is always data mode,
also for [`CrolLayer::from_sources`](layers.md#what-a-layer-can-be).

A [file loader](recipes.md#loading-documents-from-somewhere-else) of your own
names its documents. A name that ends in `.crod` or `.crol` follows the same rules
as a file. Any other name states nothing, and the call that loaded it
decides.

## Moving a file between modes

A file of plain entries, with no `#![type]`, no top-level spread and each key set
once, reads the same in both modes. So when one settings file grows into several,
as in [getting started](getting-started.md), all it takes is a new name:
`config.crol` becomes `config.crod`. If you think your program might grow, this
is the style to write in.

## Tools and modes

Because the file states its mode, tools need no flags:

- `crol check`, `crol eval` and `crol fingerprint` read one `.crol` file as a
  value document, and `.crod` files and directories as layers. With a schema they
  check a value document against it: `--schema`, or the file's
  [`#![schema(...)]`](schemas.md#the-schema-attribute). A `.crol` file among
  other layers is an error.
- `crol explain` follows a value through layers, so it does not take a `.crol`
  file. `crol eval file.crol key` prints the value.
- `crol fmt` handles both, and they format the same.
- The [language server](editors.md) goes by the extension too, whether a file is
  in your project's layers or not.

## Next

[Layers](layers.md) explains what stacking files really does.
