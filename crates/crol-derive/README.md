# crol-derive

Derive macros for [CROL](https://github.com/DreamLogics/crol), the Composable
Rust Object Language. Use them through `crol`, with the `derive` feature, rather
than depending on this crate:

```toml
[dependencies]
crol = { version = "0.1", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
```

`#[derive(crol::Navigable)]` gives a type paths for `CrolDb`, and `crol::croxp!`
checks an expression when the program is compiled.

`#[derive(crol::Schema)]` describes a type for tools and for `&default`, from its
fields, doc comments and serde attributes. `#[derive(crol::VariantName)]` lets an
enum without fields name your variants, in snake case:

```rust
use serde::Deserialize;

/// A service that runs one container image.
#[derive(Deserialize, crol::Schema)]
pub struct Service {
    /// The port it listens on.
    pub port: u16,
    /// What must be running first.
    #[crol(refers(Service))]
    pub depends_on: Vec<crol::Id>,
}

#[derive(Clone, PartialEq, Eq, crol::VariantName)]
pub enum Env {
    Dev,
    ProdEu, // `prod_eu` in data
    #[crol(rename = "live")]
    Production,
}
```

| Attribute                          | On           | Means                                                 |
| ---------------------------------- | ------------ | ----------------------------------------------------- |
| `#[crol(defaults)]`                | a type       | record default values                                 |
| `#[crol(explicit_defaults)]`       | a type       | fields left out always need `..`; `= false`: they never do |
| `#[crol(refers(Service, Volume))]` | an id field  | the root types its ids may name                       |
| `#[crol(schema = OtherType)]`      | a field      | describe it as a type with the same serialized shape  |
| `#[crol(schema = any)]`            | a field      | accept anything here                                  |
| `#[crol(rename = "...")]`          | an enum variant | the name data uses, for `VariantName`              |
| `#[crol(leaf)]`                    | a field      | a path ends here, for a type without `Navigable`     |

## Documentation

- [Schemas](https://github.com/DreamLogics/crol/blob/main/docs/schemas.md)
- [Typed variants](https://github.com/DreamLogics/crol/blob/main/docs/variants.md#typed-variants)
- [Changing data with a database](https://github.com/DreamLogics/crol/blob/main/docs/rust.md#changing-data-with-a-database)

## Licence

Licensed under either of Apache License, Version 2.0 or MIT licence at your
option.
