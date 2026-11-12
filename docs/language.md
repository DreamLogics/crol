# The language by example

If you know Rust, you can already read most of CROL. This page goes through
what you can write in a file, from plain values to references and templates.
Every example here runs: save it to a file and try `crol eval` on it.

## Entries

A file is a list of _entries_, usually one per line. Comments are written as in
Rust: `// to the end of the line`, or `/* like this */`, which can nest. `///` lines
directly above an entry are its doc comment, and `//!` lines at the top of a file the
file's. Editors show them; they do not change the data.

An entry is an operation on a key. There are four, and the prefix says which
one you mean:

```crol
// example.crod
+name: "web" // declare: give `name` its first value; an error if it is already set
port: 8080 // set: create `port`, or replace it
?replicas: 2 // ensure: set `replicas` unless it is already set
```

The fourth removes a key, and is an error if the key is not set:

```crol
-replicas
```

To remove a key only if it is set, write `-?`. It does nothing when the key, or a
step of its path, is not set, so it keeps working when a file below stops setting
the key:

```crol
-?replicas
-?web.limits.cpu // no error without `web.limits`
```

A key is _set_ when the data holds a value for it. That goes for the keys of a
map and the fields of a struct alike: a struct's type always has its fields, but
each one is unset until an entry sets it, and `-` unsets it again.

Why have several ways of writing a key? Because your intent is checked. In a
project with many files, `+web` says "I am introducing `web`", and you get an
error if someone else already did, rather than a silent overwrite.

### Line breaks and commas

Entries are separated by a line break, a comma, or both, at the top level and in
any block. A trailing comma is allowed, and two entries on one line need a comma
between them:

```crol
// example.crod
name: "web"
port: 80
limits: { cpu: 1.0, memory_mb: 512 }
env: {
    LOG_LEVEL: "info",
    PORT: &root.port,
}
```

`crol fmt` leaves out the commas after top-level entries, and keeps them in nested
blocks, so that is the style in this guide. Both ways mean the same.

**Lists and tuples only take commas.** A list element has no shape that tells it
from the expression before it, so inside `[...]` and `(...)` a line break means
nothing, and a missing comma is an error.

**A value can go on across lines.** A line break ends an entry only when the next
line _begins an entry_. So you can break a long expression, with the operator at
the end of a line or at the start of the next:

```crol
// example.crod
base: 100
extra: 20
total: &root.base + &root.extra - 5
sum: 1 + 2
```

```console
$ crol eval example.crod
base: 100
extra: 20
total: 115
sum: 3
```

Most lines are not in doubt: `?`, `^`, `..`, a key or a bind can only start an
entry, and `.`, `*`, `/`, `%`, `~`, `~^`, `|` and `??` can only continue a value. Four tokens can do
both, and what follows them on the same line decides:

| Line starts with | Begins an entry when...                            | Otherwise it continues the value |
| ---------------- | -------------------------------------------------- | -------------------------------- |
| `+`              | a key and `:` follow: `+b: 2`                      | `+ 2`, `+ b`                     |
| `-`              | an identifier, string or `?` follows: `-b`, `-?12` | `- 2`, `- &x`, `- ["x"]`         |
| `[`              | it is a key and `:` follows: `["a b"]: 1`          | `[0]`, `["key"].name`            |
| `<`              | it is a condition and `{` follows: `<prod> {`      | `<prod>`, `<prod>.name`          |

```crol
// example.crod
a: 1
+b: 2 // a new entry
c: 1 + 2 // still `c`: 3
```

```console
$ crol eval example.crod
a: 1
b: 2
c: 3
```

**When you still need a comma:**

- Between two entries on one line.
- Between list and tuple elements.
- Before removing an integer key. `-12` on a line of its own always continues the
  value, as a subtraction, so after `a: 5` it gives `-7`. Write the comma, and
  `crol fmt` will keep it:

```crol
// example.crod
+ports: { 80: "http", 443: "https" }

ports: & ~ {
    name: "web",
    -443,
}
```

```console
$ crol check example.crod
```

A line that does not begin an entry stays with the value. So a mistake is an
error rather than a different document: after `a: 1 +`, the line `b: 2` gives
``unexpected `:` after an entry``.

## Values

| Kind       | Examples                                                    |
| ---------- | ----------------------------------------------------------- |
| boolean    | `true`, `false`                                             |
| integer    | `12`, `-3`, `0x1F`, `0o17`, `0b1010`, `1_000`               |
| float      | `1.5`, `1e3`, `2.5E-3`                                      |
| char       | `'a'`, `'\n'`, `'\u{E9}'`                                   |
| string     | `"text"`, `"line\n"`, `r"raw"`, `r#"with "quotes""#`        |
| list       | `[1, 2, 3]`                                                 |
| tuple      | `(1, "a")`, `(1,)`, `()`                                    |
| keyed      | `{ port: 80 }`, `Service { port: 80 }`                      |
| unit name  | `None`, `Red`: a unit enum variant or unit struct           |
| named tuple | `Some(1)`, `Rgb(1, 2, 3)`: a tuple variant or tuple struct |
| id         | `@web`, `@"odd id"`, `@settings.filter`, `@::web`            |

Numbers have no suffixes (`1u8` is an error), and floats need a digit on both
sides of the dot. Strings can span several lines.

## Structs and maps

Structs and maps look the same in CROL, with one notation, a _keyed value_.
Your Rust type decides which it becomes.

```crol
// example.crod
+web: Service {
    replicas: 2,
    limits: Limits { cpu: 1.5, memory_mb: 512 },
    env: { LOG_LEVEL: "info", "odd key": "yes" },
    ports: { 80: "http", 443: "https" },
}
```

The name before the braces is the _type name_. It is optional, but when you
write it, it has to match the Rust type. Writing `Limits` on a `Service` field
is an error, so the name acts as a check and tells readers what they are
looking at. It also decides which variant of an enum a value is, as the next
section shows. A value that is a plain map has no type name, and with a
[schema](schemas.md) a name written on one gets a warning.

Keys are identifiers, quoted strings (for anything that is not an identifier)
or integers. `port` and `"port"` are the same key; `12` and `"12"` are not.
A key can also be an [id](#ids-as-keys), `@web: 1`, in a map whose keys name
other entries.

## Enums and options

Enum values are written as in Rust:

```crol
// example.crod
+mode: Fast // a unit variant
+colour: Rgb(255, 128, 0) // a tuple variant
+shape: Circle { radius: 2.0 } // a struct variant
+cache: Some("redis://cache:6379") // Option
+proxy: None
```

An `Option<T>` field also accepts a plain value, so `timeout: 30` is the same
as `timeout: Some(30)`.

## Changing what is already there

A _path_ reaches inside a value. This is how you change one field without
repeating the rest:

```crol
// example.crod
+web: {
    limits: { cpu: 1.0, memory_mb: 512 },
    tags: ["a", "b"],
    point: (1, 2),
    env: { "LOG LEVEL": "info" },
}

web.limits.cpu: 2.0 // a field
web.tags[0]: "first" // a list element
web.point.1: 3 // a tuple element
web.env["LOG LEVEL"]: "debug" // a key that needs quotes
```

```console
$ crol eval example.crod
web: {
    limits: { cpu: 2.0, memory_mb: 512 },
    tags: ["first", "b"],
    point: (1, 3),
    env: { "LOG LEVEL": "debug" },
}
```

A field or key goes into a named tuple of one element, such as `Some(...)` or a
newtype variant: with `weapon: Some({ damage: 5 })`, `&root.weapon.damage` is `5`,
and `weapon.damage: 6` gives `Some({ damage: 6 })`. Writing `.0.damage` means the
same.

A `?` after a step goes through it whatever it holds: `web.limits?.cpu: 2.0` works
if `limits` is not set or is `None`, starting from its default, or `{ .. }` if it has
none. If `limits` is `Some(...)`, the path goes into what it holds.

Every other step of the path but the last must exist, or you get an error that
names the missing step. Removing works the same way: `-web.limits.memory_mb`.
`-?web.limits.memory_mb` is no error: without `limits` or `memory_mb`, it does
nothing.
With a schema, [`init_defaults`](rust.md#configuration) lets a path go through a
field that was left out, from its default.

To change several fields at once, use `~`, the _merge_ operator, with `&` for
the value below. The block on the right is applied on top of the value on the
left:

```crol
// example.crod
+web: { replicas: 1, port: 80, limits: { cpu: 1.0, memory_mb: 512 } }

web: & ~ {
    replicas: 2,
    limits.cpu: 2.0,
}
```

```console
$ crol eval example.crod
web: { replicas: 2, port: 80, limits: { cpu: 2.0, memory_mb: 512 } }
```

Merging is deep: a block inside the patch merges with what is there, at any
depth. So `limits: { cpu: 2.0 }` in the patch means the same as `limits.cpu: 2.0`
above, and `memory_mb` stays. The same goes for a path that ends in a block,
`web.limits: { cpu: 2.0 }`. Lists are replaced, and so is any value that is not
written as a block. A block inside a named tuple counts too: over a value of the same
name, `weapon: Active({ range: 5 })` changes `range` and keeps the rest of the payload.

Two things replace instead. `^` in front of a key replaces that one value, and
`~^` is a _shallow_ merge, where no block inside merges:

```crol
// example.crod
+a: { limits: { cpu: 1.0, memory_mb: 512 }, port: 80 }
+b: { limits: { cpu: 1.0, memory_mb: 512 }, port: 80 }

a: & ~ { ^limits: { cpu: 2.0 } }
b: & ~^ { limits: { cpu: 2.0 } }
```

```console
$ crol eval example.crod
a: { limits: { cpu: 2.0 }, port: 80 }
b: { limits: { cpu: 2.0 }, port: 80 }
```

A block of another type replaces too, which is how you change an enum's struct
variant: `shape: Square { side: 2 }` over a `Circle`.

Inside a tuple, or a named tuple of the same name, `&` reads the same position of
the value below, so you can change what one holds without taking it apart:

```crol
// example.crod
+colour: Rgb(10, 20, 30)
+armour: Some(5)

colour: Rgb(& + 1, &, 0)
armour: Some(& + 1)
```

```console
$ crol eval example.crod
colour: Rgb(11, 20, 0)
armour: Some(6)
```

## References

References read other values. They all start with `&`:

| Reference    | Reads                                                   |
| ------------ | ------------------------------------------------------- |
| `&`          | the value below: this target before this entry applied  |
| `&self`      | the block this entry is in                              |
| `&parent`    | the block around that; `&parent.&parent` goes further   |
| `&super`     | the same as `&parent`, spelled as module paths go up    |
| `&top`       | the root entry this block is in, however deep           |
| `&root`      | the top level of the data                               |
| `&name`      | a bind or template, see below                           |
| `&default`   | the Rust default of this field, see [below][defaults]   |

[defaults]: #leaving-fields-out

```crol
// example.crod
+web: {
    port: 8080,
    url: "http://web:" + &self.port,
    health: {
        probe: "http://localhost:" + &parent.port,
    },
}
+proxy: {
    upstream: &root.web.url,
}
```

```console
$ crol eval example.crod
web: {
    port: 8080,
    url: "http://web:8080",
    health: { probe: "http://localhost:8080" },
}
proxy: { upstream: "http://web:8080" }
```

The order of entries does not matter for these references: they read the
_final_ value, wherever it was written. `&` is the exception, and reads what
came _before_ this entry. [Layers](layers.md#early-and-late-references)
explains why that difference is the heart of CROL.

There is no implicit lookup. `&port` never means "`port` from somewhere
around here". It names a bind, or it is an error that suggests `&self.port`.

### The root entry you are in

`&root` is the whole document, so from inside `web` you would write
`&root.web.port`. `&top` says the same without naming `web`: it is the root
entry the block is in, however many blocks down, and also when the target has
several steps:

```crol
// example.crod
+web: {
    host: "web",
    port: 8080,
    limits: { cpu: 2, budget: &top.limits.cpu * 100 },
}
web.limits.probe: { url: "http://" + &top.host + ":" + &top.port }
```

```console
$ crol eval example.crod
web: {
    host: "web",
    port: 8080,
    limits: { cpu: 2, budget: 200, probe: { url: "http://web:8080" } },
}
```

There, `&parent` would be the root, since `web.limits.probe: { ... }` is one
block. In a template, `&top` is the root entry the template is spread into. At
the top level there is no root entry around, and `&top` is an error.

### Binds

If counting `&parent`s gets tiresome, give an entry a name with `name =`, and
use it from any block nested inside:

```crol
// example.crod
+web: {
    port = port: 8080,
    health: {
        probe: {
            url: "http://localhost:" + &port,
        },
    },
}
```

`name := value` binds a name to a value that is not stored under any key:

```crol
// example.crod
+web: {
    base := 8000,
    port: &base,
    admin_port: &base + 1,
}
```

```console
$ crol eval example.crod
web: { port: 8000, admin_port: 8001 }
```

## Templates and spreads

At the top level, `name := value` makes a _template_: a value that belongs to
no key, for other values to be built from. `..` _spreads_ a template into a
block, copying its entries in. Entries you write yourself win over spread
ones, wherever you put them:

```crol
// example.crod
service_defaults := {
    replicas: 1,
    port: 8080,
    url: "http://localhost:" + &self.port,
}

+web: {
    ..&service_defaults,
    replicas: 3,
}

+admin: {
    ..&service_defaults,
    port: 9000,
}
```

```console
$ crol eval example.crod
web: { replicas: 3, port: 8080, url: "http://localhost:8080" }
admin: { replicas: 1, port: 9000, url: "http://localhost:9000" }
```

Look at `admin.url`. The `&self.port` in the template was evaluated where it
landed, so it read `admin`'s port. A template can describe its fields in terms
of each other, and every value that spreads it gets them computed from its own
fields.

Spreads merge like `~` does: a block after a spread merges with what the spread
put there, so `combat: { range: 7 }` keeps the other fields of `combat`. Write
`^combat: { ... }` to replace it.

A template can also be a patch, for changes that several values share. `&` in a
template has nothing below it where it is written, so it reads the value below
where it lands:

```crol
// example.crod
stronger := { hp: & + 5, tags: & + ["strong"] }

+trooper: { hp: 10, tags: [] }
+tank: { hp: 100, tags: ["big"] }

trooper: & ~ &stronger
tank: & ~ &stronger
```

```console
$ crol eval example.crod
trooper: { hp: 15, tags: ["strong"] }
tank: { hp: 105, tags: ["big", "strong"] }
```

Templates are not data: they do not show up in the output, and your program
never sees them. In layered data they are visible in every file of every
layer, and can be defined only once.

### Giving a template a type

A template written as `Service { ... }` has a type your editor can check and
complete. A map, a list, a tuple or a number has no name to write, so put
`#[type(...)]` before the template instead. It takes a Rust type, or a path
into one of your schema's types: `.field` for a field, `.0` for an element of a
tuple, and `.[]` for an element of a list or a value of a map.

```crol
// example.crod
#[type(Fleet.services)]                // the type of Fleet's field `services`
base_services := { web: { replicas: 2 } }

#[type(Fleet.services.[].ports.[])]   // one element of a service's ports
http := { port: 80, protocol: "tcp" }

#[type(HashMap<String, u16>)]
ports := { http: 80, https: 443 }
```

A path follows your Rust types when they change, so prefer it for a template
meant for one field. The attribute is only for tools: `crol check` and your
editor check the template's fields and values against the type, and
evaluation ignores it. See [Schemas](schemas.md#checking-templates).

## Leaving fields out

When CROL loads a struct, **every field must be written**, `Option` fields
included. That is on purpose: a field that goes missing because of a typo
should not silently become its default.

When you do mean to leave fields out, say so with `..` at the end of the
block. Then serde decides, as usual: `Option` fields become `None`, and fields
with `#[serde(default)]` take their default. A field with neither is still an
error.

```crol
// example.crod
+web: Service {
    port: 8080,
    ..,
}
```

The same goes for [a struct at the root](rust.md#a-struct-at-the-root): write `..` at
the top level of any layer.

A field removed with `-field` also takes its default, without `..`. Use it to
drop a value that a lower layer or a template set. With `-?field`, the field takes
its default even if nothing set it:

```crol
// example.crod
base := { port: 80, limits: { cpu: 1.0 } }

+web: {
    ..&base,
    -limits,
}
```

`&default` reads the default itself, so you can compute with it:

```crol
+web: Service {
    replicas: &default * 2,
    limits: &default ~ { cpu: 2.0 },
    ..,
}
```

`&default` needs to know your types, so it works only when your program hands
the composer a schema. [Schemas](schemas.md#using-default) has the details and
the current limits.

## Arithmetic and strings

| Operands                  | Operators                                         |
| ------------------------- | ------------------------------------------------- |
| integer, float            | `+`, `-`, `*`, `/`, `%`, and `-x`                 |
| string, string            | `+` concatenates                                  |
| string, number/char/bool  | `+` formats the value into the string             |
| list, list                | `+` concatenates, `-` removes the elements on the right, `\|` adds those it does not hold yet |
| boolean                   | `!x` inverts it                                   |

An integer and a float together count as floats. Integer division truncates.
Overflow and division by zero are errors, not surprises.

```crol
// example.crod
+memory: 512 * 4
+half: 7 / 2
+ratio: 7 / 2.0
+label: "port " + 8080
+tags: ["a", "b", "c"] - ["b"] + ["d"]
```

```console
$ crol eval example.crod
memory: 2048
half: 3
ratio: 3.5
label: "port 8080"
tags: ["a", "c", "d"]
```

Lists only combine with lists, so to append one element, write
`tags: & + ["x"]`. To add it only if the list does not hold it yet, write
`tags: & | ["x"]`: each element on the right that is not equal to one in the
list is added at the end, once.

```crol
// example.crod
+tags: ["web", "public"]
tags: & | ["public", "tls", "tls"]
```

```console
$ crol eval example.crod
tags: ["web", "public", "tls"]
```

Multiplying an integer by a float gives a float, which an integer field does not
accept. Four built-in macros bring a number back, or keep it in range:

| Macro                 | Gives                                       |
| --------------------- | ------------------------------------------- |
| `round!(x)`           | the nearest integer, ties away from zero    |
| `floor!(x)`           | the largest integer not above `x`           |
| `ceil!(x)`            | the smallest integer not below `x`          |
| `clamp!(x, min, max)` | `x`, kept between `min` and `max`           |

```crol
// example.crod
+hit_points: 1100
hit_points: round!(& * 1.15)
+armour: clamp!(12 - 20, 0, 10)
+speed: clamp!(2.5, 0, 1.5)
```

```console
$ crol eval example.crod
hit_points: 1265
armour: 0
speed: 1.5
```

The operators bind in the usual order: selectors, unary `-` and `!`, `* / %`, `+ - |`,
then `~` and `~^`, then `??`. Parentheses group.

## Fallback

`a ?? b` is `a`, unless a value in `a` is missing, in which case it is `b`:

```crol
// example.crod
+settings: { timeout: 30 }
+timeout: &root.settings.timeout ?? 60
+retries: &root.settings.retries ?? 3
counter: & + 1 ?? 1
```

```console
$ crol eval example.crod
settings: { timeout: 30 }
timeout: 30
retries: 3
counter: 1
```

The last line counts: it adds one if a lower layer wrote `counter`, and starts
at 1 if not.

`??` catches only missing values. Other errors, such as adding a string to a
list, are still reported.

## Ids

An id, written `@web`, is a value that names a key. Use one wherever one
definition points at another:

```crol
// example.crod
+database: { port: 5432 }
+web: { depends_on: [@database] }
```

An id is not a lookup: it evaluates to the name, and your program decides what
to do with it. What ids buy you is checking: `crol check`, your editor and your
program report ids that name nothing. Declare the field as
[`crol::Id`](rust.md#ids), which takes only an id.

### Scopes

Without anything else, an id names a root key. Data that is grouped can make a
block a **scope**: inside it, `@db` names the block's own `db` if it has one,
and otherwise looks further out, up to the root.

```crol
// services.crod
db: { host: "prod-db" }
environments: {
    #[scope]
    staging: {
        db: { host: "staging-db" },
        web: { depends_on: [@db] },    // staging's db
    },
    prod: {
        web: { depends_on: [@db] },    // the root's db
    },
}
```

`#[scope]` goes before an entry or a template. A template marked `#[scope]`
makes a scope of every block it is spread into. Your program can also mark a
type as a scope, so that every value of it is one, with
[`#[crol(scope)]`](schemas.md#attributes): then every `Screen` in a file is
a scope, without writing `#[scope]` on each.

An id resolves where its value ends up. An id in a template names something in
each block the template is spread into:

```crol
// screens.crod
pane := { layout: [@filter] }
#[scope]
dashboard: { ..&pane, filter: { placeholder: "Filter workers" } }
#[scope]
settings: { ..&pane, filter: { placeholder: "Search settings" } }
```

Here the `@filter` written once names `dashboard.filter` in one place and
`settings.filter` in the other.

An id can also reach into a scope, or skip the scopes around it:

| Id                   | Names                                                     |
| -------------------- | --------------------------------------------------------- |
| `@filter`            | `filter` in the innermost scope that has one, else the root's |
| `@settings.filter`   | `settings`, found the same way, then its key `filter`    |
| `@::filter`          | the root key `filter`, whatever the scopes around say    |

An id names keys only: `@items[0]` and `@pair.0` are errors.

### Ids as keys

A map whose keys name other entries has ids as its keys:

```crol
// units.crod
+is_biological: Tag {}
+armored: Tag {}

+toxin_grenade: Weapon {
    damage: {
        amount: 23.0,
        bonus: { @is_biological: 47.0, @armored: 5.0 },
    },
}
```

`bonus` is a `HashMap<crol::Id, f64>` in Rust: a map keyed by
[`crol::Id`](rust.md#ids) takes only id keys, so that `is_biological: 47.0`
there is an error, and no other map takes them. An id key has every form an
id has, `@a.b: 1` and `@::a: 1` too, and names things the same way, through
the scopes around the map it is in.

Two id keys are the same key when they name the same entry, however they are
written. A later layer changes the bonus above with any of these:

```crol
// balance.crod
toxin_grenade.damage.bonus.@is_biological: 50.0
toxin_grenade.damage.bonus[@::is_biological]: 50.0
toxin_grenade: & ~ { damage: { bonus: { @::is_biological: 50.0 } } }
```

In a path, an id key goes after a dot or in brackets. The steps after an id
without brackets belong to the id, `@a.b` being one id, so write the brackets
to go on below it: `bonus[@armored].factor: 2`. A reference reads an entry the
same way, `&root.toxin_grenade.damage.bonus[@armored]`.

Since keys are compared by what they name, an id key has to name something:
one that names nothing is an error, where an id value that names nothing is a
warning. If the keys of a map can name things that are not in the data, make
them strings. In a scope, `@armored` and `@::armored` are two keys when the
scope has an `armored` of its own, and one key when it has not.

## Attributes

A document can start with attributes, which say something about the file as a
whole:

| Attribute                       | Says                                         | Read more                                      |
| ------------------------------- | -------------------------------------------- | ---------------------------------------------- |
| `#![schema(...)]`               | which schema tools check this file against   | [Schemas](schemas.md#how-schemas-are-found)    |
| `#![extends(path)]`             | which files this one is written on top of    | [Layers](layers.md#declaring-what-a-file-builds-on) |
| `#![use(path as Alias)]`        | which type a name means                      | [Schemas](schemas.md#types-with-the-same-name) |
| `#![type(Name)]`                | the type of a whole value-mode document      | [Modes](modes.md)                              |

Two attributes go on an entry instead of a document: `#[type(...)]`, which
gives a template its type (see [Giving a template a type](#giving-a-template-a-type)),
and `#[scope]`, which makes the entry's value a scope for ids (see
[Scopes](#scopes)).

## What is not in the language

CROL is data with a little arithmetic, not a programming language. Apart from the
[built-in macros](#arithmetic-and-strings), there are no functions, and there are
no conditionals, comparisons, loops or imports, and a file cannot
read the environment, the clock or other files. That is what makes evaluation
predictable: the same files give the same data, on every machine.

- For values that differ per environment, use [variants](variants.md).
- For values that come from outside, such as secrets, have your program fill
  them in after loading, or generate a layer.
- For validation, such as ranges or rules across fields, use Rust: wrapper
  types with their own `Deserialize`, or a check after loading.

## Next

[Data mode and value mode](modes.md) explains `.crod` and `.crol` files.
