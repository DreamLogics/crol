# crol-syntax

The lexer, error-recovering parser, lossless syntax tree, typed AST and formatter
of [CROL](https://github.com/DreamLogics/crol), the Composable Rust Object
Language.

Most users want the [`crol`](https://crates.io/crates/crol) crate instead. Use this one for tools that
work on source text:

```rust
fn main() {
    let parse = crol_syntax::parse("+web: Service { replicas: 2, .. },");
    assert!(parse.errors().is_empty());
    assert_eq!(parse.syntax().to_string(), "+web: Service { replicas: 2, .. },");
}
```

The parser always produces a tree that holds every byte of the input, and reports
every syntax error it finds, so editors get a tree for half-written files.

## Documentation

- [The language by example](https://github.com/DreamLogics/crol/blob/main/docs/language.md)
- [`crol fmt`](https://github.com/DreamLogics/crol/blob/main/docs/cli.md#crol-fmt), which uses the formatter

## Licence

Licensed under either of Apache License, Version 2.0 or MIT licence at your
option.
