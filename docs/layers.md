# Layers

Layers are why CROL exists. This page explains what they are, what problem
they solve, and how to use them.

## Why layers

Real configuration comes from more than one place. A product ships defaults, a
team adds an extension, and someone needs a local override on their laptop. The
usual answers are all unsatisfying: copy the file and let the copies drift
apart, write merge code in your program, or use a template engine and debug the
text it produces.

With layers, each source stays in its own files and says only what it adds or
changes. CROL merges them with fixed rules, so the result does not depend on
your program, your platform, or the order the file system lists files in.

A _layer_ is a named, ordered set of documents. You load layers in priority
order, and later layers apply on top of earlier ones:

```text
local        web.port: 9090                  highest priority
extension    +cache: ...   web.replicas: 2
base         +web: ...     +database: ...    lowest priority
```

## A first example

Take a base layer that defines two services, in a directory of its own:

```crol
// data/base/services.crod
service_defaults := {
    replicas: 1,
    port: 8080,
    tags: ["managed"],
}

+web: {
    ..&service_defaults,
    image: "web:1.4",
    url: "http://web:" + &self.port,
}

+database: {
    ..&service_defaults,
    image: "postgres:17",
    port: 5432,
}
```

An extension layer adds a service, and changes one that it did not define.
Note that it can use the template from the layer below:

```crol
// data/extension/cache.crod
+cache: {
    ..&service_defaults,
    image: "valkey:8",
    port: 6379,
}

web.replicas: 2
web.tags: & + ["cached"]
```

And a local layer changes one value:

```crol
// data/local/overrides.crod
web.port: 9090
```

Give `crol eval` the layers, lowest first:

```console
$ crol eval data/base data/extension data/local
web: {
    replicas: 2,
    port: 9090,
    tags: ["managed", "cached"],
    image: "web:1.4",
    url: "http://web:9090",
}
database: { replicas: 1, port: 5432, tags: ["managed"], image: "postgres:17" }
cache: { replicas: 1, port: 6379, tags: ["managed"], image: "valkey:8" }
```

In Rust, each `load` adds a layer on top:

```rust
// src/main.rs
use std::collections::BTreeMap;
use std::path::Path;

use crol::CrolComposer;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Service {
    replicas: u32,
    port: u16,
    tags: Vec<String>,
    image: String,
    url: Option<String>,
}

fn main() -> Result<(), crol::Error> {
    let services: BTreeMap<String, Service> = CrolComposer::new()
        .load(Path::new("data/base"))?
        .load(Path::new("data/extension"))?
        .load(Path::new("data/local"))?
        .compose()?;
    println!("{:?}", services["web"]);
    Ok(())
}
```

## What a layer can be

- **A directory**: every `.crod` file below it, recursively. Hidden files and
  directories (starting with `.`) are skipped, and symbolic links are followed.
  A `.crov` file or a retired `.crol` file there is [an
  error](modes.md#when-the-extension-does-not-fit), and other files are ignored.
- **One file**.
- **Documents in memory**, for tests and data embedded in your binary:
  `CrolLayer::from_sources("base", [("web.crod", text)])`. These are always data
  mode.
- **Anything else**, through a [file loader](recipes.md#loading-documents-from-somewhere-else)
  of your own.

Within a layer, documents are read in order of their path, compared byte by
byte, so the order is the same on every platform.

`CrolComposer::load` takes either kind of path:

- A `&Path` or `PathBuf` is a file system path, relative to the working
  directory.
- A `&str` is a [module path](projects.md#module-paths) such as
  `"::data::base"`, relative to `CrolConfig::root_path`.

## Definitions and patches

Every entry at the top level is one of two things:

- A **definition** gives a key its value without looking at what was there:
  `+web: ...`, `?web: ...`, or `web: ...` where the value does not use `&`.
- A **patch** builds on what is below: a path like `web.port: 80`, a removal
  like `-web`, or a set that uses `&`, like `web: & ~ { ... }`.

For each root key, CROL builds a chain of these operations. Within a layer,
the definition comes first, and then the patches, in file order. Layers follow
each other in load order. Then the chain runs from the bottom up.

Two rules follow from that:

**A layer may define a key once.** Two definitions in one layer are an error,
because which one wins would depend on file names. Different layers can each
define a key: a higher layer's definition simply replaces what was below.

```crol
// twice/one.crod
+web: { port: 80 }
```

```crol
// twice/two.crod
web: { port: 81 }
```

```console
$ crol check twice
error: `web` is defined twice in the layer `twice`; a layer may define a key once
  ┌─ /path/to/project/twice/two.crod:2:1
  │
2 │ web: { port: 81 }
  │ ^^^
  │
  ┌─ /path/to/project/twice/one.crod:2:2
  │
2 │ +web: { port: 80 }
  │  --- the other definition
```

**Patches from different files of one layer get a warning.** They are allowed,
but their order depends on file names, and renaming a file would change the
result. Move them into one file, or into separate layers. If you know what you
are doing, turn the warning off with `CrolConfig::warn_cross_file_patches`.

## Early and late references

This is the one idea to take away from this page. There are two kinds of
reference, and they read at different moments:

- `&` is **early**. It is what the chain has produced so far: the value below
  this entry.
- Everything else (`&self`, `&parent`, `&root`, binds and templates) is
  **late**. It reads the **final** value, after all layers.

So in the example above, `web.url` is `"http://web:" + &self.port`, written in
`base`. The late reference reads the final `port`, which is 9090 because the
`local` layer changed it. The base layer does not need to know about `local`.

`&` is for changes relative to what is below:

```crol
// demo/a.crod
+memory: 512
+doubled: &root.memory * 2
```

```crol
// demo/b.crod
memory: & + 512
```

```console
$ crol eval demo/a.crod demo/b.crod
memory: 1024
doubled: 2048
```

Each `&` sees the chain up to its own entry, while `&root.memory` saw the
outcome of all of them.

A reference that ends up needing its own result is a cycle, and an error
listing every step. Since values are computed only when needed, fields of one
value can freely refer to each other, as `url` and `port` do.

## Seeing what happened

With many layers, you will want to know why a value is what it is.
`crol explain` lists the operations behind a value, with the result after
each:

```console
$ crol explain data/base data/extension data/local web.port
web.port:
  data/base/services.crod:8:2    +web      definition  8080
  data/local/overrides.crod:2:1  web.port  patch       9090
```

The same list is available in Rust through
[`CrolData::explain`](rust.md#inspecting-the-result), and your editor shows it
on hover.

## Declaring what a file builds on

A file that patches `web` makes no sense without the file that defines it. To
say so, name that file with `#![extends(...)]`:

```crol
// demo2/defaults/services.crod
+web: { replicas: 1 }
```

```crol
// demo2/app.crod
#![extends(defaults::services)]

web.replicas: 4
```

```console
$ crol eval demo2/app.crod
web: { replicas: 4 }
```

The path is a [module path](projects.md#module-paths), and the file or
directory it names is loaded as a layer below this one.

`extends` only adds layers below. It is not an import, and it makes nothing
visible that layering would not. `crol` commands and the language server always
follow it. **Your program does not**, unless you set `CrolConfig::load_extends`,
since by default your program decides what is loaded and in which order.

In a [project](projects.md), the project file's layers decide the order, so
`extends` is mainly useful for files outside a project. The language server warns
about one in a layer that names something that is no lower layer.

## Next

[Variants](variants.md) are the other way of combining data: not by source, but
by environment.
