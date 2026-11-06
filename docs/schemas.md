# Schemas

Your program knows the types of its data. Tools do not: without help, `crol
check` and your editor can only see that a file parses and evaluates, not that
`replicas` is a number or that `Service` has no field `replica`. A schema is a
description of your Rust types that tools can read. With one, you get type
errors, completion and hover docs without running your program.

You never write a schema by hand. **It is generated from your Rust types**, and
stored in a file that you commit.

Loading does not need a schema. `crol::from_str` and `CrolComposer` work without
one, and you can add one at any time.

## Generating a schema

This section builds one from scratch. Enable the `derive` feature:

```toml
# Cargo.toml
[package]
name = "fleet"
version = "0.1.0"
edition = "2024"

[dependencies]
crol = { version = "0.1", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
```

Then add `crol::Schema` to the derives of the types your data loads into. Doc
comments are kept, and become the docs your editor shows:

```rust
// src/lib.rs
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Everything the data can define.
#[derive(Debug, Deserialize, Serialize, crol::Schema)]
pub enum Definition {
    Service(Service),
    Volume(Volume),
}

/// A service that runs one container image.
#[derive(Debug, Deserialize, Serialize, crol::Schema)]
#[crol(defaults)]
pub struct Service {
    /// The image to run.
    pub image: String,
    /// The port it listens on.
    pub port: u16,
    /// Number of copies to run.
    #[serde(default = "one")]
    pub replicas: u32,
    /// What must be running first.
    #[serde(default)]
    #[crol(refers(Service, Volume))]
    pub depends_on: Vec<crol::Id>,
    /// Extra environment variables.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// Storage that services can mount.
#[derive(Debug, Deserialize, Serialize, crol::Schema)]
pub struct Volume {
    /// The size in gigabytes.
    pub size_gb: u32,
}

fn one() -> u32 {
    1
}
```

Every type reachable from the root must implement `crol::Schema`. `crol`
provides it for the scalars, `String`, `Option`, `Box`, `Rc`, `Arc`, `Cow`, the
standard collections, `IndexMap`, tuples, arrays, `PathBuf`, `Duration` and
`crol::Id`. For a type from another crate, such as a `chrono` date, say what it
looks like in the data, with [`#[crol(schema = String)]`](#attributes) or
`#[crol(schema = any)]`.

Then write the schema to a file. Building it runs your code, so it cannot
happen in `build.rs`, and the usual way is a test:

```rust
// tests/schema.rs
#[test]
fn crol_schema() {
    crol::schema::check_file::<fleet::Definition>("crol/schema.crol").unwrap();
}
```

`check_file` writes the file if it does not exist yet, and fails if the file
differs from your types. So `cargo test` tells you when the file is out of date,
in CI as well. To update it after changing your types:

```console
$ CROL_UPDATE_SCHEMA=1 cargo test
```

Commit the result. It is a CROL file, sorted so that it gives small diffs:

```console
$ head -n 12 crol/schema.crol
#![schema("crate:crol@0.1")]

Schema {
    format: 1,
    root: "fleet::Definition",
    types: {
        "fleet::Definition": Enum {
            name: "Definition",
            doc: "Everything the data can define.",
            source: ("src/lib.rs", 7),
            variants: {
                Service: { shape: Newtype(Named("fleet::Service")), .. },
```

The type you pass (`Definition` here) is the schema's _root type_. In layered data,
an [enum of everything the data can define](rust.md#enums) is the type of each
root value, and [a struct](rust.md#a-struct-at-the-root) is the whole document, so
each root key is one of its fields. In value mode it is the type of the whole
document.

If you would rather have a flag than a test, `crol::schema::write::<T>(path)`
writes the file unconditionally, for an `--export-schema` option of your own.

### Attributes

Serde's attributes are understood: `rename`, `rename_all`, `alias`, `default`,
`skip`, `flatten`, `tag`, `content`, `untagged`, `transparent` and
`deny_unknown_fields`. `crol` adds these:

| Attribute                        | On         | Means                                                       |
| -------------------------------- | ---------- | ----------------------------------------------------------- |
| `#[crol(defaults)]`              | a type     | record default values, for `&default` and tools             |
| `#[crol(explicit_defaults)]`     | a type     | [fields left out always need `..`](#types-that-decide-about-)  |
| `#[crol(explicit_defaults = false)]` | a type | [fields left out take their defaults, without `..`](#types-that-decide-about-) |
| `#[crol(refers(Service, Volume))]` | an id field | which root types its ids may name, also in lists, options and maps |
| `#[crol(schema = OtherType)]`    | a field    | describe it as a type with the same serialized shape        |
| `#[crol(schema = any)]`          | a field    | accept anything here, unchecked                             |
| `#[crol(constraint = "name")]`   | a field, newtype | [the constraint on its value](#constraints)           |
| `#[crol(key_constraint = "name")]` | a map field | the constraint on its keys                                 |
| `#[crol(constraints(name = Min(0)))]` | a type | define sealed constraints                                  |
| `#[crol(open_constraints(...))]` | a type     | define constraints that layers may change                   |
| `#[crol(sealed)]`                | a field, type | [only the layer that defines a value can set it](#sealed-fields) |

Use `schema = ...` for a field whose type has no `Schema`, such as a type from
another crate, and for fields with `#[serde(with = "...")]`, which the derive
rejects without it, since their serialized shape is not their type's.

`#[crol(defaults)]` needs the types involved to implement `Serialize`.

## Checking data against it

Hand the schema to `crol check` with `--schema`:

```crol
// data/services.crod
+web: Service {
    image: "web:1.4",
    port: 80,
    replica: 3,
    depends_on: [@database],
    ..,
}
```

```console
$ crol check --schema crol/schema.crol data
error: `Service` has no field `replica`; its fields are `image`, `port`, `replicas`, `depends_on`, `env`
  ┌─ /path/to/project/data/services.crod:5:14
  │
5 │     replica: 3,
  │              ^
  │
  = while evaluating web.replica

error: the id `@database` names no root key
  ┌─ /path/to/project/data/services.crod:6:18
  │
6 │     depends_on: [@database],
  │                  ^^^^^^^^^
  │
  = while evaluating web.depends_on[0]
```

Every problem appears at once, in every context, with the same rules and
messages as loading the data in your program. Unlike your program, it also
checks ids: each must name a root key, of a type that `refers` allows.

`crol eval` uses a schema too. Fields that take their defaults are written out,
each marked with a comment, so you can see what your program will get:

```crol
// data/services.crod
+web: Service {
    image: "web:1.4",
    port: 80,
    ..,
}
```

```console
$ crol eval --schema crol/schema.crol ./data
web: Service {
    image: "web:1.4",
    port: 80,
    replicas: 1, // default
    depends_on: [], // default
    env: {}, // default
}
```

Pass `--no-defaults` to print the data as it is.

## How schemas are found

Only tools look for a schema. Your program has the types, so it ignores
`#![schema(...)]` and the project file, and uses only what you give
`CrolComposer::schema`.

Tools take the first of these that applies:

1. The `--schema` option (command line only).
2. The `schema` of the [project file](projects.md#the-project-file). The usual
   choice for a project: it covers every layer, and there is nothing to repeat.
3. A `#![schema(...)]` attribute in the document. The usual choice for a single
   file that people edit on its own.

The command line reads the project file only when you give it no layers, or
pass `--project`.

When several documents name a schema with the attribute and there is no project
or `--schema`, they must agree: otherwise `crol check` stops and asks you to
choose with `--schema`. A document whose attribute disagrees with the project's
schema or `--schema` gets a warning, and is checked against that schema anyway.

Without any schema, tools check syntax and evaluation, but not types.

### The schema attribute

`#![schema(...)]` goes at the top of a document, and takes a module path, a URL
or a crate:

| Form                                            | Finds                                                       |
| ----------------------------------------------- | ----------------------------------------------------------- |
| `#![schema(::crol::schema)]`                    | a file, by [module path](projects.md#module-paths), from the root directory |
| `#![schema(crol::schema)]`                      | the same, from the document's own directory                 |
| `#![schema(super::schema)]`                     | the same, from the parent directory                         |
| `#![schema("https://example.org/v2/schema.crol")]` | a file [over HTTPS](#remote-schemas)                     |
| `#![schema("crate:fleet@2.1")]`                 | the schema a [crate publishes](#remote-schemas)             |

```crol
// settings.crod
#![schema(crol::schema)]

+web: Service {
    image: "web:1.4",
    port: "80",
    ..,
}
```

```console
$ crol check settings.crod
error: expected u16, found "80"
  ┌─ settings.crod:6:11
  │
6 │     port: "80",
  │           ^^^^
  │
  = while evaluating web.port
```

### In a project file

The same forms work in `project.crol`, written as a string. A module path there
always starts at the project's directory:

```crol
// project.crol
Project {
    layers: List([{ name: "base", dir: "data", .. }]),
    schema: Some("crol::schema"),
    ..,
}
```

With no layers on the command line, `crol` reads all of this from the project
file:

```console
$ crol check
```

### Remote schemas

A schema does not have to be on disk. This is for files whose users do not have
your source: a configuration file for an application they installed, say. They
get completion and checks from nothing but the file.

**From a URL.** Put the schema file on any HTTPS server, with a version in the
path, and name it:

```crol
#![schema("https://example.org/fleet/v2/schema.crol")]

+web: Service {
    image: "web:1.4",
    port: 80,
    ..,
}
```

**From a crate.** Include the file in your package and say where it is. It is
`crol/schema.crol` by default, so the table can be empty:

```toml
[package.metadata.crol]
schema = "crol/schema.crol"
```

Then `#![schema("crate:fleet@2.1")]` names it, with a version requirement as in
`Cargo.toml`. Leave out `@...` to accept any version. Tools look for the crate
in this order:

1. the current Cargo workspace and its dependencies;
2. the local Cargo registry cache (`~/.cargo/registry/src`);
3. crates.io, taking the highest matching version.

`crate:crol` is built in: it is the schema of CROL's own schema files. `crol
check` and your editor check a schema file against it without looking anything
up, and your editor completes and documents it at every level.

**Caching.** Remote schemas are fetched over HTTPS only, and cached in
`CROL_CACHE_DIR` if set, or else a `crol` directory in your platform's cache
directory (`$XDG_CACHE_HOME` or `~/.cache`, `~/Library/Caches`,
`%LOCALAPPDATA%`). A URL is fetched again once per `crol` run or language
server session, falling back to the cached copy if that fails. A crate's schema
is fetched once per version, since published versions do not change.

**Offline.** `crol --offline` and the language server's `remote_schemas: false`
turn fetching off. Cached copies and local crates are still used.

The `crol` tool and language server fetch out of the box. In your own tools,
enable [a `remote-schemas` feature](rust.md#cargo-features), and use
`crol::schema::resolve_remote`.

## What is checked

A schema makes tools apply the same rules as your program:

- type names, and values of the wrong kind or out of range;
- unknown fields;
- missing fields, on final values only, so a patch can leave out what it does
  not change;
- enum variants;
- ids: each must name a root key in the same context, of an allowed type.

Templates are not root values, so they are not checked as data.

### Which type a document has

In layered data, the kind of the root type says what it describes:

| Root type                                  | Describes         | A root key is                 |
| ------------------------------------------ | ----------------- | ----------------------------- |
| a struct                                   | the whole document | a field of it                 |
| a map, like `IndexMap<String, Service>`    | the whole document | a key, with a `Service` value |
| an enum, or anything else that has no keys | each root value   | a name for a value of it      |

A newtype or `#[serde(transparent)]` wrapper counts as what it wraps.

With `Definition`, an enum, as the root, `+web: Service { ... }` picks the `Service`
variant. With a struct like `Config`, `port: 8080` is its field `port`, and
`name: "x"` in a file is checked as `Config`'s `name`.

For a struct, the check runs on all the layers together, never on one file. So
a layer can leave out what another sets, and `crol check` and your editor only
report a missing field when the result lacks it. A missing field belongs to no
line, so it is reported at the top of the last file of the last layer. A
`.crod` file on its own, with what it extends, is checked as a whole.

To check data whose every root value is one struct, like a map of services, build
the schema from the map: `crol::schema::check_file::<IndexMap<String, Service>>(...)`.

A project can name another root with `root: Some("fleet::Definition")`, and the
kind of that type decides in the same way. A plain type name works if it is
unambiguous.

In value mode, the root type is the type of the whole document, unless the
document has a top-level type name, which then chooses it. `crol check` on a
`.crol` file and your editor both do this.

### Types with the same name

Data refers to types by name, but two schema types can share one, like
`net::Config` and `db::Config`. In that case, tools ask you to say which one you
mean. `#![use(...)]` at the top of the document does that, and `as` gives a type
a name of its own in that file:

```crol
#![use(app::net::Config, app::db::Config as DbConfig)]

listener: Config { port: 8080 }
database: DbConfig { url: "postgres://db" }
```

A path is the Rust module path followed by the type's name as data writes it.
Your program replaces `DbConfig` by `Config` when it reads the file, so
`#[derive(Deserialize)]` needs no change.

## Using `&default`

`&default` reads the Rust default of the field it is written in, so data can
build on it. It is the one feature of the language that needs a schema when
loading, because serde has no way to ask a type for its defaults. Mark the types
with `#[crol(defaults)]`, and give the composer the schema:

```rust
// src/main.rs
use std::collections::BTreeMap;

use crol::{CrolComposer, CrolLayer};
use fleet::Definition;

fn main() -> Result<(), crol::Error> {
    let text = r#"
        +web: Service {
            image: "web:1.4",
            port: 80,
            replicas: &default * 3,
            ..,
        },
    "#;
    let definitions: BTreeMap<String, Definition> = CrolComposer::new()
        .schema(crol::schema::build::<Definition>())
        .load_layer(CrolLayer::from_sources("base", [("a.crod", text)]))
        .compose()?;
    println!("{:?}", definitions["web"]);
    Ok(())
}
```

```console
$ cargo run
Service(Service { image: "web:1.4", port: 80, replicas: 3, depends_on: [], env: {} })
```

A field's default is its own `#[serde(default)]` if it has one, or else that
field's value in the struct's default. Directly under a type name, `&default` is
the whole default value, so `..&default` writes every default into the block.
The difference with `..` is when the defaults are filled in: `..` leaves fields
out for serde, and `..&default` copies them into the data.

Where no default is recorded, `&default` counts as missing, so `&default | 1`
falls back.

`crol check`, `crol eval`, `crol explain`, `crol fingerprint` and the language
server read defaults from the schema they check against: `--schema`, the project's,
or the one a document names. Generate the schema from types with
`#[crol(defaults)]`, so that it records them.

With a struct as the root type, a root key is a field like any other, so
`retries: &default + 1` is one more than the default of that field.

### Paths through fields left out

Data that leaves defaults out is short, but a path cannot go through what is not
there: `web.limits.cpu: 2.0` fails when `limits` was left out. With
`CrolConfig::init_defaults`, or `init_defaults: true` in the project file's
`options`, such a step starts from its default, found as `&default` finds it. So
the path works, `&` in it reads the default, and a block merged into the field
starts from the default as well. An `Option` that is `None` starts from the default
of what it holds. A step with no default stays an error.

To do this for one step, in any data, write a `?` after it: `web.limits?.cpu: 2.0`.
See [paths](language.md).

**Limits.** `&default` works in layered data only. In a value-mode document it is
an error, with `crol::from_str`, `crol::from_str_with_schema`, a `.crol` file given
to `crol` and in the language server alike. There, and in data with no schema at
all, it reports `defaults come from a schema, and no schema is given`.

## Constraints

A type says what kind of value a field holds. A _constraint_ says more, such as a
range, or a set of values. Your types name constraints, and either define them or
leave that to the data:

```rust
// src/lib.rs
use std::collections::BTreeMap;

use serde::Deserialize;

#[derive(Deserialize, crol::Schema)]
#[crol(constraints(positive = Min(0)))]
#[crol(open_constraints(channel = OneOf(["voice", "music"])))]
pub struct Unit {
    #[crol(constraint = "positive")]
    pub hp: i32,
    #[crol(constraint = "channel")]
    pub channels: Vec<String>,
    #[crol(key_constraint = "stat")]
    pub bonuses: BTreeMap<String, i32>,
}
```

| Constraint              | Holds for                                                |
| ----------------------- | -------------------------------------------------------- |
| `Min(n)`, `Max(n)`      | a number not below, or not above, `n`                    |
| `Range(a, b)`           | a number from `a` to `b`, inclusive                      |
| `OneOf(["a", "b"])`     | one of these; a string, an id and a unit name count as their text |
| `Refers(Type)`          | an id or string naming a root value of that type         |

On an `Option`, list or map, a constraint is on what it holds, and `key_constraint`
is on a map's keys. `#[crol(constraint = "...")]` on a newtype holds wherever the
type is used. Constraints are checked on the final value, in every context:

```console
$ crol check
error: -1 is below 0, the minimum of `positive`
  ┌─ data/local/overrides.crod:2:7
  │
2 │ web.hp: -1
  │         ^^
```

### Constraints in the data

A schema is generated, so you do not edit it. To define or extend a constraint,
write a file called `schema.crod` at the root of a layer. It is not data, and it can
hold only `constraints`:

```crol
// data/base/schema.crod
constraints: {
    stat: OneOf(["hit_points", "speed"]),
}
```

This is how a game engine can name a constraint and leave its values to the game. A
higher layer builds on the ones below, like any data, so an extension can add to a
set:

```crol
// extensions/shields/schema.crod
constraints.stat: OneOf(& + ["shields"])
```

A constraint from `#[crol(constraints(...))]` is _sealed_: your program relies on it,
so changing it in a `schema.crod` is an error. Use `open_constraints` for one that
layers may change. A name that nothing defines is an error too.

`CrolComposer`, `crol` and your editor read these files by themselves, given a schema.

## Sealed fields

Some fields only make sense where a value is defined, such as an id into another
file. `#[crol(sealed)]` says that only the layer that defines a value can set the
field. On a type, it seals all of its fields.

```rust
// src/lib.rs
use serde::Deserialize;

#[derive(Deserialize, crol::Schema)]
pub struct Tech {
    #[crol(sealed)]
    pub original: u32,
    pub name: String,
}
```

```crol
// data/base/techs.crod
+armour: Tech { original: 12, name: "Armour" }
```

```crol
// data/mod/changes.crod
armour.name: "Better armour"   // fine
armour.original: 13            // error
+shields: Tech { original: 99, name: "Shields" }   // fine: this layer defines it
```

```console
$ crol check
error: `armour.original` is sealed: only the layer that defines `armour` (`base`) can set it
```

The check compares values, so it does not matter how the value changed: through a
path, by defining the key again, or through a reference to something a higher layer
changed. Your editor leaves sealed fields out of completion in higher layers.

## What else the loader learns from a schema

serde tells a deserializer nothing about attributes, so two more things reach it
only through a schema: which field names are `#[serde(alias)]`es of one field, so
that a field written by its alias counts as present, and
[`explicit_defaults`](#types-that-decide-about-). Give the loader the schema if
you use either: `CrolComposer::schema` for layers, and
`crol::from_str_with_schema(text, &schema)` for a single document.

## Checking from Rust

`crol::check` applies the same checks to evaluated data, without deserializing,
and reports everything it finds:

```rust
// src/main.rs
use crol::{CheckOptions, CrolComposer, CrolLayer};
use fleet::Definition;

fn main() -> Result<(), crol::Error> {
    let text = r#"+web: Service { image: "web:1.4", port: 70000, depends_on: [@nothing], .. }"#;
    let data = CrolComposer::new()
        .load_layer(CrolLayer::from_sources("base", [("a.crod", text)]))
        .compose_data()?;
    let schema = crol::schema::build::<Definition>();
    for diagnostic in crol::check(&data, &schema, None, &CheckOptions::default()) {
        println!("{}", diagnostic.display(data.sources()));
    }
    Ok(())
}
```

```console
$ cargo run
base/a.crod:1:41: expected u16, found 70000 (while evaluating web.port)
base/a.crod:1:61: the id `@nothing` names no root key (while evaluating web.depends_on[0])
```

The third argument names the root type by its path, and `None` uses the schema's
own. `crol::check_value` does the same for a value-mode document.

## Types that decide about `..`

`#[crol(explicit_defaults)]` and `#[crol(explicit_defaults = false)]` let a type
decide whether its fields need `..` to be left out.
[Loading data in Rust](rust.md#types-that-decide-for-themselves) explains them. The
setting is stored in the schema, so the tools agree with your program:

```rust
// src/lib.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, crol::Schema)]
pub enum Definition {
    Retry(Retry),
    Service(Service),
}

#[derive(Debug, Deserialize, Serialize, crol::Schema)]
#[crol(explicit_defaults = false)]
pub struct Retry {
    pub limit: u32,
    #[serde(default)]
    pub backoff_ms: u32,
}

#[derive(Debug, Deserialize, Serialize, crol::Schema)]
pub struct Service {
    pub port: u16,
    #[serde(default)]
    pub replicas: u32,
}
```

```crol
// retry.crod
+fast: Retry { limit: 3 }
+web: Service { port: 80 }
```

```console
$ CROL_UPDATE_SCHEMA=1 cargo test
$ crol check --schema crol/schema.crol ./retry.crod
error: `Service` is missing the field `replicas`; write it, or end the block with `..` to leave out fields that have defaults
  ┌─ ./retry.crod:3:7
  │
3 │ +web: Service { port: 80 }
  │       ^^^^^^^^^^^^^^^^^^^^
  │
  = while evaluating web
```

`Retry` needs no `..`, and `Service`, with no attribute, still does.

Schema files that do not use the attribute look the same as before, and a tool too
old to know it rejects a file that does, rather than check it by other rules.

### Generated files and defaults

Every field of a struct with `#[serde(default)]` on the struct now counts as
having a default, so tools accept `Limits { .. }` for such a struct, as serde
does. Fields that used to be recorded as required get `default: Unrecorded`, or
their value:

```rust
// src/lib.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Deserialize, Serialize, crol::Schema)]
#[serde(default)]
pub struct Limits {
    pub cpu: f64,
}

#[derive(Debug, Deserialize, Serialize, crol::Schema)]
pub enum Definition {
    Limits(Limits),
}
```

```console
$ CROL_UPDATE_SCHEMA=1 cargo test
$ grep cpu crol/schema.crol
            fields: { cpu: { ty: F64, default: Unrecorded, .. } },
```

If your types have such a struct, a schema file generated earlier is out of date,
and `crol::schema::check_file` fails until you regenerate it with
`CROL_UPDATE_SCHEMA=1 cargo test`.

## Next

[Project structure](projects.md) shows where all these files go.
