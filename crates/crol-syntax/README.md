# crol-syntax

Lexer, error-recovering parser, lossless concrete syntax tree and typed AST
for [CROL](https://github.com/DreamLogics/crol), the Composable Rust Object
Language.

```rust
let parse = crol_syntax::parse("+web: Service { replicas: 2, .. },");
assert!(parse.errors().is_empty());
assert_eq!(parse.syntax().to_string(), "+web: Service { replicas: 2, .. },");
```

The parser always produces a tree that holds every byte of the input, and
reports every syntax error it finds, so editors get a tree for half-written
files. Most users want the [`crol`](https://crates.io/crates/crol) crate
instead.

Licensed under either of Apache License, Version 2.0 or MIT licence at your
option.
