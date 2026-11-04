# crol-eval

The evaluator of [CROL](https://github.com/DreamLogics/crol), the Composable Rust
Object Language: layers, chains of operations, variants, fingerprints and
provenance.

Most users want the [`crol`](../crol) crate, which adds file loading and serde on
top of this one. Use `crol-eval` directly for tools that work on evaluated values
without Rust types:

```rust
use crol_eval::{Sources, evaluate_value};

fn main() {
    let mut sources = Sources::new();
    let file = sources.add("example.crol", "port: 8000 + 80");
    let evaluation = evaluate_value(&sources, file);
    assert!(!evaluation.has_errors());
    assert!(evaluation.value.is_some());
}
```

Evaluation does not stop at the first error: every independent problem ends up in
`evaluation.diagnostics`, with its place in the source.

## Documentation

- [Layers](https://github.com/DreamLogics/crol/blob/main/docs/layers.md)
- [Variants](https://github.com/DreamLogics/crol/blob/main/docs/variants.md)
- [The whole guide](https://github.com/DreamLogics/crol/blob/main/docs/README.md)

## Licence

Licensed under either of Apache License, Version 2.0 or MIT licence at your
option.
