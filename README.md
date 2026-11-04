# CROL

**CROL, the Composable Rust Object Language**, is a text format for
configuration and data files that load into Rust data models through
[serde](https://serde.rs). It looks like [RON](https://github.com/ron-rs/ron),
but a document holds _operations_ as well as values: it can declare, replace,
patch and remove definitions, including ones written in other files and in
lower-priority _layers_. Many files, from many sources, combine into one data
set with fixed, predictable rules.

```crol
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

```crol
// local/overrides.crol: a later layer
web.port: 9090,
web.tags: & + ["local"],
```

The full design is in [`SPEC.md`](SPEC.md).

> **Status:** early development. The format and the APIs will change before
> 1.0. Milestones 1 to 8 are done, and parts of 9 and 10: the parser and
> formatter, single documents, layers and variants with `CrolComposer`,
> provenance, the tree-sitter grammar, schemas with
> `#[derive(crol::Schema)]`, `crol::check`, `&default` and project files,
> and the language server with the Zed extension. See the
> [milestones](SPEC.md#143-milestones) and the CHANGELOG.

## Crates

| Crate                                  | Contents                                                                                    | Status                                                          |
| -------------------------------------- | ------------------------------------------------------------------------------------------- | --------------------------------------------------------------- |
| [`crol`](crates/crol)                  | the library: `CrolComposer`, serde `Deserializer` and `Serializer`, `from_str`, `to_string` | done                                                            |
| [`crol-syntax`](crates/crol-syntax)    | lexer, error-recovering parser, lossless syntax tree, typed AST, formatter                  | done                                                            |
| [`crol-eval`](crates/crol-eval)        | layers, chains, the evaluator, variants, fingerprints                                       | done                                                            |
| [`crol-derive`](crates/crol-derive)    | `#[derive(Schema)]` and `#[derive(VariantName)]`                                            | done                                                            |
| [`crol-lsp`](crates/crol-lsp)          | the language server, as a library                                                           | diagnostics, symbols, formatting, hover, completion, definition |
| [`crol-cli`](crates/crol-cli)          | the `crol` command-line tool                                                                | done                                                            |
| [`tree-sitter-crol`](tree-sitter-crol) | the tree-sitter grammar and queries                                                         | done                                                            |
| [`editors/zed`](editors/zed)           | the Zed extension                                                                           | syntax and language server                                      |

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
