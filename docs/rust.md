# Loading data in Rust

The [`crol`](../crates/crol) crate is a serde `Deserializer` with an evaluator
in front of it. This page covers how CROL values map onto your types, and how
to use what you get back.

## Two ways in

| You have                              | Use                                                 |
| ------------------------------------- | --------------------------------------------------- |
| one document (`.crol`)                | [`crol::from_str`](modes.md#value-mode-a-file-is-a-value) |
| one document and a [schema](schemas.md) | `crol::from_str_with_schema`                      |
| layers, variants or several files (`.crod`) | `CrolComposer`, then `compose` or `compose_data` |
| layers your program changes and saves | [`CrolComposer`, then `compose_db`](#changing-data-with-a-database) |

To read a few fields of many values rather than whole values, see
[views and queries](#views-and-queries).

[Data mode and value mode](modes.md) explains the difference. Everything below
about types applies to both.

## How values map onto types

Evaluated values go through serde, so your `Deserialize` types work as they do
with any format, with a few CROL rules on top.

| Rust type                      | Accepts                                                |
| ------------------------------ | ------------------------------------------------------ |
| `bool`, `char`                 | a boolean, a char                                      |
| integers                       | an integer that fits; `-1` for a `u16` is an error     |
| `f32`, `f64`                   | a float or an integer                                  |
| `String`                       | a string, or an id's text                              |
| `Option<T>`                    | `None`, `Some(x)`, or a plain `x`                      |
| `()`                           | `()`                                                   |
| `Vec<T>`, sets                 | a list                                                 |
| tuples, arrays                 | a tuple                                                |
| maps                           | a keyed value; keys are strings, integers, unit enum variants or `crol::Id` |
| structs                        | a keyed value, with the checks below                   |
| `struct N(T)`                  | `N(x)` or `x`                                          |
| `struct P(A, B)`               | `P(a, b)` or `(a, b)`                                  |
| enums                          | `V`, `V(x)`, `V(a, b)`, `V { ... }`                    |

### Structs

A struct is checked more strictly than in most formats:

- **Every field must be written**, `Option` fields included, unless the block
  ends with `..`. With `..`, serde fills in what is left out:
  [Leaving fields out](language.md#leaving-fields-out) has the details.
- **Unknown fields are errors**, so a misspelt field never goes unnoticed.
- **The type name, if written, must match**: `Limits { ... }` cannot load into
  a `Service`. The name is the Rust name, after any `#[serde(rename)]`.

Only the final value is checked, so a patch can leave out whatever it does not
change.

`#[serde(flatten)]` makes serde ask for a map rather than a struct. It works,
but these checks then do not apply to that struct.

### Types that decide for themselves

Whether fields can be left out without `..` is normally one setting for all your
data, `require_complete`. Some types know better. A retry policy with sensible
defaults should not need `..` at every use, and a critical type may want `..`
even where you turned the check off. With the `derive` feature, a type can decide
with `#[crol(explicit_defaults)]`:

| On the type                          | Fields left out without `..`               |
| ------------------------------------ | ------------------------------------------ |
| nothing                              | as `require_complete` says: an error by default |
| `#[crol(explicit_defaults)]`         | always an error                            |
| `#[crol(explicit_defaults = false)]` | take their defaults, as with `..`          |

```rust
// src/main.rs
use serde::Deserialize;

#[derive(Deserialize, crol::Schema)]
#[crol(explicit_defaults = false)]
struct Retry {
    limit: u32,
    #[serde(default)]
    backoff_ms: u32,
    jitter: Option<f64>,
}

fn main() -> Result<(), crol::Error> {
    let schema = crol::schema::build::<Retry>();
    let retry: Retry = crol::from_str_with_schema("Retry { limit: 3 }", &schema)?;
    println!("{} {} {:?}", retry.limit, retry.backoff_ms, retry.jitter);

    // Without the schema, the deserializer cannot know.
    if let Err(error) = crol::from_str::<Retry>("Retry { limit: 3 }") {
        println!("{error}");
    }
    Ok(())
}
```

```console
$ cargo run
3 0 None
`Retry` is missing the fields `backoff_ms`, `jitter`; write them, or end the block with `..` to leave out fields that have defaults (<string>:1:1)
```

**The loader needs the schema**, because serde does not pass attributes to a
deserializer. Use `crol::from_str_with_schema` for one document, or
`CrolComposer::schema` for layers. Without one, only `require_complete` counts:
loading then fails, rather than load something the tools did not check.

`explicit_defaults = false` is about the type, not the data. It changes no value
and no fingerprint, a field without a default is still an error, and `..` remains
allowed. It does not apply to the types of the fields, which have settings of
their own.

The attribute goes on a struct with named fields, a struct variant, or an enum,
for its struct variants that have none of their own. Elsewhere it is a compile
error, as it is on variants of tagged and untagged enums, and
`#[crol(explicit_defaults)]` on a struct with `#[serde(flatten)]`, because those
do not read through a path where `..` can be asked for.

For a type that needs `..`, one `..` that reaches the final value is enough, in
any layer or template, such as a later `web: & ~ { .. }`.

### Enums

External tagging, serde's default, fits CROL best, and lets you load a root of
mixed types into one map. Each type name selects the enum variant of that name:

```crol
// fleet.crod
+web: Service { port: 8080, image: "web:1.4" }
+uploads: Volume { size_gb: 20 }
```

```rust
// src/main.rs
use std::collections::BTreeMap;
use std::path::Path;

use crol::CrolComposer;
use serde::Deserialize;

#[derive(Deserialize)]
struct Service {
    port: u16,
    image: String,
}

#[derive(Deserialize)]
struct Volume {
    size_gb: u32,
}

#[derive(Deserialize)]
enum Definition {
    Service(Service),
    Volume(Volume),
}

fn main() -> Result<(), crol::Error> {
    let fleet: BTreeMap<String, Definition> =
        CrolComposer::new().load(Path::new("fleet.crod"))?.compose()?;
    for (name, definition) in &fleet {
        match definition {
            Definition::Service(s) => println!("{name}: {} on port {}", s.image, s.port),
            Definition::Volume(v) => println!("{name}: {} GB", v.size_gb),
        }
    }
    Ok(())
}
```

```console
$ cargo run
uploads: 20 GB
web: web:1.4 on port 8080
```

This is the shape for layered data that defines many things of several kinds: an
enum of everything the data can define, in a map. Build the
[schema](schemas.md) from the enum. Use `indexmap::IndexMap` if you want to keep
the order of the data. For one configuration with known fields, use
[a struct at the root](#a-struct-at-the-root).

Internally tagged, adjacently tagged and untagged enums work, but serde reads
them without type names, so they behave as they would with
`serde_json::Value`.

### A struct at the root

The plain way to use layers for a configuration is a struct at the root: the
struct describes the whole document, and every root key is one of its fields,
as in [getting started](getting-started.md). The layers together make the
struct, so one layer can set a field another leaves out, and only the result has
to be complete.

Like any block, the root can end with `..` to let fields take their defaults. One
`..` in any layer is enough:

```rust
// src/main.rs
use crol::{CrolComposer, CrolLayer};
use serde::Deserialize;

#[derive(Deserialize)]
struct Config {
    port: u16,
    workers: Option<u32>,
    #[serde(default)]
    tags: Vec<String>,
}

fn main() -> Result<(), crol::Error> {
    let config: Config = CrolComposer::new()
        .load_layer(CrolLayer::from_sources("base", [("a.crod", "port: 80\n..")]))
        .load_layer(CrolLayer::from_sources("local", [("a.crod", "port: & + 8000")]))
        .compose()?;
    println!("port {}, workers {:?}, tags {:?}", config.port, config.workers, config.tags);
    Ok(())
}
```

```console
$ cargo run
port 8080, workers None, tags []
```

Everything else works as in a block, too:

- `-tags` in a layer removes the field, and it takes its default, without `..`. A
  field with no default is then `missing field`, reported for the result, not
  for the layer that removed it.
- `<prod> { .. }` leaves fields out in `prod` only.
- A key the struct does not have is an error that lists its fields, unless you
  set `allow_unknown_fields`.
- `t := { ... }` at the root is a template, not a field.

If the fields of the struct should take their defaults without anyone writing
`..`, give it `#[crol(explicit_defaults = false)]`, and give the composer its schema:

```rust
// src/main.rs
use crol::{CrolComposer, CrolLayer};
use serde::Deserialize;

#[derive(Deserialize, crol::Schema)]
#[crol(explicit_defaults = false)]
struct Config {
    port: u16,
    workers: Option<u32>,
}

fn main() -> Result<(), crol::Error> {
    let config: Config = CrolComposer::new()
        .schema(crol::schema::build::<Config>())
        .load_layer(CrolLayer::from_sources("base", [("a.crod", "port: 80")]))
        .compose()?;
    println!("port {}, workers {:?}", config.port, config.workers);
    Ok(())
}
```

```console
$ cargo run
port 80, workers None
```

With a [schema](schemas.md) built from the struct, `crol check` and your editor see
the same thing: root keys are its fields, and completeness is that of all the
layers together.

## Ids

An [id](language.md#ids) names a root key. Use `crol::Id` for a field that must
hold one: unlike `String`, it refuses a plain string. CROL does not check what
ids point at, because only you know what is valid. It tells you which ids it
saw, so you can check them:

```rust
// src/main.rs
use crol::{CrolComposer, CrolLayer, Id, IdUse};
use serde::Deserialize;

#[derive(Deserialize)]
struct Service {
    depends_on: Vec<Id>,
}

fn main() -> Result<(), crol::Error> {
    let layer = CrolLayer::from_sources(
        "base",
        [("a.crod", "+web: { depends_on: [@db, @cache] }, +db: { depends_on: [] }")],
    );
    let data = CrolComposer::new().load_layer(layer).compose_data()?;

    let (web, ids): (Service, Vec<IdUse>) = data.get_with_ids("web")?;
    println!("web has {} dependencies", web.depends_on.len());
    for id in ids {
        if !data.contains(&id.id) {
            println!("{}: nothing is called `{}`", id.rust_path, id.id);
        }
    }
    Ok(())
}
```

```console
$ cargo run
web has 2 dependencies
web.depends_on[1]: nothing is called `cache`
```

With a [schema](schemas.md), `#[crol(refers(...))]` and `crol::check` do this
for you, including the type of what an id names.

## Configuration

`CrolComposer::new()` uses the defaults. To change them, pass a `CrolConfig`
to `CrolComposer::with_cfg`:

```rust
// src/main.rs
use crol::{CrolComposer, CrolConfig, CrolLayer, VariantKeys};
use serde::Deserialize;

#[derive(Deserialize)]
struct Config {
    port: u16,
    workers: Option<u32>,
}

fn main() -> Result<(), crol::Error> {
    let config: Config = CrolComposer::with_cfg(CrolConfig::<String> {
        require_complete: false,
        variant_keys: VariantKeys::SameKeys,
        ..Default::default()
    })
    .load_layer(CrolLayer::from_sources("base", [("a.crod", "port: 80")]))
    .compose()?;
    println!("port {}, workers {:?}", config.port, config.workers);
    Ok(())
}
```

```console
$ cargo run
port 80, workers None
```

`CrolConfig::<String>` says that variants are named by strings. Rust cannot work
that out when you do not set `variants`.

| Field                     | Default            | Meaning                                                      |
| ------------------------- | ------------------ | ------------------------------------------------------------ |
| `variants`                | none declared      | the [variants](variants.md#declaring-variants)               |
| `root_path`               | working directory  | where `::` [module paths](projects.md#module-paths) start    |
| `file_loaders`            | the file system    | where [documents come from](recipes.md#loading-documents-from-somewhere-else) |
| `load_extends`            | `false`            | follow [`#![extends(...)]`](layers.md#declaring-what-a-file-builds-on) |
| `contexts`                | `None`: all        | the contexts `compose_data` evaluates                        |
| `each_variant`            | `false`            | with no `contexts`, the base and [one context for each variant](variants.md#using-more-than-one-context) |
| `max_contexts`            | `4096`             | the most contexts evaluated when `contexts` is `None`        |
| `threads`                 | `None`: decided    | how many threads evaluate the contexts; `Some(1)` uses none. The result is the same, and more threads take more memory |
| `variant_keys`            | `Any`              | [keys per context](variants.md#keys-that-exist-only-in-some-contexts) |
| `implicit_some`           | `true`             | accept `x` for an `Option`, as `Some(x)`                     |
| `allow_unknown_fields`    | `false`            | ignore fields a struct does not have                         |
| `require_complete`        | `true`             | require every field of a struct without `..`, unless the [type decides](#types-that-decide-for-themselves) |
| `warn_cross_file_patches` | `true`             | warn about [patches from several files](layers.md#definitions-and-patches) of a layer |
| `keep_lowered`            | `true`             | keep what `.crob` layers hold for `reload`; off takes less memory, and `reload` parses them |
| `init_defaults`           | `false`            | let a path go through a field left out, from its default; needs a [schema](schemas.md#using-default) |

## Inspecting the result

`compose` gives you your type and nothing else. `compose_data` evaluates every
context and returns a `CrolData`, which you can ask more of:

```rust
// src/main.rs
use crol::{CrolComposer, CrolLayer};

fn main() -> Result<(), crol::Error> {
    let base = CrolLayer::from_sources("base", [("web.crod", "+web: { port: 80 }")]);
    let local = CrolLayer::from_sources("local", [("web.crod", "web.port: & + 1")]);
    let data = CrolComposer::new()
        .load_layer(base)
        .load_layer(local)
        .compose_data()?;

    let port: u16 = data
        .get::<std::collections::BTreeMap<String, u16>>("web")?["port"];
    println!("port {port}");
    println!("keys {:?}", data.keys());
    println!("warnings {:?}", data.warnings());

    // Why is it 81?
    let context = data.context(Vec::<String>::new())?;
    for step in data.explain("web.port", &context)? {
        println!("{} in layer {}", step.entry, step.layer);
    }

    // Two data sets are the same if and only if this is.
    let hash: String = data.fingerprint().iter().map(|b| format!("{b:02x}")).collect();
    println!("{hash}");
    Ok(())
}
```

```console
$ cargo run
port 81
keys ["web"]
warnings []
+web in layer base
web.port in layer local
c7d4854de35b6a1a5b67d3512790a4783e1f17737b07cdba636ef9e3dfa91503
```

The most useful methods:

| Method                             | Returns                                            |
| ---------------------------------- | -------------------------------------------------- |
| `get(key)`, `get_in(key, &context)` | a root value, deserialized                        |
| `get_with_ids(key)`                | a root value and the ids in it                     |
| `deserialize_root(context)`        | the whole root, deserialized                       |
| `context(variants)`, `contexts()`  | one context, or every evaluated context            |
| `evaluate(variants)`               | evaluates a context now, without reading the files |
| `keys()`, `contains(id)`, `type_name(id)` | what is in the data                         |
| `value(key, &context)`             | the raw evaluated `crol::Value`                    |
| `warnings()`                       | warnings, which `compose` drops                    |
| `explain(path, &context)`          | the operations behind a value                      |
| `distinct(key)`, `variations(key)` | a key's [distinct values](#views-and-queries) across contexts |
| `query()`                          | [keys chosen](#views-and-queries) by what they hold |
| `fingerprint()`                    | a BLAKE3 hash of all evaluated data                |
| `key_fingerprints()`               | one for each root key; `crol::changed_keys` compares two |
| `reload(changes)`                  | [applies changes to documents](#reloading)         |

The fingerprint covers values, not source text. Two data sets written
differently but with the same values have the same fingerprint, on every
platform. Use it to check that two machines loaded the same data, or to see
whether a change really changed anything.

## Reloading

A program that watches its files can give `reload` what changed. Unchanged documents
are not read again, and data that does not load leaves everything as it was, so a
mistake in a file does not stop the program:

```rust
// src/main.rs
use crol::{CrolComposer, CrolLayer, DocumentChange};

fn main() -> Result<(), crol::Error> {
    let layer = CrolLayer::from_sources("base", [("a.crod", "+web: { port: 80 }\n+db: { port: 1 }")]);
    let mut data = CrolComposer::new().load_layer(layer).compose_data()?;

    let reloaded = data.reload(&[DocumentChange {
        layer: "base",
        path: "a.crod",
        text: Some("+web: { port: 81 }\n+db: { port: 1 }"),
    }])?;
    println!("{:?}", reloaded.changed);

    let broken = DocumentChange { layer: "base", path: "a.crod", text: Some("web: ") };
    println!("{}", data.reload(&[broken]).is_err());
    Ok(())
}
```

```console
$ cargo run
[Changed("web")]
true
```

Keep `CrolContext`s from before a reload only if your variants are declared. Everything
is evaluated again, so a reload takes about as long as loading.

## Views and queries

Deserializing gives you a whole value in one context, in a type that mirrors it.
That is often more than you want. A program may need two fields of every unit, or
want to know how one value differs between contexts, or which keys hold something.

| You want                                       | Use                                     |
| ---------------------------------------------- | --------------------------------------- |
| a whole value, as your own type                | `get`, or `compose`                     |
| a few fields, from anywhere in a value         | [a view](#views)                        |
| how a value differs between contexts           | [`variations`](#across-contexts)        |
| the keys that match something                  | [`query`](#queries)                     |
| one known place, typed, or to change the data  | [a database](#changing-data-with-a-database), which reads views and queries too |

All of them read evaluated data, so they see every layer and every patch. These
examples read this file:

```crol
// units.crod
+artillery: Unit {
    hp: 1100,
    combat: { range: 12, reload: 8 },
    tags: ["mech", "heavy"],
}
+trooper: Unit { hp: 50, combat: { range: 5, reload: 1 }, tags: [] }
+tree: Scenery { hp: 10 }

artillery<empire>.hp: 1200
```

### Views

A _view_ is a small struct whose fields say what to read. Unlike a `Deserialize` type,
it does not mirror the value: it leaves out what it does not need, and can lift a
field from deep inside. Views need the `derive` feature.

```rust
// src/main.rs
use std::path::Path;

use crol::{CrolComposer, View};

#[derive(Debug, View)]
struct Stats {
    hp: u32,
    #[crol(path = "combat.range")]
    range: u32,
    armour: Option<u32>,
    #[crol(default)]
    tags: Vec<String>,
}

fn main() -> Result<(), crol::Error> {
    let data = CrolComposer::new().load(Path::new("units.crod"))?.compose_data()?;
    let units = data.query().of_type("Unit").views::<Stats>()?;
    for (key, stats) in units {
        println!("{key}: {stats:?}");
    }
    Ok(())
}
```

```console
$ cargo run
artillery: Stats { hp: 1100, range: 12, armour: None, tags: ["mech", "heavy"] }
trooper: Stats { hp: 50, range: 5, armour: None, tags: [] }
```

A field reads the value's field of its own name, or the path in
`#[crol(path = "...")]`. The name is the one serde gives it, if the struct also derives
`Deserialize` or `Serialize` with `#[serde(rename)]` or `rename_all`. A path is field
names with dots, and it goes through `Some(...)`, but not into a list. A field can be
any type that deserializes.

Nothing has to be there. Where a path is missing, the first of these that applies
decides:

1. The default your [schema](schemas.md) records for it, if the value has a type name
   and the composer has the schema.
2. An `Option` is `None`.
3. `#[crol(default)]` takes the field type's `Default`.
4. Anything else is an error: `` `tree` has no `combat.range`, and no default for it ``.

That is why `of_type` comes first in the example: a `tree` has no range, and no
`Stats`.

A view is a trait, so you can write one for what a derive cannot say. `key` gives the
root key:

```rust
// src/main.rs
use std::path::Path;

use crol::{CrolComposer, View, ViewSource};

#[derive(Debug)]
struct Label(String);

impl View for Label {
    fn read(source: &ViewSource<'_>) -> Result<Self, crol::Error> {
        let hp: u32 = source.field(&["hp"])?;
        let range: Option<u32> = source.optional(&["combat", "range"])?;
        Ok(Label(format!("{}: {hp} hp, range {range:?}", source.key())))
    }
}

fn main() -> Result<(), crol::Error> {
    let data = CrolComposer::new().load(Path::new("units.crod"))?.compose_data()?;
    for (_, label) in data.query().views::<Label>()? {
        println!("{}", label.0);
    }
    Ok(())
}
```

```console
$ cargo run
artillery: 1100 hp, range Some(12)
tree: 10 hp, range None
trooper: 50 hp, range Some(5)
```

### Across contexts

`variations` gives each distinct value of a key, read into a view, with the contexts
that have it. Values are compared whole, before the view reads them, so two variations
can look the same through a view that leaves out the field that differs.

```rust
// src/main.rs
use std::path::Path;

use crol::{CrolComposer, View};

#[derive(Debug, View)]
struct Health {
    hp: u32,
}

fn main() -> Result<(), crol::Error> {
    let data = CrolComposer::new().load(Path::new("units.crod"))?.compose_data()?;
    for variation in data.variations::<Health>("artillery")? {
        let contexts: Vec<_> = variation.contexts.iter().map(|c| data.context_names(c)).collect();
        println!("{:?}: {:?}", contexts, variation.value);
    }
    Ok(())
}
```

```console
$ cargo run
[[]]: Health { hp: 1100 }
[["empire"]]: Health { hp: 1200 }
```

`distinct` gives the same variations as raw values, without a view. Only
[evaluated contexts](variants.md) count, and the order does not depend on the order
they were evaluated in.

### Queries

`query` chooses root keys in one context, the base unless `in_context` names another.

| On a query                  | Does                                                  |
| --------------------------- | ----------------------------------------------------- |
| `of_type(name)`             | keeps values of that type name                        |
| `holding(path, value)`      | keeps values with that value at a field path, or a list holding it; an id counts as its text |
| `holding_at(path, value)`   | the same, at a [typed path](#paths)                   |
| `in_context(&context)`      | reads another context                                 |
| `keys()`                    | the keys, compared byte by byte                       |
| `views::<V>()`              | each key and its view                                 |

```rust
// src/main.rs
use std::path::Path;

use crol::CrolComposer;

fn main() -> Result<(), crol::Error> {
    let data = CrolComposer::new().load(Path::new("units.crod"))?.compose_data()?;
    let mechs = data.query().of_type("Unit").holding("tags", "mech");
    println!("{:?}", mechs.keys()?);
    let short = data.query().holding("combat.range", &5);
    println!("{:?}", short.keys()?);
    let empire = data.context(["empire"])?;
    let strong = data.query().in_context(&empire).holding("hp", &1200);
    println!("{:?}", strong.keys()?);
    Ok(())
}
```

```console
$ cargo run
["artillery"]
["trooper"]
["artillery"]
```

A query only reads evaluated data. A patch cannot choose keys by what they hold, since
what it selects would then depend on what patches do.

## Errors

Everything fails with `crol::Error`. Its `Display` says what went wrong and
where, including the file, line and column:

```rust
// src/main.rs
use serde::Deserialize;

#[derive(Deserialize)]
struct Config {
    port: u16,
}

fn load(text: &str) -> Result<u16, crol::Error> {
    crol::from_str::<Config>(text).map(|config| config.port)
}

fn main() {
    let error = load("port: -1").unwrap_err();
    println!("{error}");
    println!("message: {}", error.message());
    println!("path: {:?}", error.path());

    let error = load("port: 1 + \"a\" + [], other: 1 / 0").unwrap_err();
    for diagnostic in error.diagnostics() {
        println!("{diagnostic}");
    }
}
```

```console
$ cargo run
expected u16, found -1 (<string>:1:7, while deserializing port)
message: expected u16, found -1
path: Some("port")
<string>:1:7: +: cannot add a list to a string (while evaluating port)
<string>:1:28: /: division by zero (while evaluating other)
```

Evaluation does not stop at the first problem. An error from reading or
evaluating carries every problem found, in `diagnostics()`. Deserializing stops
at the first error, as serde does. To check everything against your types
without stopping, use [`crol::check`](schemas.md#checking-from-rust).

## Writing CROL

`crol::to_string` and `crol::to_string_pretty` write any `Serialize` value as a
plain value document, for a `.crol` file. The output has no operators, and reads back to the
same value:

```rust
// src/main.rs
use crol::PrettyConfig;
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Deserialize, Serialize)]
struct Config {
    port: u16,
    name: Option<String>,
    tags: Vec<String>,
}

fn main() -> Result<(), crol::Error> {
    let config = Config {
        port: 8080,
        name: Some("web".to_owned()),
        tags: vec!["a".to_owned()],
    };
    println!("{}", crol::to_string(&config)?);

    let options = PrettyConfig::default()
        .with_max_width(30)
        .with_struct_names(false)
        .with_implicit_some(true);
    let text = crol::to_string_pretty(&config, options)?;
    print!("{text}");

    assert_eq!(crol::from_str::<Config>(&text)?, config);
    Ok(())
}
```

```console
$ cargo run
Config {port: 8080, name: Some("web"), tags: ["a"]}
port: 8080
name: "web"
tags: ["a"]
```

CROL does not edit files for you: there is no way to change a value and keep
the file's layout and comments. To change data from a program, write a new
layer on top instead, with `crol::DataWriter`:

```rust
// src/main.rs
use crol::DataWriter;
use serde::Serialize;

#[derive(Serialize)]
struct Service {
    port: u16,
    replicas: u32,
}

fn main() -> Result<(), crol::Error> {
    let web = Service { port: 80, replicas: 2 };
    let scaled = Service { port: 80, replicas: 8 };

    let mut writer = DataWriter::new();
    writer.comment("Generated; do not edit.");
    writer.declare("web", &web)?;
    writer.variant("prod", |w| w.diff("web", &web, &scaled))?;
    print!("{}", writer.finish()?);
    Ok(())
}
```

```console
$ cargo run
// Generated; do not edit.
+web: Service { port: 80, replicas: 2 }
<prod> {
    web: & ~ Service { replicas: 8 },
}
```

`diff` writes only what differs, at any depth. A struct that leaves fields out with
`skip_serializing_if` gets `..`, so the output reads back.

To write a reference, or any other expression, where a value goes, use
`crol::Expression`, as a field of what you serialize:
`writer.set("weapon", &crol::Expression::new("&root.rifle")?)` writes
`weapon: &root.rifle`. Where the type has no place for one, change the value:
`crol::to_value` it, put `expression.to_value()` where the reference goes, and write it
as a `crol::RawValue`.

## Changing data with a database

`compose_db` gives a `CrolDb`: typed reads, and changes that are written as
operations in one layer of its own, the _active layer_. The layers below stay as
they are, and references follow a change like any other. It needs the `derive`
feature.

```crol
// data/base/config.crod
+retries: 1
+options: { dark: false, port: 80 }
<prod> { retries: & * 10 }
```

```rust
// src/main.rs
use std::path::Path;

use crol::{CrolComposer, CrolDb, Navigable, croxp};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Navigable)]
struct Options {
    dark: bool,
    port: u16,
}

#[derive(Deserialize, Serialize, Navigable)]
struct Config {
    retries: u32,
    options: Options,
}

fn main() -> Result<(), crol::Error> {
    let mut db: CrolDb<Config> = CrolComposer::new()
        .load(Path::new("data/base"))?
        .load_active("user.crod")?
        .compose_db()?;

    let dark = Config::path().options().dark();
    println!("{}", db.select().get(&dark)?);

    let reloaded = db
        .transaction()
        .at(Config::path().retries())
        .set_expr(croxp! { & + 2 })?
        .within(Config::path().options(), |options| {
            options
                .at(Options::path().dark())
                .set_expr(croxp! { !& })?
                .at(Options::path().port())
                .set(8080)
        })?
        .commit()?;
    println!("{:?}", reloaded.changed);
    println!("{}", db.select().get(&dark)?);

    let prod = db.select().with_variant("prod")?;
    println!("{}", prod.get(Config::path().retries())?);

    db.save()
}
```

```console
$ cargo run
false
[Changed("options"), Changed("retries")]
true
30
```

```crol
// user.crod, as the program saved it
retries: & + 2
options.dark: !&
^options.port: 8080
```

### Paths

`Config::path()` starts a path, and each field continues it. A path holds no data,
so keep it and use it as often as you like: pass `&path` to use it again.

| On a                                | Step                             |
| ----------------------------------- | -------------------------------- |
| struct with `#[derive(Navigable)]`  | `.field()`, named as in the Rust |
| `Vec`                               | `.at(index)`                     |
| `HashMap`, `BTreeMap`, `IndexMap`   | `.key(key)`, a string or an integer |
| `Option`                            | `.some()`                        |
| tuple of up to four                 | `.t0()`, `.t1()`, ...            |

A `Box` is walked through, and so is a `#[serde(flatten)]` field. `#[serde(rename)]`
changes the key the path writes, not the method's name. A `#[serde(skip)]` field has
no method. A field whose type has no `Navigable` needs `#[crol(leaf)]`. Paths end at
enums and sets: they do not go into an enum's variants yet.

`.at(i)` is positional: a saved write goes to that index, whatever element is there
by then.

### Reading

`db.select().get(path)` reads the value at a path, as the type the path ends at.
`Config::path()` on its own reads the whole root. A path that is not there is an
error when it is read, and that includes a field left out for its default and a path
through a `None`.

`.with_variant("prod")?` chooses the context, as it does on the composer, and the
base context is read without it. If you limited the contexts with
`CrolConfig::contexts` or `each_variant`, a context that was not evaluated is an
error: `db.evaluate(["prod"])?` evaluates it first. `db.data()` gives the
[`CrolData`](#inspecting-the-result) as it is now, with what has been committed.

### Writing

| On a place, after `.at(path)` | Writes                                           |
| ----------------------------- | ------------------------------------------------ |
| `.set(value)`                 | a fixed value, in place of what is there         |
| `.set_expr(expression)`       | the expression, which stays relative             |
| `.update(closure)`            | a fixed value, computed in Rust                  |
| `.remove()`                   | a remove                                         |

Each gives the transaction back, to go on with `.at`, or to `commit`. The root itself
cannot be written: a path has to reach a root key. A write through `.some()` goes
through a `None` too, as [`?`](language.md#changing-what-is-already-there) does, so it can start a value
that is not there.

**Prefer `set_expr`.** `croxp! { & + 2 }` is kept as written, so if the base later
goes from 1 to 10, the value goes from 3 to 12. A mistake in it is a compile error,
though nothing checks its type before it is read. `#name` puts a Rust variable in
it: `croxp! { & + #delta }`. That makes the macro a `Result`, because the value may
not serialize, and `set_expr` takes that too. `update` is for what CROL cannot say.
It stores a constant, so on a computed value it ends the computing, and each one
after a write evaluates the data again, which takes about as long as a reload. `set`,
`set_expr` and `remove` wait for `commit`.

**A transaction runs in sequence.** `tx.get(path)` and `update` see the operations
before them. `.within(path, closure)` gives the closure the transaction with paths
that start at `path`, as the example does for `Options`, and scopes can nest. An
error from `get` leaves the transaction open. An error from anything else ends it.

**`commit` applies them all or none.** It runs every check that loading runs,
[constraints](schemas.md#constraints) and [sealed fields](schemas.md#sealed-fields)
included, and if one fails the data stays as it was. It returns what
[`reload`](#reloading) does, so `changed` lists the root keys that changed, those that
follow a reference included. Until then nothing changes, and dropping a transaction
discards it. `commit` changes the data in memory, and `save` writes the file.

**A write applies to the transaction's context.** `.with_variant("prod")?` first, and
the write goes under `<prod>`. Without it, the write is a base operation, which
reaches every context except where something more specific applies: with
`<prod> { retries: & * 10 }` in the base, setting `retries` to 3 gives 3 in the base
and 30 in `prod`. `commit` is an error if a write did not take effect in the
transaction's context, and names the operation that won.

### Views and queries

A database reads [views and queries](#views-and-queries) too, with the context it
already has, and with typed paths instead of strings. A transaction sees its own
writes in them, so it can look at the data before it commits.

```rust
// src/main.rs
use std::path::Path;

use crol::{CrolComposer, CrolDb, Navigable, View};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Navigable)]
struct Combat {
    range: u32,
    reload: u32,
}

#[derive(Deserialize, Serialize, Navigable)]
struct Unit {
    hp: u32,
    combat: Combat,
    tags: Vec<String>,
}

#[derive(Deserialize, Serialize, Navigable)]
struct Game {
    artillery: Unit,
    trooper: Unit,
}

#[derive(Debug, View)]
struct Stats {
    hp: u32,
    #[crol(path = "combat.range")]
    range: u32,
}

fn main() -> Result<(), crol::Error> {
    let mut db: CrolDb<Game> =
        CrolComposer::new().load(Path::new("units.crod"))?.compose_db()?;

    let artillery = Game::path().artillery();
    let empire = db.select().with_variant("empire")?;
    println!("{:?}", empire.view::<Stats>(&artillery)?);

    let tags = Unit::path().tags();
    println!("{:?}", db.select().query().holding_at(&tags, "mech").keys()?);

    let trooper = Game::path().trooper();
    let mut tx = db
        .transaction()
        .at(trooper.clone().tags())
        .set(vec!["mech".to_owned()])?
        .at(trooper.clone().hp())
        .update(|hp| hp * 2)?;
    println!("{:?}", tx.query()?.holding_at(&tags, "mech").keys()?);
    println!("{:?}", tx.view::<Stats>(&trooper)?);
    drop(tx);

    println!("{:?}", db.select().view::<Stats>(&trooper)?);
    Ok(())
}
```

```console
$ cargo run
Stats { hp: 1200, range: 12 }
["artillery"]
["artillery", "trooper"]
Stats { hp: 100, range: 5 }
Stats { hp: 50, range: 5 }
```

| On a selection or transaction | Gives                                                 |
| ----------------------------- | ----------------------------------------------------- |
| `.view::<V>(path)`            | a view of the value at a typed path, at any depth     |
| `.query()`                    | a query of its context; call `keys` or `views` on it. On a transaction it is a `Result`, because it evaluates |

A typed path given to `holding_at` starts at the values' type, not the root:
`Unit::path().tags()`. It can go through a list or an `Option`, which a string cannot.
In a `within` scope, a view's path starts at the scope, and a query still covers
every root key. For `variations`, which covers every context, use `db.data()`.

**Batch your writes before you read.** On a transaction, `get`, `view` and `query`
evaluate the data if a write is pending, which takes about as long as a reload,
however small the write. Two writes and one read pay for that once, and a loop that
writes and reads in turn pays for it every time round. With nothing pending, they
cost no more than on a selection. Evaluating parses text layers again, so a
[compiled layer](layers.md#compiled-layers) below the active layer makes it cheaper.

**`get` and `view` treat what is missing differently.** `get` reads one place, so a
value that is not there is an error, even if it has a default. A view follows
[its rules](#views), so the same field can be a default or `None`. The path you give
`view` itself must be set, though, as it must for `get`.

A struct can derive both `View` and `Navigable`, and `#[crol(path = "combat.range")]`
then means the same to both: `Stats::path().range()` goes to `combat.range`.

### The active layer

**It is the last layer.** `load_active` takes one `.crod` file, once, and a `load`
after it is an error. A file that does not exist yet is an empty layer, so a first
run needs nothing else. It is an ordinary layer, so tools can stack it on the others:
`crol explain data/base user.crod retries` shows what the program wrote.

**The database owns it.** It rewrites the whole file, so comments are not kept, and
anything it did not write is an error on loading. `save` refuses to overwrite a file
that changed since it was read: a `CrolDb` is for one process, not several writing
at once. Without `load_active`, transactions still work in memory, `save` is an
error, and `save_as(file)` starts a new file, which must not exist. The next run has
to load that file with `load_active`.

## Cargo features

`crol` has no default features.

| Feature                | Gives                                                      |
| ---------------------- | ---------------------------------------------------------- |
| `derive`               | `#[derive(crol::Schema)]`, `#[derive(crol::VariantName)]`, `#[derive(crol::View)]`, `#[derive(crol::Navigable)]` and `crol::croxp!` |
| `remote-schemas`       | [fetch schemas](schemas.md#remote-schemas) with a blocking client, for tools of your own |
| `remote-schemas-async` | the same, without blocking, using Tokio and reqwest        |

Your program does not need the last two to load data. They are for building
your own tools.

## Next

[Schemas](schemas.md) let tools know your types.
