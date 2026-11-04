# Recipes

Short answers to common tasks. Each one assumes you have read
[getting started](getting-started.md).

## An optional local layer

`load` fails if the path does not exist, which is what you want for most layers.
For a layer that may not be there, such as overrides ignored by Git, check first:

```crol
// config.crod
port: 8080
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
    let mut composer = CrolComposer::new().load(Path::new("config.crod"))?;
    let local = Path::new("local.crod");
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
// config.crod
port: 8080
<prod> {
    port: 443,
}
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
    .load(Path::new("config.crod"))?;
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
Error: Error(ErrorImpl { message: "composer: `staging` is not a declared variant", span: None, location: None, path: None, diagnostics: [], source_id: None })
```

An unknown environment is an error rather than a silent fallback.

## Secrets and other values from outside

Keep secrets out of files, and add them as an in-memory layer on top.
`crol::to_string` takes care of quoting:

```crol
// config.crod
+database: { host: "db.internal", password: "" }
url: "postgres://app:" + &root.database.password + "@" + &root.database.host
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
        .load(Path::new("config.crod"))?
        .load_layer(CrolLayer::from_sources("secrets", [("secrets.crod", secrets)]))
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
// src/defaults.crod
port: 8080
workers: 4
```

```crol
// config.crod
workers: 2
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
        [("defaults.crod", include_str!("defaults.crod"))],
    );
    let config: Config = CrolComposer::new()
        .load_layer(defaults)
        .load(Path::new("config.crod"))?
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

## Loading documents from somewhere else

By default, module paths name files. A _file loader_ lets them name anything: data
embedded in your binary, a database, a server. Everything your program reads by
module path goes through the loaders, the layers of `load` and the documents of
`#![extends(...)]` alike.

A loader answers two questions. `resolve` says which source a module path names,
or `None` if it has nothing by that name. `load` returns what a source holds. This
one keeps documents in a map:

```rust
// src/main.rs
use std::collections::BTreeMap;

use crol::{CrolComposer, FileLoader, Loaded, LoaderError, ModulePath, SourceId};
use serde::Deserialize;

struct Memory {
    files: BTreeMap<String, String>,
}

impl FileLoader for Memory {
    fn resolve(&self, path: &ModulePath<'_>) -> Result<Option<SourceId>, LoaderError> {
        let name = format!("{}.crod", path.segments().join("/"));
        let found = self.files.contains_key(&name);
        Ok(found.then(|| SourceId::new(format!("memory:{name}"))))
    }

    fn load(&self, source: &SourceId) -> Result<Loaded, LoaderError> {
        let name = source.as_name().and_then(|n| n.strip_prefix("memory:"));
        let text = name.and_then(|n| self.files.get(n)).ok_or("no such document")?;
        Ok(Loaded::Document(text.clone().into_bytes()))
    }
}

#[derive(Deserialize)]
struct Config {
    port: u16,
}

fn main() -> Result<(), crol::Error> {
    let files = BTreeMap::from([("data/base.crod".to_owned(), "port: 8080".to_owned())]);
    let config: Config = CrolComposer::new()
        .with_file_loader(Memory { files })
        .load("::data::base")?
        .compose()?;
    println!("port {}", config.port);
    Ok(())
}
```

```console
$ cargo run
port 8080
```

`path.segments()` gives the names from the root to what the path names, with
`super` and the document's directory already applied, so `super::defaults` in
`data/base/web.crod` is `["data", "defaults"]`. A loader cannot be led above the
root. A module path has no extension: if your loader also serves schema files,
`path.mode().extension()` gives `crod` or `crov`, whichever is looked for.

The source's name appears in messages, so start it with something of your own,
like `memory:` here. Its extension [counts as a file's
does](modes.md#text-has-no-extension): a layer called `memory:base.crov` is an
error, and one called `memory:base` follows the call that loaded it. To hold a directory of documents, return `Loaded::Directory`
with the relative path and source of each: CROL orders them by path itself.
`crol::Error::source_id()` tells you which source an error from loading is about.

Loaders are chained by order: those you register, first one first, and then the
file system. The first to resolve a path has it, a loader that returns `None`
falls through to the next, and one that fails stops the search. `FsLoader::new(dir)`
adds another directory tree to the chain. A loader must give the same text for the
same source, or your data stops being reproducible.

Loaders work in your program only. The `crol` tool and language server read the
file system.

### Documents you do not trust

**A path a loader does not have falls through to the file system, also when it is
written in a document that loader loaded.** So with `load_extends` on, a document
from a custom loader can pull in local files below `root_path`:

```crol
// local_secrets.crod
port: 1
```

```rust
// src/main.rs
use std::collections::BTreeMap;

use crol::{CrolComposer, CrolConfig, FileLoader, Loaded, LoaderError, ModulePath, SourceId};
use serde::Deserialize;

struct Memory {
    files: BTreeMap<String, String>,
}

impl FileLoader for Memory {
    fn resolve(&self, path: &ModulePath<'_>) -> Result<Option<SourceId>, LoaderError> {
        let name = format!("{}.crod", path.segments().join("/"));
        let found = self.files.contains_key(&name);
        Ok(found.then(|| SourceId::new(format!("memory:{name}"))))
    }

    fn load(&self, source: &SourceId) -> Result<Loaded, LoaderError> {
        let name = source.as_name().and_then(|n| n.strip_prefix("memory:"));
        let text = name.and_then(|n| self.files.get(n)).ok_or("no such document")?;
        Ok(Loaded::Document(text.clone().into_bytes()))
    }
}

#[derive(Deserialize)]
struct Config {
    port: u16,
}

fn composer() -> CrolComposer {
    let text = "#![extends(::local_secrets)]\n\nport: & + 1";
    let files = BTreeMap::from([("remote.crod".to_owned(), text.to_owned())]);
    CrolComposer::with_cfg(CrolConfig {
        load_extends: true,
        ..Default::default()
    })
    .with_file_loader(Memory { files })
}

fn main() {
    let open: Result<Config, _> = composer().load("::remote").and_then(|c| c.compose());
    println!("port {}", open.unwrap().port);

    let closed = composer().without_file_system().load("::remote");
    println!("{}", closed.unwrap_err());
}
```

```console
$ cargo run
port 2
the module path `::local_secrets` names nothing: no file loader has it
```

For documents that are not trusted with your files, call `without_file_system()`.
It takes the file system out of the chain, and also refuses a `&Path` in `load`.

## Checks for CI

These commands all exit with status 1 on failure:

```sh
crol check                          # every layer, in every context
crol fmt --check data/base/*.crod   # formatting
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
name: "web"
port: 8080
tags: ["a"]
limit: Some(3)
..
```

The result goes to standard output. It is a value document, so keep it in a
`.crov` file and read it with `crol::from_str`:

```console
$ crol from-ron settings.ron > settings.crov
$ crol check settings.crov
```

The `..` it adds means that fields left out take their defaults, as RON assumes.
To make the file usable in layers, remove the `..`, add any fields it was hiding,
and name it `.crod`.

## Finding out why a value is what it is

```sh
crol explain data/base data/local web.limits.memory_mb
crol explain --context prod data/base data/local web.replicas
```

In your editor, hover over a key. See [layers](layers.md#seeing-what-happened).

## Next

The [command-line reference](cli.md) lists everything `crol` can do.
