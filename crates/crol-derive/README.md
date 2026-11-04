# crol-derive

Derive macros for [CROL](https://github.com/DreamLogics/crol):
`#[derive(Schema)]`, which describes a type for tools and `&default` from its
fields and serde attributes, and `#[derive(VariantName)]`, for typed
variants. Use them through `crol` with the `derive` feature:

```toml
crol = { version = "0.1", features = ["derive"] }
```

Licensed under either of Apache License, Version 2.0 or MIT licence at your
option.
