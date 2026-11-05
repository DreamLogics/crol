# CROL

The Composable Rust Object Language: configuration and data files that combine
with fixed rules, and load into your Rust types through
[serde](https://serde.rs). It is:

- **Composable**: split data over many files and sources. A file says what it
  adds, changes or removes, and CROL merges the result without code on your
  side.
- **Predictable**: the same files give the same data on every machine, and
  `crol explain` shows how any value got its value.
- **Rust-native**: it reads like Rust, loads into ordinary `Deserialize` types,
  and generates its schema from those types. There is no schema language to
  learn.

[![Build Status][actions-badge]][actions-url]

[Guide](docs/README.md) |
[Getting started](docs/getting-started.md) |
[Language](docs/language.md) |
[CLI](docs/cli.md) |
[Editors](docs/editors.md)

[actions-badge]: https://github.com/DreamLogics/crol/actions/workflows/ci.yml/badge.svg
[actions-url]: https://github.com/DreamLogics/crol/actions/workflows/ci.yml

## Overview

A file's extension says how it behaves: a `.crol` file, the default, is one value,
and a `.crod` file is data that [stacks in layers](docs/modes.md).

CROL looks like [RON](https://github.com/ron-rs/ron), but a document holds
_operations_ as well as values: it can declare, replace, patch and remove
definitions, including ones written in other files. At a high level, it gives
you:

- [Layers](docs/layers.md): stack base data, extensions and local overrides,
  each in its own files.
- [Variants](docs/variants.md): differences per environment, region or feature,
  written next to the value they change.
- [Schemas](docs/schemas.md), generated from Rust with
  `#[derive(crol::Schema)]`, for type checks without running your program.
- [A command-line tool](docs/cli.md) to check, evaluate, format and explain data.
- [A language server](docs/editors.md) with completion, hover and diagnostics,
  and a Zed extension.

## Example

A base file defines two services. `..&service_defaults` copies in a template,
and `&self.port` reads the final value of `port`, whichever file sets it:

```crol
// data/base/services.crod
service_defaults := { replicas: 1, port: 8080 }

+web: Service {
    ..&service_defaults,
    image: "web:1.4",
    url: "http://web:" + &self.port,
}

+worker: Service {
    ..&service_defaults,
    image: "worker:1.4",
    url: "http://worker:" + &self.port,
}

<prod> {
    web.replicas: 4,
}
```

A second file, in a higher layer, changes what it needs to and nothing else:

```crol
// data/local/overrides.crod
web.port: 9090
web.replicas: & + 1
```

Make sure you add `crol` and `serde` to your `Cargo.toml`. CROL is not on
crates.io yet:

```toml
[dependencies]
crol = { git = "https://github.com/DreamLogics/crol" }
serde = { version = "1", features = ["derive"] }
```

Then load the layers into your own types, lowest first:

```rust
// src/main.rs
use std::collections::BTreeMap;
use std::path::Path;

use crol::CrolComposer;
use serde::Deserialize;

#[derive(Deserialize)]
struct Service {
    replicas: u32,
    port: u16,
    image: String,
    url: String,
}

fn main() -> Result<(), crol::Error> {
    let services: BTreeMap<String, Service> = CrolComposer::new()
        .load(Path::new("data/base"))?
        .load(Path::new("data/local"))?
        .with_variant("prod")?
        .compose()?;

    for (name, service) in &services {
        println!(
            "{name}: {} x{} at {} (port {})",
            service.image, service.replicas, service.url, service.port
        );
    }
    Ok(())
}
```

```console
$ cargo run
web: web:1.4 x4 at http://web:9090 (port 9090)
worker: worker:1.4 x1 at http://worker:8080 (port 8080)
```

`web.url` changed even though the local file never mentions it, and `<prod>`
applied on top of everything else. More examples are in
[getting started](docs/getting-started.md).

## Getting help

Start with [the guide](docs/README.md), which reads from top to bottom, and
[troubleshooting](docs/troubleshooting.md) for common errors. If that does not
answer your question, please
[open an issue](https://github.com/DreamLogics/crol/issues).

[`SPEC.md`](SPEC.md) is the precise definition of the format. You do not need it
to use CROL, but it settles the fine points.

## Performance

CROL is built to load a whole game's data at every start. On the data of a real-time
strategy game, with about 14,000 definitions, 8 MB of text in 3,200 files, a mod layer
over them, and 9 variants, so 10 contexts:

| Load and evaluate everything | Time | Peak memory |
| --- | --- | --- |
| From text | 522 ms | 373 MB |
| From text, on one core | 758 ms | 265 MB |
| Base layer from a [`.crob` file](docs/layers.md#compiled-layers) | 280 ms | 330 MB |
| Base layer from a `.crob` file, on one core | 406 ms | 272 MB |

That is `compose_data` on an Intel Core i7-13700K, the median of 5 runs with the files in
the page cache, stopping at evaluated values, before any deserializing. Evaluating one
more context afterwards takes about 50 ms. CROL uses a few threads only when the data is
large enough to gain from them, and they cost memory: `CrolConfig::threads` sets how many
evaluate contexts.

`cargo bench -p crol-eval --bench game` measures data of the same shape on your machine.
[`PERFORMANCE.md`](PERFORMANCE.md) has the method and the full figures.

## Status

CROL is in early development: the format and the APIs will change before 1.0.
Nothing is published yet, so there is no crate on crates.io, no binary release
and no entry in Zed's extension registry. Install from this repository, as the
[guide](docs/getting-started.md#install-the-tools) describes.

Of the [milestones](SPEC.md#143-milestones), everything is in place except
publishing the Zed extension. See the [changelog](CHANGELOG.md), and
[troubleshooting](docs/troubleshooting.md) for known rough edges.

## Crates

| Crate                                  | Contents                                                                                    | Status                                                                                                                                |
| -------------------------------------- | ------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------- |
| [`crol`](crates/crol)                  | the library: `CrolComposer`, serde `Deserializer` and `Serializer`, `from_str`, `to_string` | done                                                                                                                                  |
| [`crol-syntax`](crates/crol-syntax)    | lexer, error-recovering parser, lossless syntax tree, typed AST, formatter                  | done                                                                                                                                  |
| [`crol-eval`](crates/crol-eval)        | layers, chains, the evaluator, variants, fingerprints                                       | done                                                                                                                                  |
| [`crol-derive`](crates/crol-derive)    | `#[derive(Schema)]` and `#[derive(VariantName)]`                                            | done                                                                                                                                  |
| [`crol-lsp`](crates/crol-lsp)          | the language server, as a library                                                           | diagnostics, symbols, formatting, hover, completion, definition, references, rename of binds, semantic tokens, inlay hints, code actions |
| [`crol-cli`](crates/crol-cli)          | the `crol` command-line tool                                                                | done                                                                                                                                  |
| [`tree-sitter-crol`](tree-sitter-crol) | the tree-sitter grammar and queries                                                         | done                                                                                                                                  |
| [`editors/zed`](editors/zed)           | the Zed extension                                                                           | syntax and language server; not in the registry yet                                                                                   |

## Development

```sh
cargo test --workspace              # all tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
UPDATE_EXPECT=1 cargo test          # update snapshot tests, then review the diff
```

The minimum supported Rust version is 1.99. See [CONTRIBUTING.md](CONTRIBUTING.md).

## Licence

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT licence ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
licence, shall be dual licensed as above, without any additional terms or
conditions.
