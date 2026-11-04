# Recipes

Short answers to common tasks. Each one assumes you have read
[getting started](getting-started.md).

## An optional local layer

`load` fails if the path does not exist, which is what you want for most layers.
For a layer that may not be there, such as overrides ignored by Git, check first:

```crol
// config.crol
port: 8080,
```

```rust
// src/main.rs
use std::path::Path;

use crol::CrolComposer;
use serde::Deserialize;

#[derive(Deserialize)]
struct Config {
    port: u16,
}

fn main() -> Result<(), crol::Error> {
    let mut composer = CrolComposer::new().load(Path::new("config.crol"))?;
    let local = Path::new("local.crol");
    if local.exists() {
        composer = composer.load(local)?;
    }
    let config: Config = composer.compose()?;
    println!("port {}", config.port);
    Ok(())
}
```

```console
$ cargo run
port 8080
```

## Choosing the environment at run time

Files cannot read the environment, but your program can. Pick the variant in
Rust:

```crol
// config.crol
port: 8080,
<prod> {
    port: 443,
},
```

```rust
// src/main.rs
use std::path::Path;

use crol::{CrolComposer, CrolConfig, CrolVariants};
use serde::Deserialize;

#[derive(Deserialize)]
struct Config {
    port: u16,
}

fn main() -> Result<(), crol::Error> {
    let mut composer = CrolComposer::with_cfg(CrolConfig {
        variants: CrolVariants::new().add_grouped(["dev", "prod"]),
        ..Default::default()
    })
    .load(Path::new("config.crol"))?;
    if let Ok(env) = std::env::var("APP_ENV") {
        composer = composer.with_variant(env)?;
    }
    let config: Config = composer.compose()?;
    println!("port {}", config.port);
    Ok(())
}
```

```console
$ APP_ENV=prod cargo run
port 443
$ APP_ENV=staging cargo run
Error: Error(ErrorImpl { message: "composer: `staging` is not a declared variant", span: None, location: None, path: None, diagnostics: [] })
```

An unknown environment is an error rather than a silent fallback.

## Secrets and other values from outside

Keep secrets out of files, and add them as an in-memory layer on top.
`crol::to_string` takes care of quoting:

```crol
// config.crol
+database: { host: "db.internal", password: "" },
url: "postgres://app:" + &root.database.password + "@" + &root.database.host,
```

```rust
// src/main.rs
use std::path::Path;

use crol::{CrolComposer, CrolLayer};
use serde::Deserialize;

#[derive(Deserialize)]
struct Database {
    host: String,
    password: String,
}

#[derive(Deserialize)]
struct Config {
    database: Database,
    url: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let password = std::env::var("DB_PASSWORD")?;
    let secrets = format!("database.password: {}", crol::to_string(&password)?);

    let config: Config = CrolComposer::new()
        .load(Path::new("config.crol"))?
        .load_layer(CrolLayer::from_sources("secrets", [("secrets.crol", secrets)]))
        .compose()?;

    assert_eq!(config.database.password, password);
    println!("{} via {}", config.database.host, config.url);
    Ok(())
}
```

```console
$ DB_PASSWORD='s3"cret' cargo run
db.internal via postgres://app:s3"cret@db.internal
```

Because `url` uses late references, it picks up the password from the top layer.

## Embedding defaults in the binary

Ship your defaults inside the program, and let files on disk override them:

```crol
// src/defaults.crol
port: 8080,
workers: 4,
```

```crol
// config.crol
workers: 2,
```

```rust
// src/main.rs
use std::path::Path;

use crol::{CrolComposer, CrolLayer};
use serde::Deserialize;

#[derive(Deserialize)]
struct Config {
    port: u16,
    workers: u32,
}

fn main() -> Result<(), crol::Error> {
    let defaults = CrolLayer::from_sources(
        "defaults",
        [("defaults.crol", include_str!("defaults.crol"))],
    );
    let config: Config = CrolComposer::new()
        .load_layer(defaults)
        .load(Path::new("config.crol"))?
        .compose()?;
    println!("{} workers on port {}", config.workers, config.port);
    Ok(())
}
```

```console
$ cargo run
2 workers on port 8080
```

`load_layer` is also the way to test your configuration handling without
touching the file system.

## Checks for CI

These commands all exit with status 1 on failure:

```sh
crol check                          # every layer, in every context
crol fmt --check data/base/*.crol   # formatting
cargo test                          # includes the schema file test
```

`crol check` covers every combination of variants, so a mistake that shows up
only in one environment is found without deploying to it.

To see whether a change affects the data at all, compare `crol fingerprint`
before and after. Reformatting a file or moving an entry between files does not
change the fingerprint, as long as the values and their order stay the same.

## Moving from RON

CROL's values are close to [RON](https://github.com/ron-rs/ron)'s, and
`crol from-ron` converts a file:

```console
$ printf '(name: "web", port: 8080, tags: ["a"], limit: Some(3))' > settings.ron
$ crol from-ron settings.ron
name: "web",
port: 8080,
tags: ["a"],
limit: Some(3),
..,
```

The result goes to standard output. It is a value-mode document, so read it
with `crol::from_str`. The `..` it adds means that fields left out take their
defaults, as RON assumes. To make the file usable in layers, remove the `..`,
and add any fields it was hiding.

## Finding out why a value is what it is

```sh
crol explain data/base data/local web.limits.memory_mb
crol explain --context prod data/base data/local web.replicas
```

In your editor, hover over a key. See [layers](layers.md#seeing-what-happened).

## Next

The [command-line reference](cli.md) lists everything `crol` can do.
