# CROL

**CROL, the Composable Rust Object Language**, is a text format for
configuration and data files that load into Rust data models through
[serde](https://serde.rs). It looks like [RON](https://github.com/ron-rs/ron),
but a document holds *operations* as well as values: it can declare, replace,
patch and remove definitions, including ones written in other files and in
lower-priority *layers*. Many files, from many sources, combine into one data
set with fixed, predictable rules.

```rust
// base/services.crol
service_defaults := { replicas: 1, port: 8080, tags: ["managed"] },

+web: Service {
    ..&service_defaults,
    image: "web:1.4",
    url: "http://web:" + &self.port,  // read late: the final web.port
},

<prod> {
    web.replicas: 4,
},
```

```rust
// local/overrides.crol: a later layer
web.port: 9090,
web.tags: & + ["local"],
```

The full design is in [`SPEC.md`](SPEC.md).

> **Status:** early development. The format and the APIs will change before
> 1.0. Milestone 1 (lexer, parser, syntax tree, formatter) is in progress;
> see the [milestones](SPEC.md#143-milestones).

## Crates

| Crate | Contents | Status |
|---|---|---|
| [`crol`](crates/crol) | the library: `CrolComposer`, serde `Deserializer` and `Serializer`, `from_str`, `to_string` | planned |
| [`crol-syntax`](crates/crol-syntax) | lexer, error-recovering parser, lossless syntax tree, typed AST, formatter | in progress |
| [`crol-eval`](crates/crol-eval) | layers, chains, the evaluator, variants, fingerprints | planned |
| [`crol-derive`](crates/crol-derive) | `#[derive(Schema)]` and `#[derive(VariantName)]` | planned |
| [`crol-lsp`](crates/crol-lsp) | the language server, as a library | planned |
| [`crol-cli`](crates/crol-cli) | the `crol` command-line tool | planned |

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
