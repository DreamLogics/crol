# crol-cli

`crol`, the command-line tool for
[CROL](https://github.com/DreamLogics/crol), the Composable Rust Object Language.
It checks, evaluates, formats and explains CROL data, and includes the language
server.

## Install

CROL is not on crates.io yet, so install it from the repository:

```sh
cargo install --git https://github.com/DreamLogics/crol crol-cli
```

## Commands

| Command            | Does                                          |
| ------------------ | --------------------------------------------- |
| `crol init`        | set up a project                              |
| `crol check`       | read and evaluate layers, and report problems |
| `crol eval`        | print evaluated data                          |
| `crol explain`     | print the operations behind a value           |
| `crol fingerprint` | print a hash of the evaluated data            |
| `crol fmt`         | format files                                  |
| `crol from-ron`    | convert a RON file to CROL                    |
| `crol lsp`         | run the language server                       |

```sh
crol eval data/base data/local          # the layers, lowest first
crol eval --context prod data/base data/local web.port
crol explain data/base data/local web.port
crol check                              # the layers of project.crov
```

Schemas named by URL or crate are fetched by default. Build without default
features to leave that out.

## Documentation

- [Command-line reference](https://github.com/DreamLogics/crol/blob/main/docs/cli.md)
- [Getting started](https://github.com/DreamLogics/crol/blob/main/docs/getting-started.md)
- [The whole guide](https://github.com/DreamLogics/crol/blob/main/docs/README.md)

## Licence

Licensed under either of Apache License, Version 2.0 or MIT licence at your
option.
