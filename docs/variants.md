# Variants

Layers combine data from different sources. Variants solve a different
problem: the same data, with differences per environment, region, platform,
feature or tenant.

## Why variants

Without variants, you end up with a layer per combination (`prod`, `prod-eu`,
`prod-eu-cache`, ...), or code that picks values at run time. A variant keeps
each difference next to the value it changes:

```crol
// services.crol
+web: {
    replicas: 2,
    log_level: "info",
}

<prod> {
    web.replicas: 8,
}

<dev> {
    web.log_level: "debug",
}
```

`<prod> { ... }` holds entries that apply only when `prod` is active. The set
of active variants is called the _context_. With nothing active, you get the
_base_ context:

```console
$ crol eval --group dev,prod services.crol
web: { replicas: 2, log_level: "info" }
$ crol eval --group dev,prod --context prod services.crol
web: { replicas: 8, log_level: "info" }
```

## Declaring variants

Only your program declares variants. Data can use them, but not add or change
them, so a typo in a file is an error rather than a new variant. There are two
kinds:

- **Discrete**: a flag, on or off, independent of everything else.
- **Grouped**: alternatives, of which at most one is active.

A variant can have a **parent**: when it is active, its parent is too. So
`prod_eu` below counts as `prod` as well, and only has to state what is
different.

```rust
// src/main.rs
use crol::{CrolComposer, CrolConfig, CrolVariants};
use serde::Deserialize;

#[derive(Deserialize)]
struct Web {
    replicas: u32,
    log_level: String,
}

#[derive(Deserialize)]
struct Root {
    web: Web,
}

fn main() -> Result<(), crol::Error> {
    let variants = CrolVariants::new()
        .add_grouped(["dev", "prod", "prod_eu"])
        .add_discrete("cache")
        .add_parent("prod_eu", "prod");

    let root: Root = CrolComposer::with_cfg(CrolConfig {
        variants,
        ..Default::default()
    })
    .load(std::path::Path::new("services.crol"))?
    .with_variant("prod_eu")?
    .with_variant("cache")?
    .compose()?;

    println!("{} replicas, log level {}", root.web.replicas, root.web.log_level);
    Ok(())
}
```

```console
$ cargo run
8 replicas, log level info
```

`with_variant` fails for a name that was not declared, and for a second choice
in a group.

If you declare no variants at all, every name the data uses counts as a
discrete variant. That is handy for trying things out, but declaring them is
what catches typos.

For the command line, the same declaration is `--group`, `--discrete` and
`--parent`, or the [project file](projects.md#the-project-file).

### Typed variants

Variants can be an enum of your own, so the compiler checks the ones your
program passes. Each name is the variant in snake case, unless you rename it.
This needs the `derive` feature of `crol`.

```toml
# Cargo.toml
[package]
name = "variants-demo"
version = "0.1.0"
edition = "2024"

[dependencies]
crol = { git = "https://github.com/DreamLogics/crol", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
```

```rust
// src/main.rs
use crol::{CrolComposer, CrolConfig, CrolVariants};
use serde::Deserialize;

#[derive(Clone, PartialEq, Eq, crol::VariantName)]
enum Env {
    Dev,
    Prod,
    ProdEu, // written `prod_eu` in data
    #[crol(rename = "cache")]
    WithCache,
}

#[derive(Deserialize)]
struct Web {
    replicas: u32,
    log_level: String,
}

#[derive(Deserialize)]
struct Root {
    web: Web,
}

fn main() -> Result<(), crol::Error> {
    let variants: CrolVariants<Env> = CrolVariants::default()
        .add_grouped([Env::Dev, Env::Prod, Env::ProdEu])
        .add_discrete(Env::WithCache)
        .add_parent(Env::ProdEu, Env::Prod);

    let root: Root = CrolComposer::with_cfg(CrolConfig {
        variants,
        ..Default::default()
    })
    .load(std::path::Path::new("services.crol"))?
    .with_variant(Env::Prod)?
    .compose()?;

    println!("{} replicas, log level {}", root.web.replicas, root.web.log_level);
    Ok(())
}
```

```console
$ cargo run
8 replicas, log level info
```

## Conditions

Between `<` and `>` goes a condition:

| Condition        | Holds when                              |
| ---------------- | --------------------------------------- |
| `<a>`            | `a` is active                           |
| `<!a>`           | `a` is not active                       |
| `<a & b>`        | both hold                               |
| `<a \| b>`       | either holds                            |
| `<a ^ b>`        | exactly one holds                       |
| `<(a \| b) & !c>` | parentheses group                      |

You can write one in several places, all meaning the same thing:

```crol
// example.crol
+web: {
    replicas: 2,
    timeout<dev>: 600, // inside a block
    limits: { cpu: 1.0 },
}

web<prod>.replicas: 8 // on an entry
web.limits<prod>.cpu: 4.0 // anywhere in the path

// Around a group of entries:
<prod & !cache> {
    web.replicas: & + 1,
}
```

```console
$ crol eval --group dev,prod --discrete cache --context prod example.crol
web: { replicas: 9, limits: { cpu: 4.0 } }
$ crol eval --group dev,prod --discrete cache --context dev example.crol
web: { replicas: 2, limits: { cpu: 1.0 }, timeout: 600 }
```

Nesting means _and_: `<a> { <b> { ... } }` is `<a & b>`.

`<a + b>` is different from `<a | b>`: it writes the entries once for each
condition. It shows when both hold: `x<a | b>.n: & + 1` adds 1, and
`x<a + b>.n: & + 1` adds 2.

A condition that can never hold, like `<dev & prod>` for two variants in one
group, is an error.

## Which entry wins

A key's value in a context is built in two steps. First come all of its normal
entries, across all layers. Then come the conditional entries that hold, least
specific first. So:

- a change in a later layer reaches every context, because the base is built
  first;
- a more specific condition wins over a less specific one, in whatever file or
  layer it is written, as with CSS selectors.

Specificity counts how much a condition says. `a` is 1, plus 1 for each
ancestor; `!a` is 0; `a & b` adds up; `a | b` and `a ^ b` take the lower of the
two. Entries of equal specificity keep their normal order.

```crol
// example.crol
+web: { replicas: 2 }

<prod> {
    web.replicas: & * 4,
}

<prod_eu> {
    web.replicas: & + 2,
}

<prod & !cache> {
    web.replicas: & + 1,
}
```

```console
$ crol eval --group prod,prod_eu --discrete cache --parent prod_eu=prod --context prod_eu example.crol
web: { replicas: 11 }
```

In `{prod_eu}`, the base gives 2. `<prod>` (specificity 1) makes it 8.
`<prod & !cache>` (also 1, written later) makes it 9. `<prod_eu>` (specificity
2, because it has a parent) comes last and makes it 11.

`crol explain` shows this order for any value:

```console
$ crol explain --group prod,prod_eu --discrete cache --parent prod_eu=prod --context prod_eu example.crol web.replicas
web.replicas in {prod_eu}:
  example.crol:2:2   +web                         definition  2
  example.crol:5:5   web<prod>.replicas           patch       8
  example.crol:13:5  web<prod & !cache>.replicas  patch       9
  example.crol:9:5   web<prod_eu>.replicas        patch       11
```

## References and contexts

Late references inherit the context, so `&root.database.host` reads the host in
the current context. In a reference, a variant does not test anything: it
_switches_ context for that one reference. This is how you compare environments
or copy a value from another one:

```crol
// example.crol
+database: { host: "db.local" }
database<prod>.host: "db.internal"

+report: {
    current: &root.database.host,
    production: &root.database<prod>.host,
    development: &root.database<dev>.host,
}
```

```console
$ crol eval --group dev,prod --context dev example.crol report
current: "db.local"
production: "db.internal"
development: "db.local"
```

Only names, `!` and `&` are allowed there, since the reference has to name
exactly one context.

## Keys that exist only in some contexts

A conditional entry can add a key that the base lacks, or remove one:

```crol
// example.crol
+web: { port: 80 }

<dev> {
    +debug_tools: { port: 9999 },
}
```

Whether that is allowed is up to your program. `CrolConfig::variant_keys` is
`Any` by default. `NoNewKeys` means every key in a context must exist in the
base, and `SameKeys` means every context has exactly the base's keys.

## Using more than one context

`compose` evaluates the context you chose with `with_variant`. `compose_data`
evaluates every valid context and checks them all, which is what `crol check`
does: an error that only shows up in `<prod_eu & cache>` is found before you
deploy to it. Messages say which contexts they apply to.

```rust
// src/main.rs
use crol::{CrolComposer, CrolConfig, CrolLayer, CrolVariants};
use serde::Deserialize;

#[derive(Deserialize)]
struct Web {
    replicas: u32,
}

fn main() -> Result<(), crol::Error> {
    let variants = CrolVariants::new().add_grouped(["dev", "prod"]);
    let data = CrolComposer::with_cfg(CrolConfig {
        variants,
        ..Default::default()
    })
    .load_layer(CrolLayer::from_sources(
        "base",
        [("a.crol", "+web: { replicas: 2 }, web<prod>.replicas: 8")],
    ))
    .compose_data()?;

    let base: Web = data.get("web")?;
    let prod = data.context(["prod"])?;
    let in_prod: Web = data.get_in("web", &prod)?;
    println!("{} in the base, {} in prod", base.replicas, in_prod.replicas);
    Ok(())
}
```

```console
$ cargo run
2 in the base, 8 in prod
```

The number of contexts is every group's choice (or none) times every discrete
variant on or off, so it multiplies. Past `CrolConfig::max_contexts` (4096 by
default) you have to list the contexts you actually use in
`CrolConfig::contexts`. Only evaluated contexts are checked.

To see everything at once, `crol eval --all-contexts` prints the base and then
what differs in each other context.

## Next

[Loading data in Rust](rust.md) covers how values map onto your types.
