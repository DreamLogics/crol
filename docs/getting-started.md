# Getting started

This page takes you from an empty directory to a Rust program that reads its
settings from two CROL files, one on top of the other. It takes about ten
minutes. You need Rust 1.99 or newer.

## What you are building

CROL is a text format for configuration and data. Most formats make you pick
between a file that is easy to write and a program that merges files by hand.
CROL files say what they do to the data: _create this_, _change that_, _remove
the other_. That means several files can be stacked, and the result is the
same on every machine.

You will write one file with defaults, load it into a Rust struct, then add a
second file that changes a value without touching the first.

## Install the tools

CROL is not on crates.io yet, so both the command-line tool and the library
come from the Git repository. Install the `crol` tool first:

```console
$ cargo install --git https://github.com/DreamLogics/crol crol-cli
```

Then create a project:

```console
$ cargo new hello-crol
$ cd hello-crol
```

and add the library, with [serde](https://serde.rs) to do the deserializing:

```toml
# Cargo.toml
[package]
name = "hello-crol"
version = "0.1.0"
edition = "2024"

[dependencies]
crol = { git = "https://github.com/DreamLogics/crol" }
serde = { version = "1", features = ["derive"] }
```

## Your first file

Create `config.crol` next to `Cargo.toml`:

```crol
// config.crol
port: 8080,
listen: "0.0.0.0:" + &root.port,
admin: "127.0.0.1:" + (&root.port + 1),
workers: 4,
```

This looks like a struct literal, and it nearly is. Each line is an _entry_
that gives a key a value, and entries are separated by commas. Two things are
new:

- `&root.port` is a _reference_: it reads the value of `port`.
- `+` works on values: strings concatenate, and numbers add up.

Ask `crol` what the file evaluates to:

```console
$ crol eval config.crol
port: 8080,
listen: "0.0.0.0:8080",
admin: "127.0.0.1:8081",
workers: 4,
```

`crol eval` is the quickest way to see what a file really means. You will use
it a lot.

## Load it into Rust

Any type that implements serde's `Deserialize` can be loaded. Replace
`src/main.rs` with:

```rust
// src/main.rs
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Config {
    port: u16,
    listen: String,
    admin: String,
    workers: u32,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string("config.crol")?;
    let config: Config = crol::from_str(&text)?;
    println!(
        "{} and {} (port {}), workers: {}",
        config.listen, config.admin, config.port, config.workers
    );
    Ok(())
}
```

```console
$ cargo run
0.0.0.0:8080 and 127.0.0.1:8081 (port 8080), workers: 4
```

Your program never sees the `+` or the `&root.port`. By the time serde runs,
the file has been evaluated to plain values.

`crol::from_str` reads one document on its own. That is all many programs
need. If yours is one of them, you can stop here, and read
[the language by example](language.md) when you want to know more.

## Add a second file

Now say that on your machine you want another port and fewer workers. You
could edit `config.crol`, but that file is shared. Instead, write only what
differs:

```crol
// local.crol
port: 9000,
workers: & - 3,
```

`workers: & - 3` uses `&` on its own, which means _the value below_: whatever
`workers` was before this entry. `port: 9000` simply replaces the old value.

Files stacked like this are called [_layers_](layers.md). Give `crol eval`
both, lowest first:

```console
$ crol eval config.crol local.crol
port: 9000,
listen: "0.0.0.0:9000",
admin: "127.0.0.1:9001",
workers: 1,
```

Look at `listen`. `local.crol` never mentions it, and yet it changed. That is
the difference between `&` and the other references: `&` looks down, at what
came before, and `&root.port` looks at the final value, after every layer has
had its say. So one change in a top layer reaches everything that depends on
it.

If you ever wonder where a value came from, ask:

```console
$ crol explain config.crol local.crol workers
workers:
  config.crol:5:1  workers  definition  4
  local.crol:3:1   workers  patch       1
```

To load layers in Rust, use `CrolComposer` instead of `from_str`. Each `load`
adds a layer on top of the ones before it, and `compose` evaluates them and
deserializes the result:

```rust
// src/main.rs
use std::path::Path;

use crol::CrolComposer;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Config {
    port: u16,
    listen: String,
    admin: String,
    workers: u32,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config: Config = CrolComposer::new()
        .load(Path::new("config.crol"))?
        .load(Path::new("local.crol"))?
        .compose()?;
    println!(
        "{} and {} (port {}), workers: {}",
        config.listen, config.admin, config.port, config.workers
    );
    Ok(())
}
```

```console
$ cargo run
0.0.0.0:9000 and 127.0.0.1:9001 (port 9000), workers: 1
```

`load` also takes a directory, in which case every `.crol` file below it is
part of the layer. Real projects usually have a directory per layer.

## When something is wrong

Mistakes are reported with the place they happened. Say you misspell a key in
`local.crol`, in a path that reaches into a value:

```crol
// local.crol
port: 9000,
worker.count: 1,
```

```console
$ crol check config.crol local.crol
error: worker.count: `worker` does not exist, so it cannot be patched
  ┌─ local.crol:3:1
  │
3 │ worker.count: 1,
  │ ^^^^^^^^^^^^
```

`crol check` reads and evaluates the files, prints every problem it finds, and
exits with status 1 if there were errors, so it fits in CI. In Rust, the same
problems come back as a [`crol::Error`](rust.md#errors) from `compose`.

Files that evaluate fine can still be wrong for your types. A misspelt field
is caught when deserializing, and with a [schema](schemas.md) also by
`crol check` and in your editor, while you type.

## Keep your files tidy

`crol fmt` formats files in place, like `rustfmt`:

```console
$ crol fmt config.crol local.crol
```

## Where to go next

- [The language by example](language.md): everything you can write in a file.
- [Data mode and value mode](modes.md): why `from_str` and `CrolComposer` read
  a file differently.
- [Layers](layers.md) and [variants](variants.md): the two ways of combining
  data, by stacking files and by environment.
- [Schemas](schemas.md) and [editor setup](editors.md): completion, hover and
  type errors in your editor.
