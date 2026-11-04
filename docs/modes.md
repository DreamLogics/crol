# Data mode and value mode

A CROL file can be read in two ways, and your program chooses which. This page
explains both, and when to use each. The other pages link here rather than
repeating it.

## The short version

|                         | Value mode                       | Data mode                                 |
| ----------------------- | -------------------------------- | ----------------------------------------- |
| You read it with        | `crol::from_str`                 | `CrolComposer`                            |
| A file is               | one value                        | a list of operations on shared data       |
| How many files          | one                              | any number, in layers                     |
| Good for                | a single settings file           | data split over files, sources or environments |
| The `crol` command line | [`crol fmt` only](#tools-and-modes) | all commands                           |

If you have one file and always will, use value mode. If you want overrides,
environments or more than one file, use data mode.

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
Config {
    port: 8080,
    workers: 4,
}
```

```crol
#![type(Config)]

port: 8080
workers: 4
```

Because the file is one value, it can use everything a block can, such as `..`
to leave fields out, and a spread at the top:

```crol
#![type(Config)]

defaults := { workers: 4 }

..&defaults
port: 8080
..
```

Entries apply in order, as they do in any block, so a later `port: 80`
replaces an earlier one.

## Data mode: a file is operations

In data mode, a file does not describe a value. It holds operations on the
_root_, a set of keys shared by every file. `+web: ...` means "add `web` to
the data". The composer gathers the operations of all files, orders them by
[layer](layers.md), and evaluates the result.

```crol
// services.crol
+web: Service { port: 8080, .. }
+database: Service { port: 5432, .. }
```

The root is not a value that the file builds, which explains the rules that
differ from value mode:

- There is no type name at the top, and no `#![type(...)]`.
- There is no `..` or spread at the top level. To share fields between root
  keys, spread a [template](language.md#templates-and-spreads) inside each.
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
        [("services.crol", "+web: Service { port: 80 }, +db: Service { port: 5432 }")],
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

| You write                      | Value mode                 | Data mode                                 |
| ------------------------------ | -------------------------- | ----------------------------------------- |
| `port: 80, name: "x"`          | works                      | works                                     |
| `{ port: 80 }`                 | works                      | works; the braces are dropped             |
| `Config { port: 80 }`          | works                      | error: the root has no type               |
| `#![type(Config)]`             | works                      | error                                     |
| `[1, 2]`, `42`                 | works                      | error: a file holds entries               |
| `..` or `..&x` at the top      | works                      | error: the root is not a value            |
| `x := 1` at the top            | visible in this file       | a template, visible in every file         |
| `x = port: 80` at the top      | works                      | error: write `&root.port`                 |
| `port: 1, port: 2`             | the last one wins          | error: `port` is defined twice            |
| `<prod> { ... }`               | error: there are no contexts | works                                   |
| `#![extends(...)]`             | error: there are no layers | works                                     |
| `&default`                     | error: there is no schema  | works, with `CrolComposer::schema`        |

`&self`, `&root` and `&` work the same way in both. At the top level, `&self`
and `&root` are the same thing, and `&` has nothing below it.

## Writing files that work in both

A file of plain entries, with no `#![type]`, no top-level `..` and each key set
once, reads the same in both modes. That is why the
[getting started](getting-started.md) example could move from `from_str` to
`CrolComposer` without changing `config.crol`. If you think your program might
grow from one file to several, this is the style to write in.

## Tools and modes

Your program chooses the mode, but tools have to work it out for themselves:

- `crol check`, `crol eval`, `crol explain` and `crol fingerprint` read every
  file as **data mode**, because they work on layers. A value-mode file that
  happens to be plain entries works; one with `#![type]`, a top-level type
  name or a top-level `..` is rejected. There is no flag to choose value mode.
- `crol fmt` handles both.
- The [language server](editors.md) reads files in a project's layers as data
  mode, and any other open file as value mode, checked against its
  `#![schema(...)]` if it has one. This is how to check a value-mode file
  today, and the reason [layered data needs a project
  file](editors.md#how-the-server-sees-your-files) in your editor.

## Next

[Layers](layers.md) explains what stacking files really does.
