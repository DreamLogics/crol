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
use std::path::Path;

use crol::CrolComposer;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Service {
    replicas: u32,
    port: u16,
    tags: Vec<String>,
    image: String,
    url: String,
}

fn main() -> Result<(), crol::Error> {
    let data = CrolComposer::new()
        .load(Path::new("data/base"))?
        .load(Path::new("data/extension"))?
        .load(Path::new("data/local"))?
        .compose_data()?;
    let web: Service = data.get("web")?;
    println!("{web:?}");
    Ok(())
}
```

```console
$ cargo run
Service { replicas: 2, port: 9090, tags: ["managed", "cached"], image: "web:1.4", url: "http://web:9090" }
```

## What a layer can be

- **A directory**: every `.crod` file below it, recursively. Hidden files and
  directories (starting with `.`) are skipped, and symbolic links are followed.
  A `.crol` file there is [an error](modes.md#when-the-extension-does-not-fit),
  and other files are ignored.
- **One file**.
- **Documents in memory**, for tests and data embedded in your binary:
  `CrolLayer::from_sources("base", [("web.crod", text)])`. These are always data
  mode.
- **A compiled layer**: a [`.crob` file](#compiled-layers).
- **An active layer**: one `.crod` file on top of the others, which
  [your program writes](rust.md#changing-data-with-a-database) through a `CrolDb`.
- **Anything else**, through a [file loader](recipes.md#loading-documents-from-somewhere-else)
  of your own.

Within a layer, documents are read [folder by folder](#the-order-of-files-in-a-layer),
so the order is the same on every platform.

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
the definition comes first, and then the patches, in
[file order](#the-order-of-files-in-a-layer). Layers follow each other in load
order. Then the chain runs from the bottom up.

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

**Patches from different files of one layer need an order.** If only the file
names would decide which applies first, it is an error, since renaming a file
would change the result. The next section shows how to give them one.

## The order of files in a layer

Within a layer, the files of a folder come before the files of its subfolders.
A file deeper down can patch what the files above it set:

```text
units/bomber.crod            the bomber, for everyone
units/empire/bomber.crod     the empire's changes to it
units/rebels/bomber.crod     the rebels' changes to it
```

`units/empire/bomber.crod` and `units/rebels/bomber.crod` are side by side, so
nothing but their names orders them. They may both patch `bomber` as long as
their patches never meet: under `<empire>` and `<rebel>`, with those variants
[in one group](variants.md#declaring-variants), they never apply together. A base patch and a
conditional one never meet either, because the conditional one always comes
after.

When two patches of one key do meet, and only the names of their files order
them, it is an error:

```crol
// shop/web.crod
+web: { port: 80, workers: 2 }
```

```crol
// shop/tuning.crod
web.workers: & * 2
```

```crol
// shop/local.crod
web.workers: & + 1
```

```console
$ crol check shop
error: `web` is patched from `local.crod` and `tuning.crod` of the layer `shop`, and only their names put the patches in order; put one in a folder below the other's, or name the other in its `#![extends(...)]`
  ┌─ /path/to/project/shop/tuning.crod:1:1
  │
1 │ web.workers: & * 2
  │ ^^^^^^^^^^^
  │
  ┌─ /path/to/project/shop/local.crod:1:1
  │
1 │ web.workers: & + 1
  │ ----------- the other patch
```

There are two ways to fix it. Move `local.crod` into a folder, as
`shop/local/workers.crod`, so it comes after the files above it. Or keep it where
it is, and say what it builds on:

```crol
// shop/local.crod
#![extends(tuning)]

web.workers: & + 1
```

Either way `tuning.crod` comes first, and `workers` is 5. An `extends` that
names a file or folder of the same layer orders the layer, and loads nothing:
at the deepest folder that holds both files, the one it names comes first, with
everything below it. A folder still comes after the files beside it, so an
`extends` cannot put a folder first, and `extends` that go round in a circle are
an error.

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
directory it names is loaded as a layer below this one. One in the same layer
[orders that layer](#the-order-of-files-in-a-layer) instead.

`extends` only adds layers below. It is not an import, and it makes nothing
visible that layering would not. `crol` commands and the language server always
follow it. **Your program does not**, unless you set `CrolConfig::load_extends`,
since by default your program decides what is loaded and in which order.

In a [project](projects.md), the project file's layers decide the order, so
`extends` is mainly useful for files outside a project. The language server warns
about one in a layer that names something that is no lower layer.

## Compiled layers

Parsing large data takes time at every start. A layer that does not change between
loads, such as data you generate, can be compiled to a `.crob` file, which loads
several times faster, and takes about half the room of its text:

```console
$ crol compile data/base
$ ls data
base  base.crob  local
$ crol eval data/base.crob data/local
```

In Rust, a path to one is a layer like any other, and `CrolLayer::to_binary` writes
one:

```rust,no_run
// src/main.rs
use std::path::Path;

use crol::CrolComposer;

fn main() -> Result<(), crol::Error> {
    let data = CrolComposer::new()
        .load(Path::new("data/base.crob"))?
        .load(Path::new("data/local"))?
        .compose_data()?;
    println!("{} warnings", data.warnings().len());
    Ok(())
}
```

For a layer whose text may change, `layer.compiled_in(&bytes)` says whether the file
was made from just that text. Read the text, and compile again if it says no:

```rust
// src/main.rs
use std::error::Error;
use std::path::Path;

use crol::{CrolComposer, CrolLayer};

fn main() -> Result<(), Box<dyn Error>> {
    let text = CrolLayer::from_path("base", Path::new("data/base"))?;
    let bytes = std::fs::read("data/base.crob").unwrap_or_default();
    let layer = if text.compiled_in(&bytes) {
        CrolLayer::from_binary("base", &bytes)?
    } else {
        std::fs::write("data/base.crob", text.to_binary()?)?;
        text
    };
    let data = CrolComposer::new().load_layer(layer).compose_data()?;
    println!("{} warnings", data.warnings().len());
    Ok(())
}
```

It evaluates exactly as its documents do, with the same fingerprint, and it keeps
their text, so errors and `crol explain` still show where a value was written. Layers
above it can be plain text, and use its templates.

A few things to keep in mind:

- It does not update itself. Compile it again when its documents change: a `.crob`
  file holds what they said then. A module path prefers the text, if both are there.
- It cannot use a template of another layer, since it is compiled alone.
- It lives outside a layer's directory: inside one, it is an error.
- Its format can change between versions of `crol`. An old file is refused with a
  message, and you compile it again.

## Next

[Variants](variants.md) are the other way of combining data: not by source, but
by environment.
