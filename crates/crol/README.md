# crol

The library for [CROL](https://github.com/DreamLogics/crol), the Composable Rust
Object Language: configuration and data files that combine with fixed rules, and
load into your types through [serde](https://serde.rs).

## Usage

CROL is not on crates.io yet, so add it from the repository:

```toml
[dependencies]
crol = { git = "https://github.com/DreamLogics/crol" }
serde = { version = "1", features = ["derive"] }
```

Read one document with `from_str`:

```rust
use serde::Deserialize;

#[derive(Deserialize)]
struct Config {
    listen: String,
    workers: u32,
}

fn main() -> Result<(), crol::Error> {
    let config: Config = crol::from_str(r#"
        port := 8080
        listen: "0.0.0.0:" + &port
        workers: 4
    "#)?;
    println!("{} workers on {}", config.workers, config.listen);
    Ok(())
}
```

Or stack files in layers with `CrolComposer`, lowest first:

```rust
use std::path::Path;

use crol::CrolComposer;
use serde::Deserialize;

#[derive(Deserialize)]
struct Config {
    port: u16,
}

fn main() -> Result<(), crol::Error> {
    let config: Config = CrolComposer::new()
        .load(Path::new("data/base"))?
        .load(Path::new("data/local"))?
        .compose()?;
    println!("port {}", config.port);
    Ok(())
}
```

## Features

`crol` has no default features.

| Feature                | Gives                                                        |
| ---------------------- | ------------------------------------------------------------ |
| `derive`               | `#[derive(crol::Schema)]` and `#[derive(crol::VariantName)]` |
| `remote-schemas`       | fetch schemas by URL or crate, with a blocking client        |
| `remote-schemas-async` | the same, without blocking, using Tokio and reqwest          |

## Documentation

- [Getting started](https://github.com/DreamLogics/crol/blob/main/docs/getting-started.md)
- [Loading data in Rust](https://github.com/DreamLogics/crol/blob/main/docs/rust.md)
- [The whole guide](https://github.com/DreamLogics/crol/blob/main/docs/README.md)

## Licence

Licensed under either of Apache License, Version 2.0 or MIT licence at your
option.
