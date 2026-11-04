# CROL: Composable Rust Object Language — Specification

Status: draft 1 (2026-11-03). Appendix A records the design decisions
made while writing it.

CROL is a text format for configuration and data files that load into
Rust data models through serde. It looks like RON, but a document holds
*operations* as well as values: it can declare, replace, patch and remove
definitions, including ones written in other files and in lower-priority
*layers*. Many files, from many sources, combine into one data set
with fixed, predictable rules.

The key words *must*, *must not*, *should* and *may* are used in their
RFC 2119 sense.

---

## 0. Goals and non-goals

**Goals**

1. Data split over many files and layers (base data, extensions,
   local overrides) combines into one data set without code.
2. Every change says what it means: declare, replace, patch, remove,
   or default, and each is checked.
3. It maps onto ordinary `serde` types. No schema language; loading
   needs no derive of its own, except for `&default` (tools use an
   optional schema derive, section 10.9).
4. Evaluation is deterministic across platforms, so two machines that
   load the same sources get bit-identical data and fingerprints.
5. It works well in editors: a lossless syntax tree, precise spans,
   and for every value the list of operations that produced it.

**Non-goals**

- A programming language. CROL has no functions, conditionals,
  comparisons, loops, imports or I/O. Every expression is pure
  arithmetic and structural combination.
- A schema language. The format has no types. Types, defaults,
  required fields and the meaning of ids come from the Rust program
  (section 10), and for tools from a schema generated from it (10.9).
- Validation. Ranges, cross-field rules and the like are checked on the
  Rust side, by wrapper types or derive attributes. Data that needs
  logic to be valid is better written as code.
- Writing back edited source while keeping layout (only the formatter
  rewrites files).

---

## 1. Overview by example

Three layers, loaded in this order: `base`, `extension`, `local`.
`<dev>` and `<prod>` are *variants* the application declared.

`base/services.crol`:

```rust
// A template: a named value, not a root key (section 6.1.2).
service_defaults := {
    replicas: 1,
    port: 8080,
    tags: ["managed"],
    env: { LOG_LEVEL: "info" },
    limits: Limits { cpu: 1.0, memory_mb: 512 },
},

+web: Service {
    ..&service_defaults,              // copy the template's fields in
    image: "web:1.4",
    depends_on: [@database],          // an id: names another key
    url: "http://web:" + &self.port,  // read late: the final web.port
},

+database: Service {
    ..&service_defaults,
    image: "postgres:17",
    port: 5432,
    depends_on: [],
    ..,                               // url and anything else: defaults
},

<prod> {
    web.replicas: 4,
    database.limits.memory_mb: & * 4,
},
```

`extension/cache.crol`:

```rust
+cache: Service {
    ..&service_defaults,
    image: "valkey:8",
    port: 6379,
    depends_on: [],
    ..,                               // fields left out take their defaults
},

web: & ~ {
    depends_on: & + [@cache],
    env.CACHE_URL: "redis://cache:" + &root.cache.port,
},

web.limits.memory_mb: 1024,
```

`local/overrides.crol`:

```rust
web.env.LOG_LEVEL: "debug",
web<dev>.replicas: 1,
-web<dev>.limits,                     // web takes the default `Limits` in dev
```

Result, as `crol eval --context prod web` prints it:

```rust
web: Service {
    replicas: 4,
    port: 8080,
    tags: ["managed"],
    env: {
        LOG_LEVEL: "debug",
        CACHE_URL: "redis://cache:6379",
    },
    limits: Limits { cpu: 1.0, memory_mb: 1024 },
    image: "web:1.4",
    depends_on: [@database, @cache],
    url: "http://web:8080",
},
```

Note how `url` reads the final `port`, because late references read the
*final* value (section 7.3), and how `extension` patches `web` after
`base` wrote it. `database<prod>` has `memory_mb: 2048`: its `& * 4`
runs after the base chain is complete.

---

## 2. Lexical structure

### 2.1 Source text

- Source is UTF-8. A leading byte order mark is ignored. A file that is
  not valid UTF-8 is an error.
- Line ends are `\n` or `\r\n`. Columns in diagnostics count Unicode
  scalar values from 1; tools that need UTF-16 columns convert.
- Whitespace is space, tab, `\r` and `\n`. Whitespace separates tokens
  and is otherwise meaningless, except where section 2.5 says a token
  must follow another directly.

### 2.2 Comments

`// ...` to the end of the line, and `/* ... */`, which nest, as in Rust.
Doc comments (`///`, `//!`) are ordinary comments. Comments are kept in
the syntax tree (section 12.1).

### 2.3 Identifiers and keywords

```
identifier ::= [A-Za-z_][A-Za-z0-9_]*
```

A lone `_` is not an identifier. Identifiers are case-sensitive.

- `true` and `false` are reserved everywhere.
- `use`, `as`, `schema`, `type`, `extends` and `super` are keywords
  only inside a document attribute (sections 4.1, 4.2).
- `self`, `parent`, `root` and `default` are keywords only directly
  after `&` (`&self`, `&parent`, `&root`, `&default`). Everywhere else
  they are ordinary identifiers, so a field may be called `parent` or
  `root`. A
  self-relative reference to such a field is written `&self.parent`.

Raw identifiers (`r#type`) are not supported. Rust keywords other than
the above are ordinary CROL identifiers (`type: "x"` is a field named
`type`; serde sees `type`).

### 2.4 Literals

| Kind | Syntax |
|---|---|
| integer | `12`, `0x1F`, `0o17`, `0b1010`, `1_000`. No suffixes (`1u8` is an error). |
| float | `1.5`, `1e3`, `2.5E-3`, `1_000.0`. A digit is required on both sides of `.` (`1.` and `.5` are errors). No `inf` or `NaN`. |
| string | `"text"` with Rust escapes (`\n \r \t \\ \0 \" \' \x7F \u{1F600}`, and `\` + newline to skip leading whitespace on the next line); raw `r"..."`, `r#"..."#`, any number of `#`. Strings may span lines. |
| char | `'a'`, `'\n'`, `'\u{E9}'`, as in Rust. |
| boolean | `true`, `false` |

A `-` before a number is the unary operator; section 6.2 says when the
result counts as a literal.

### 2.5 Punctuation and adjacency

```
{ } [ ] ( ) < > , : :: := . .. + - * / % ~ | & ^ @ ? = # !
```

Two adjacency rules, both to keep one-token lookahead:

- In a reference, a name or keyword must follow `&` directly:
  `&port`, `&root`, `.&parent`. `& port` is an error ("a reference name must follow
  `&` directly"); `& + 1` is `&` plus 1.
- In an id, the identifier or string must follow `@` directly.

`..` is one token; `...` is an error.

---

## 3. Values

The evaluated data consists of these values:

| Value | Written as | Notes |
|---|---|---|
| boolean | `true` | |
| integer | `12`, `-3` | 128-bit signed during evaluation; range-checked against the Rust type on deserialization |
| float | `1.5` | IEEE 754 binary64; always finite |
| char | `'a'` | |
| string | `"a"` | |
| id | `@web`, `@"odd id"` | a key of the root, stored as a value (section 3.2) |
| list | `[a, b]` | |
| tuple | `(a, b)`, `(a,)`, `()` | `()` is the unit value; `(a)` is `a` |
| unit name | `None`, `Red`, `Marker` | a unit enum variant or unit struct |
| named tuple | `Some(1)`, `Rgb(1, 2, 3)` | a tuple or newtype variant, or a tuple or newtype struct |
| keyed value | `{ ... }`, `Service { ... }` | a struct, struct variant or map (section 3.1) |

The format does not distinguish a struct from an enum's struct variant,
or a unit struct from a unit variant: both are a name with a payload.
Which one it is, the Rust type decides when it is deserialized.

### 3.1 Keyed values

A keyed value has:

- an optional **type name**;
- **entries** from keys to values, in insertion order. Order is preserved and
  deterministic (section 7.6). Setting an existing key keeps its
  position; removing a key and adding it again moves it to the end;
- a **defaults flag**, set by `..`: fields it does not hold take their
  defaults (section 10.3);
- a set of **defaulted fields**: fields removed with `-field`, which
  take their defaults even without the flag.

Structs and maps are the same thing in CROL: one notation, one kind of
value. Whether a keyed value becomes a struct or a map is the Rust
type's business (section 10).

**Keys** are strings or integers.

| Key | In an entry | In a path |
|---|---|---|
| a string that is an identifier | `port: 80` or `"port": 80` | `.port` or `["port"]` |
| any other string | `"cache url": 1` | `["cache url"]` |
| a non-negative integer | `12: "a"` | `[12]` |
| a negative integer | `[-3]: "a"` (section 5.1) | `[-3]` |

`port`, `"port"`, `.port` and `["port"]` all name the same key. A string
key and an integer key are never equal: `"12"` and `12` are different
keys. The formatter writes string keys bare when they are identifiers,
and paths with `.name` when it can.

Map keys of other Rust types are written as strings or integers: a unit
enum variant by its name (`Red: 1` for a `HashMap<Color, _>`), an id as
its text (section 10.5). Boolean, char, tuple and other keys are not
supported.

### 3.2 Ids

An id `@x` is the root key `x` as a value. It is not a lookup: the
deserializer stores the string `"x"` and reports the use to the
application, which checks that `x` exists and is the right kind of
thing (section 10.5). `@"any text"` writes ids that are not
identifiers.

### 3.3 Equality

Values are equal when they are the same kind and structurally equal.
Integers and floats are never equal to each other. Floats compare as
IEEE numbers (`0.0 == -0.0`). Keyed values are equal when their type
names and entries (as sets of key and value) are equal; entry
order, the defaults flag and defaulted fields are ignored. This equality
is what list `-` uses.

**Identity** is stricter: values are identical when they are equal and
also have the same entry order, defaults flags and defaulted fields, and
floats the same bits. Identity decides whether a context's value
"differs from the base" (sections 7.6 and 10.7, appendix B), since a
different flag or order changes how a value deserializes or is
fingerprinted.

---

## 4. Documents

A document is a file or an in-memory text. It is read in one of two
modes, which the application chooses.

**Data mode** (layered data, section 7). The document is a list of
entries operating on the *root*. The outer braces may be left out:

```rust
+a: 1,
+b: 2,
```

and

```rust
{
    +a: 1,
    +b: 2,
}
```

are the same document. A type name on the outer braces, a spread or
`..` at the root, or a document that is not entries is an error.

**Value mode** (`crol::from_str`). The document is one expression: the
whole file is a single value, like a RON file. If the document parses
as entries, it is the keyed value they build (outer braces optional,
an optional type name allowed). `&root` refers to the document's own
value. There are no layers; `&` at the top level is missing.

The parser decides between entries and an expression by the first
tokens: a document is entries when it is empty or begins with `+`,
`?`, `..`, `<`, an identifier followed by `=` or `:=` (a bind), `-`
followed by an identifier, string or `[`, or a target followed by `:`.
Otherwise it is an expression.

### 4.1 Document attributes

A document may start with **attributes**, before its first entry or
expression (comments may come before them):

```rust
#![use(app::net::Config, app::db::Config as DbConfig)]

listener: Config { port: 8080 },
database: DbConfig { url: "postgres://db" },
```

There are four attributes, `use`, `schema`, `type` and `extends`; any
other is an error, so that others can be added later. Several `use` and
`extends` attributes, and several paths in one `use`, are allowed; the
others appear at most once. Attributes apply in both data and
value mode, to the whole document.

**`use(path)`** says which schema type a type name in this document
means, when the schema has several types of that name (section 10.9). A
path is a Rust module path followed by the type's name *as data writes it*
(its serde name): `app::net::Config`. For the loader, a `use` without
`as` changes nothing; tools use it to resolve the name, and report a
path that names no schema type.

A generic type is written with its arguments, as in the schema file
(section 10.10): `#![use(app::Pair<u32, String>)]`.

**`use(path as Alias)`** also lets the document write `Alias` for that
type. The loader replaces `Alias` by the path's last segment wherever a
name is written (a block's type name, a named tuple, a unit name), so
`DbConfig { ... }` above is checked as `Config` by the deserializer.

**`schema(...)`** names the schema that tools check this document
against, as one of:

| Form | Schema |
|---|---|
| `::crol::schema` | a file, by module path (section 4.2) |
| `"https://example.org/app/v2/schema.crol"` | a file fetched over HTTPS |
| `"crate:app@2.1"` | the schema a crate publishes, for a version requirement as in `Cargo.toml` (`@` and the requirement may be left out: any version) |

A string that starts with neither `https://` nor `crate:` is an error.

A crate publishes its schema by including the file in the package and
naming it in its `Cargo.toml`:

```toml
[package.metadata.crol]
schema = "crol/schema.crol"   # the default when the table exists
```

(This path is Cargo's, relative to the package, like `include`; it is
not a CROL module path.)

Tools resolve `crate:app@2.1` in order: a package of that name and a
matching version in the current Cargo workspace or its dependencies (by
`cargo metadata`); the same in the local Cargo registry cache
(`~/.cargo/registry/src`); the package downloaded from crates.io, highest
matching version. A crate-based schema suits applications installed
with `cargo install` and libraries whose types appear in other
programs' configuration.

```rust
#![schema("https://example.org/app/v2/schema.crol")]

Config {
    listen: "0.0.0.0:8080",
}
```

- It is for tools only; the loader ignores it, since the application
  has the types.
- It is mainly for documents outside a project: a configuration file
  that users of an application edit, with nothing but the file.
- **The root type.** A schema's root type means, in value mode, the
  type of the whole document (unless the document has a top-level type
  name), and in data mode, the type of each root value.
- For a document in a project, it overrides the project file's schema
  for that document. In a layer, every document should agree with the
  project's schema; tools warn about one that names another.
- A remote schema (a URL, or a crate downloaded from crates.io) is
  fetched over HTTPS only, cached, and used from the cache when
  offline. The language server's `remote_schemas` setting
  (default on) and `crol --offline` turn fetching off; cached copies
  and local crates are still used.
- **The cache** is `CROL_CACHE_DIR` if set, else `crol` in the platform's
  cache directory (`$XDG_CACHE_HOME` or `~/.cache`, `~/Library/Caches`,
  `%LOCALAPPDATA%`). A URL is fetched again once per process (each
  `crol` run, each language server session); when that fails, the
  cached copy is used. A crate's schema is downloaded once per version,
  since published versions do not change; only the version list is
  fetched again. Only the schema file is kept, never the package.
- Fetching needs a feature of `crol`: `remote-schemas` fetches with a
  blocking client (`schema::resolve_remote`), `remote-schemas-async`
  with Tokio and reqwest (`schema::resolve_remote_async`, and
  `resolve_remote` on a runtime of its own). `crol-cli` and `crol-lsp`
  enable the async one by default. The language server never waits for
  a fetch: it evaluates with the cache, says the schema is being
  fetched, and evaluates again when it arrives.
- At most one `schema` attribute per document.

**`type(Name)`** gives the type name of a value-mode document written as
entries, which has no outer block to carry one:

```rust
#![type(Config)]

port := 8080,
listen: "0.0.0.0:" + &port,
admin: "127.0.0.1:" + (&port + 1),
```

It is the same as writing the entries inside `Config { ... }`: the name
is resolved like any type name (`use` applies), and the deserializer
checks it (section 10.2). It is an error in data mode, where the root
has no type, and in a document that is an expression or a named block.

**`extends(path)`** names the documents this one is written on top of:

```rust
#![extends(super::defaults::services)]

web.replicas: 4,
```

- The module path (section 4.2) names a file or a directory. Each
  `extends` is a layer below this document, in the order written; an
  extended document's own `extends` come below it. A document that is
  already loaded is not loaded again. A cycle is an error.
- **Tools** always follow it: they evaluate the document on top of those
  layers, so the editor shows the values below, the merged results, and
  the templates and keys the document can use. Diagnostics in extended
  documents are not reported, only in the document itself.
- **The loader** follows it only with `CrolConfig::load_extends`
  (section 10.7); by default it ignores it, and the application decides
  what is loaded and in which order. Either way `extends` only adds
  layers below: it is not an import, and makes nothing visible that
  layering does not.
- It is an error in value mode, which has no layers.
- In a project, the project file's layers take precedence; tools warn
  when an `extends` names something that is not a lower layer.

Errors: two `use`s that give one name to different types; an alias that
is a reserved word; more than one `schema` or `type`; `type` in data
mode, on a document that has a type name, or on an expression; `extends` in value mode; a
cycle of `extends`; an attribute after the first entry or expression.

### 4.2 Module paths

Documents and code name files with **module paths**, as Rust names
modules, never with file system paths, so they read the same on every
platform.

| Path | Starts from |
|---|---|
| `::defaults::services` | the **root directory**: `CrolConfig::root_path` (section 10.7); for tools, the project file's directory, else the working directory (CLI) or the workspace folder (language server) |
| `defaults::services` | the directory of the document it is written in; in code, the root directory |
| `super::defaults::services` | the parent of that directory; `super` may repeat (`super::super::x`), only at the start |

Every segment but the last names a directory. The last names the file
`services.crol` or the directory `services/` (every `.crol` file below
it, as section 7.1 reads directories). Errors: both exist; neither
exists; `super` above the root directory. A file or directory whose name
is not an identifier (`my-file.crol`) or is hidden cannot be named by a
module path; code can still load it by a file system path.


---

## 5. Entries

An entry is an operation on a **target**: a key, followed by an
optional path into its value.

| Entry | Name | Meaning | Error when |
|---|---|---|---|
| `+target: expr` | declare | create the target | it exists |
| `target: expr` | set | replace the target, or create it | |
| `?target: expr` | ensure | create the target if missing; otherwise nothing | |
| `-target` | remove | remove the target | it does not exist |
| `<c> { entries }` | variant block | the entries apply under condition `c` (section 8.2) | |
| `..operand` | spread | copy the keyed value's entries in | it is not a keyed value |
| `..` | defaults | fields left out take their defaults (section 10.3) | |
| `name = entry` | bind | names the entry's target (section 6.1.1) | |
| `name := expr` | value bind | names a value; at the top level, a template (section 6.1.2) | defined twice |

### 5.1 Targets and paths

A target is a key (section 3.1) followed by **selectors**:

- `.name` — the string key `name`;
- `.0`, `.1`, … — a tuple element, or an element of a named tuple's
  payload (`Rgb(1, 2, 3)`); `.0` is the first element. On a keyed
  value, `.0` is an error: write `[0]`;
- `[key]` — the accessor, with a string or integer inside: any key
  (`["cache url"]`, `[12]`, `[-3]`), or on a list, an index (a
  non-negative integer). Whether `[3]` is a key or an index depends on
  the value it is applied to. An identifier in brackets (`[port]`) is
  an error, since a bare identifier is never a lookup: write `.port`;
- `<c>` — a variant condition (section 8.2). Several in one path combine
  with `&`.

A target may also start with an accessor, which then applies to the
keyed value the entry is in. This is needed only for negative integer
keys (`[-3]: "a"`, `-[-3]`), since `-3: "a"` would read as removing
key `3`.

**Root keys** are strings (the text an id `@web` names): `web: ...` or
`"odd id": ...` at the root, and `&root.web` or `&root["odd id"]` to
reach them. An integer key at the root is an error.

**Path entries.** For a target with selectors, every step but the last
must exist; a missing step is an error naming it. The operation then
applies at the last step:

- set, declare and ensure work on keys. On list indices and tuple
  elements only set works, and the index must exist. Whether a key is a
  known field of a struct, the deserializer decides later.
- remove works on keys, and on list indices (later elements move down).
  Removing a tuple element is an error.

A path entry is always a patch: `web.replicas: 4` needs `web` to exist.

### 5.2 Blocks

A block `Name? { entries }` builds a keyed value in three steps:

1. Start from the block's **start value**: the left operand when the
   block is the right side of `~` (a *patch block*), and an empty keyed
   value otherwise (a *whole-value block*).
2. Apply every spread, in source order, wherever it is written. A
   spread sets each of the source's entries (as `key: value`), ORs its
   defaults flag into the block's, and adds its defaulted fields to the
   block's. A spread's operand is a postfix expression (section 9):
   `..&root.x`, `..{ ... }`, or in parentheses, `..(&root.a ~ { b: 1 })`.
3. Apply the other entries in source order. `..` sets the defaults
   flag, wherever in the block it is written.

So explicit entries always win over spreads; `+field` fails if a spread
supplied `field`; `?field` keeps a value that a spread supplied.

`-key` removes the entry and adds the key to the defaulted fields, so a
struct field takes its default. Setting the key again (`+key`, `?key`,
`key`) takes it out of the defaulted fields.

Entries inside a block may have paths (`limits.cpu: 2.0`), which
navigate into what the block holds at that point.

**Merging is shallow.** In a patch, `limits: { cpu: 2.0 }` *replaces*
`limits`. To change one field deep inside, write a path
(`limits.cpu: 2.0`) or patch the nested value explicitly
(`limits: & ~ { cpu: 2.0 }`).

**Type names.** A block's type name is checked against the start value
when the block is a patch (section 6.4), and against the Rust type at
deserialization (section 10.2). A block without one takes the start
value's name.

### 5.3 Separators

Entries and elements are separated by commas. A trailing comma is
allowed. A missing comma between two entries is an error.

---

## 6. Expressions

### 6.1 References

| Reference | Value | Timing |
|---|---|---|
| `&` | the value *below*: the current target's value before this entry applies | early |
| `&self` | the keyed value this entry is in | late |
| `&parent` | the keyed value around `&self`; `&parent.&parent` the one around that, and so on | late |
| `&root` | the root | late |
| `&default` | the Rust default of the current target (section 6.1.3) | fixed |
| `&name` | a *bind* or *template* (sections 6.1.1, 6.1.2) | late |

Any reference takes selectors: `&[0]`, `&.limits.cpu`, `&self.port`,
`&root.web<prod>`. `&.port` is the field `port` of the value below;
`&self.port` is the field `port` of the value this entry is in.

There is no implicit lookup: `&port` is never "`port` from some
enclosing scope". It names a bind or is an error, with a hint
("no bind `port` is visible; did you mean `&self.port`?").

`.&parent` is allowed only directly after `&parent` or another
`.&parent`: `&parent.&parent.port`. Elsewhere (`&self.&parent`,
`&root.web.&parent`) it is an error.

**`&` binding.** Every `&` belongs to the innermost entry containing it,
and is that entry's value below:

- for a root entry, the value of the target from earlier operations
  (section 7);
- for an entry inside a block, the block's *current* value at that
  target, at the moment the entry applies: what the start value,
  spreads and earlier entries have put there;
- for an entry inside a whole-value block that is itself an entry's
  value, the start value is empty, but `&` in its entries still sees
  through to the enclosing entry's value below. So in
  `web: Service { replicas: & + 1, .. }`, `&` is the old `web.replicas`.
  Precisely: an entry's value below is the block's current value at
  that key if it exists there, else the *outer value below* at that
  key, where a whole-value block's outer value below is the value below
  of the entry it is the value of (and nothing if it is elsewhere, such
  as in a list).

`&` with nothing below is *missing* (section 6.5).

**Self and parent.** Every block has a **location**: the path where its
value ends up.

- A block that is an entry's value, or an operand of `~` or `|` there,
  is at the entry's target.
- A list element is at `[i]`; a tuple or named-tuple element is at `.i`;
  a spread's operand is at the location of the block containing it.
- A block that is a value bind's expression (section 6.1.2) is at a
  location of its own, the bind; `&parent` there is above the top level.
- A block anywhere else (an operand of `+`, for instance) has no
  location, and `&self`, `&parent` or a bind in its entries is an error.

A block has a **fixed location** when it is an entry's value or the
right side of `~` there, or a field value nested in such a block. List
elements, `|` operands and spread operands have a location, but not a
fixed one, because whether and where their entries end up depends on
evaluation. Variant entries need a fixed location (section 8.2).

`&self` is the final value at the location of the innermost block
containing the entry; for an entry at the document's top level, `self`
is the root. `&parent` is the final value at the location of the next
block out, `&parent.&parent` the one after that; going above the top
level is an error.

**Rebinding through spreads.** An entry that a spread copies in is
evaluated at its *new* location. `&self`, `&parent` and binds in it,
and in the blocks nested in it, refer to the value it was copied into,
not to the one it was written in. `&` does not move: in a copied entry it
is still the value below where the entry was written (so `replicas: & +
1` in a template adds to the template's own value below, which in a
template is missing, section 6.1.2). `&root` paths are absolute either
way.

```rust
service_defaults := {
    port: 80,
    url: "http://localhost:" + &self.port,
},
+web: Service {
    ..&service_defaults,
    port: 8080,
    ..
},
```

`web.url` is `"http://localhost:8080"`; the template's own `url` stays
`"http://localhost:80"`. This is what makes templates work: a template
describes its fields in terms of each other, and every value that
spreads it gets them computed from its own fields.

Locations are relative: `&parent.&parent` and binds count blocks up from
the entry, so a reference keeps pointing at the same relative place
wherever its entry is copied. Evaluation is memoized per location and
context, not per source expression (section 7.5).

#### 6.1.1 Binds

A bind names an entry, so that nested blocks can refer to it without
counting levels:

```rust
service_defaults := {
    port = port: 80,
    health: {
        probe: { url: "http://localhost:" + &port },
    },
},
+web: Service {
    ..&service_defaults,
    port: 8080,          // web.health.probe.url is "http://localhost:8080"
    ..
},
```

- **Syntax.** `name =` before an entry that sets, declares or ensures:
  `port = port: 80`, `p = +port: 80`, `p = ?port: 80`,
  `cpu = limits.cpu: 2.0`. The bind is declared without `&` and used
  with it (`&port`). A bind on a remove, a variant block, a spread or
  `..` is an error.
- **Value.** `&name` is the final value at the bound entry's target,
  read late: the same as `&self.port` written in the block that holds
  the bind. Variant context applies as for any late reference.
- **Scope** is lexical: the block that holds the bind and every block
  nested in it, anywhere in that block (before or after the bind).
  Binds are not visible in other entries' blocks, other documents or
  other layers; use `&root` for those. At the top level of a data-mode
  document, entry binds are an error: `&root.web` already names a root
  entry. At the top level of a value-mode document, a bind is visible
  in that document.
- **Names.** A bind with the same name as another visible one (in the
  same block or an enclosing one), or as a template (section 6.1.2), is
  an error; there is no shadowing. `self`, `parent`, `root` and
  `default` cannot be bind names. A bind name and a key may be the same.
- **Relative.** Like `&parent`, a bind is "*n* blocks up, then this
  target", and follows its entries when a spread copies them
  elsewhere. A value bind (section 6.1.2) is not relative: a copied
  entry that uses one reads the value bound where the entry was
  written.
- **Variants.** A bind on a variant entry (`p = port<prod>: 80`) or
  inside a variant block (`<prod> { p = port: 80 }`) is an error; a
  bind on a base entry is read in the current context.

#### 6.1.2 Value binds and templates

`name := expr` binds a name to a value that is not stored under any
key:

```rust
service_defaults := Service {
    replicas: 1,
    port: 8080,
    limits: Limits { cpu: 1.0, memory_mb: 512 },
},

+web: Service { ..&service_defaults, image: "web:1.4", .. },
```

- **Inside a block**, a value bind is lexical, like an entry bind:
  `{ base := 8000, port: &base, admin_port: &base + 1 }`.
- **At the top level of a data-mode document**, it is a **template**:
  visible in every document of every layer, so a template defined in
  one layer can be used in later ones.
- **At the top level of a value-mode document**, it is visible in that
  document.
- **Defined once.** A value bind cannot be patched, replaced, removed or
  made conditional: a second definition of a template anywhere in the
  data set is an error, as is a value bind inside a variant block. To
  vary a value, vary the root keys that use it.
- **Late.** `&name` reads the bound value in the current context, and
  entries spread from it are evaluated where they land (rebinding,
  above). `&` in a value bind's expression is missing, since there is
  nothing below.
- **Not data.** Templates are not root keys: they are never
  deserialized, never checked against the root type, and not part of
  the fingerprint; only their effect on root values is. Tools check a
  template with a type name for unknown fields and wrong kinds, but not
  for completeness, since templates are usually partial.
- **Names** share the namespace of binds (section 6.1.1).

#### 6.1.3 Defaults

`&default` is the value the target would have by default in Rust (here
for a `Service` with `#[serde(default)]` and `#[crol(defaults)]` on the
struct):

```rust
+web: Service {
    ..&default,                       // every default, written out
    replicas: &default * 2,           // twice the default
    limits: &default ~ { cpu: 2.0 },  // the default limits, one field changed
},
```

- **Which default.** The target's type is found from the nearest
  enclosing type name and the schema: in `web: Service { limits: { cpu:
  &default }, .. }`, it is the default of `Service`'s field `limits`, field
  `cpu`. A field's default is its own `#[serde(default)]` or
  `#[serde(default = "...")]` if it has one, else the field's value in
  the struct's default (`#[serde(default)]` on the struct). At a type
  name itself (`..&default` directly in `Service { }`), it is the
  struct's whole default value. Selectors apply as usual:
  `&default.limits.cpu`.
- **Where defaults come from.** From the schema (section 10.9), which
  records them for types with `#[crol(defaults)]`. The loader needs the
  schema for this: `CrolComposer::schema(crol::schema::build::<Definition>())`.
  Tools read them from the schema file.
- **Missing.** A field without a default, a type without recorded
  defaults, or a target whose type cannot be found makes `&default`
  *missing*, so `&default | 1` falls back (section 6.5). Without a
  schema at all, `&default` is an error.
- **Fixed.** Defaults depend neither on layers nor on the context, so
  `&default` is evaluated once per target and never takes part in a
  cycle.

`..` and `..&default` differ in when defaults are filled in: `..` leaves
fields out and lets serde fill them while deserializing; `..&default`
writes them into the evaluated data, where `crol eval` and the language
server show them.

### 6.2 Operators

From tightest to loosest:

| Level | Operators | Associates |
|---|---|---|
| 1 | postfix `.field`, `.0`, `[key]`, `<v>` | left |
| 2 | unary `-` | right |
| 3 | `*`, `/`, `%` | left |
| 4 | `+`, `-` | left |
| 5 | `~` (merge) | left |
| 6 | `\|` (fallback) | left |

Parentheses group. `&-1` is `&` minus 1, and `-&` negates `&`.

A unary `-` directly before an integer or float literal is folded into
the literal, so `-170141183460469231731687303715884105728` (`i128::MIN`)
is valid, and so is the accessor `[-1]`.

Postfix selectors apply to any expression: `(&root.a ~ { b: 1 }).b`,
`[1, 2][0]`.

### 6.3 Arithmetic and concatenation

| Operands | `+` | `-` | `*` | `/` | `%` | unary `-` |
|---|---|---|---|---|---|---|
| integer, integer | add | subtract | multiply | divide, truncating toward zero | remainder, sign of the left side | negate |
| float, float | add | subtract | multiply | divide | remainder (`fmod`) | negate |
| integer, float (either order) | as floats | as floats | as floats | as floats | as floats | |
| string, string | concatenate | | | | | |
| string, integer/float/char/boolean (either order) | concatenate the formatted value (section 6.6) | | | | | |
| list, list | concatenate | remove every left element equal to a right element | | | | |

- Integer overflow (outside `i128`) is an error. Integer division or
  remainder by zero is an error.
- A float result that is not finite (division by zero, overflow) is an
  error.
- Converting an integer to a float rounds to nearest, ties to even.
- Lists only take lists: `tags: & + ["x"]`. A single element is an
  error, because without types `[[1]] + [2]` could not tell an element
  from a list to append.
- Any other combination is an error naming both kinds.

### 6.4 Merge `a ~ b`

`b` is normally a block (a patch), and section 5.2 gives the rules.
In general:

| `a` | `b` | Result |
|---|---|---|
| keyed value | block | the block applied with `a` as its start value |
| keyed value | other keyed value (e.g. `& ~ &root.x`) | as if `b` were a block that sets each of its entries, plus its defaults flag and defaulted fields |
| named tuple `N(...)` | named tuple `N(...)` | `b` |
| unit name `N` | unit name `N` | `N` |
| anything else | | error; use `:`, `+`, `-` or a path |

- If both sides have a type name they must be equal, else it is an
  error ("cannot merge `Service` with `Limits`"). This also rejects
  merging different enum variants; replace them with `:` instead.
- The result's type name is `a`'s, or `b`'s if `a` has none.

### 6.5 Fallback `a | b`

`a | b` is `a`, unless evaluating `a` fails because a value is
*missing*, in which case it is `b`. Missing means:

- `&` with nothing below;
- a reference whose path does not exist (a key, field, index or variant
  value that is not there);
- a spread of a missing value;
- `&default` where no default is recorded (section 6.1.3).

Any other error in `a` (wrong kinds, overflow, a cycle, a type name
mismatch) is reported, not caught.

`a` is checked *to its own level*: evaluation is lazy (section 7.5),
and a missing value inside one of `a`'s entries or list elements is not
caught here; put the `|` inside, where the value is. Examples:

```rust
counter: & + 1 | 1,                        // counts from 1
web: & ~ { replicas: 2 } | Service { replicas: 2, .. }, // patch, or create
port: &root.settings.port | 80,
```

### 6.6 Formatting values into strings

`"port " + 8080` is `"port 8080"`. Integers format in decimal. Floats
use the shortest representation that reads back as the same value, with
`.0` added if it would otherwise look like an integer (`1.0`, `0.1`,
`1e30`), as Rust's `{:?}` does. Chars insert
the character; booleans `true`/`false`. Other kinds (ids, lists, keyed
values) are an error; write `"..."` around an id's text if needed.

---

## 7. Layers and evaluation

### 7.1 Layers

A **layer** is a named, ordered set of documents. The application loads
layers in priority order: later layers apply over earlier ones. Each
document in a layer has a path relative to the layer, with `/`
separators. A layer is one of:

- a directory: every `*.crol` file below it, recursively. Symbolic links
  are followed; hidden files and directories (name starts with `.`) are
  skipped;
- one file;
- in-memory documents, `CrolLayer::from_sources(name, [(path, text), ...])`,
  for tests, editors and embedded data.

`CrolComposer::load` (section 10.7) takes a module path (section 4.2) or
a file system path naming a file or a directory, and names the layer
after what it was given (`::data::base`, `data/local`). With
`CrolConfig::load_extends`, the `extends` of a layer's documents
(section 4.1) are loaded before it, as layers of their own, in document
order.

Documents in a layer are ordered by relative path, comparing the UTF-8
bytes. Two documents with the same path in one layer are an error.

### 7.2 Chains

Reading turns every document into operations and groups them in
**chains**. There is one chain per root key. An operation holds its
entry kind, its condition (none for base operations, section 8.2), its
path (from the root key), its expression unevaluated, its span and its
layer.

Root entries are classified as follows:

- A **definition** is a root entry (no path) that is a declare, an
  ensure, or a set whose expression does not use the value below. An
  expression *uses the value below* when it contains a `&` bound to the
  entry, including `&` in the entries of a whole-value block that is
  the entry's value, and in the left operand of `~` (section 6.1).
- Everything else is a **patch**: path entries, removes, and sets that
  use the value below (`web: & ~ {...}`).

Within one layer, a chain's operations are ordered:

1. the layer's definition of the key, if any;
2. its patches, in document order, then in order within each document.

Layers then follow each other in load order. A layer may hold at most
one definition per chain: two, in one file or in two, are an error
naming both. The loader warns (a *warning*, not an error) when patches
to one root key come from more than one document of one layer, since
their order then depends on file names; `CrolConfig::warn_cross_file_patches`
turns this off.

These rules order the base operations, and the conditional operations
within one specificity (section 8.3). For definitions, conditional
operations count per condition.

### 7.3 Early and late references

- `&` is **early**: it is the result of the chain's earlier operations
  (or, inside blocks, of the block's earlier steps).
- Every other reference (`&self`, `&parent`, `&root`, binds) and every
  spread of one is **late**: it reads the **final** value at that path,
  after all layers, in the current variant context (section 8).

So when a later layer changes `database.port`, every
`&root.database.port` sees the change, wherever it was written.

### 7.4 Applying a chain

Let `v` be *missing*. Apply each operation in order:

| Operation | Result |
|---|---|
| declare `+k: e` | error if `v` exists; else `e` evaluated (`&` missing) |
| set `k: e` | `e`, evaluated with `&` = `v` |
| ensure `?k: e` | `v` if it exists; else `e` (`&` missing) |
| remove `-k` | error if `v` is missing; else missing |
| path entry `k.p…: e` | error if `v` is missing; else `v` with the path entry applied (section 5.1) |

The key's final value is `v` after the last operation. If it is missing,
the key does not exist.

### 7.5 Laziness and cycles

Evaluation is lazy and memoized. A value is computed when something
needs it, and then kept.

Each value has two levels:

- its **shape**: its kind, type name and, for keyed values, its keys,
  defaults flag and defaulted fields; for lists and tuples, their
  length;
- its **contents**: the values of its entries and elements, each lazy
  in turn.

Operations need only what they use:

| Needs the shape of its operands | Needs the full value of its operands |
|---|---|
| `~`, spreads, selectors, path entries, `\|` (left side), list `+` | arithmetic, string concatenation, list `-` (equality) |

The application deserializing a value needs its full value.

A value whose computation needs itself, at the same level, is a
**cycle**, an error that lists every reference in it with its span.
Because levels are separate, this is fine:

```rust
+web: { port: 80, url: "http://x:" + &self.port },  // url needs port, not url
```

and this is a cycle (the shape of `a` needs the shape of `a`):

```rust
+a: { ..&self.b, b: {} },
```

### 7.6 Determinism

Evaluating the same layers, with the same options, gives the same data
on every platform:

- No operation reads the environment, the clock, the file system (after
  loading) or randomness.
- Integer arithmetic is exact or an error. Float arithmetic is IEEE 754
  binary64 with round-to-nearest-even and no fused multiply-add; float
  literal parsing is correctly rounded (`f64::from_str`).
- Keyed values keep insertion order, and the order of operations is
  fixed by section 7.2. No hash-map iteration order is observable.

**Fingerprint.** `CrolData::fingerprint()` is the BLAKE3-256 hash of the
canonical encoding of all evaluated data: every key in root order with
its base value, then each evaluated context (section 8.1) with the keys
whose value differs from the base. The canonical encoding is defined in
appendix B. It is taken over evaluated values, so two data sets with
different sources but the same values have the same fingerprint, and any
difference in values (or in the library version's behaviour) shows as a
mismatch.

### 7.7 Errors and recovery

Errors do not stop evaluation. A value that fails is *poisoned*; values
that need it are poisoned too, and report only "depends on a value that
failed" at debug level. Evaluating everything therefore reports every
independent error once. Each diagnostic has:

- a message, starting with the operator or entry that failed:
  `+: cannot add a string to a list`;
- a primary span (file, line, column, layer);
- secondary spans (the entry that created a key that a `+` collides
  with, every reference in a cycle, both definitions in a layer);
- the evaluation context: `while evaluating web<prod>.limits.memory_mb`.

**Limits.** So that no input can crash or stall a loader or editor,
reading and evaluation have fixed limits, each an error when exceeded:

- nesting in a document (brackets, prefix operators, chains of binary
  operators): 256 levels;
- references being evaluated inside one another (a chain of late
  references, each needing the next): 128;
- an evaluated value nesting deeper than 256 levels, or containing a
  copy of itself, as `x: { ..&root }` does: the copy holds `x`, which
  holds another copy, without end. It is reported as a cycle; removing
  the entry stops it (`x: { ..&root, -x }` is fine);
- 2 000 000 values computed in one evaluation;
- one entry standing for more than 256 conditional operations: `+`
  lists multiply through nesting, so `<a + b> { <c + d> { x: 1 } }` is
  four, and nine such levels are 512.

The limits are part of the format: a later version may raise them, but
never lower them.

---

## 8. Variants

A **variant** is a named alternative of the data, such as an
environment, a region, a platform, a feature or a tenant. Variants can
be combined: the data is evaluated in a **context**, the set of variants
that are active.

### 8.1 Declaring variants

Only the application declares variants; data cannot add or change them.

```rust
pub enum Variant<T = String> {
    Discrete(T),       // a flag: on or off, independent of the rest
    Grouped(Vec<T>),   // alternatives: at most one of them is active
}
```

They are declared with `CrolVariants`, passed in `CrolConfig::variants`
(section 10.7):

```rust
let variants = CrolVariants::new()
    .add_grouped(["dev", "prod", "prod_eu"])
    .add_discrete("cache")
    .add_discrete("debug")
    .add_parent("prod_eu", "prod");
```

- Every name is declared once. A name in a selector that is not
  declared is an error.
- **Parents.** `add_parent(child, parent)` gives a variant a parent. A
  variant being active makes its parent active too, and so on up:
  `prod_eu` implies `prod`. A variant and its parent must both be
  `Discrete`, or be in the same group; in a group, an active variant's
  ancestors do not count against "at most one". A cycle among parents is
  an error.
- If no variants are declared, every name the data uses in a selector
  (and that `T` accepts) is a `Discrete` with no parent.

**Typed variants.** `T` is `String` by default. An application may use
its own type instead, so that the variants it passes are checked by the
compiler:

```rust
#[derive(Clone, PartialEq, Eq, crol::VariantName)]
enum Env { Dev, Prod, ProdEu, Cache }

let variants: CrolVariants<Env> = CrolVariants::default()
    .add_grouped([Env::Dev, Env::Prod, Env::ProdEu])
    .add_discrete(Env::Cache)
    .add_parent(Env::ProdEu, Env::Prod);
```

`T` implements `crol::VariantName`, which maps a value to the name data
writes and back:

```rust
pub trait VariantName: Clone + Eq {
    fn name(&self) -> &str;
    fn from_name(name: &str) -> Option<Self>;
}
```

It is implemented for `String` (every identifier is accepted). The
derive (cargo feature `derive`) is for enums without fields; it names
each variant in snake case (`ProdEu` is `prod_eu`), and
`#[crol(rename = "...")]` overrides a name. A selector name that
`from_name` rejects is an unknown variant. (`From<String>` would not do:
the composer needs each value's name to match selectors, and turning an
unknown name into a value must be able to fail.)

A **context** is a set of variants with at most one chosen per group,
together with all their ancestors. The **base** context is the empty
set. `{prod_eu, cache}` is the context in which `prod_eu`, `prod` and
`cache` are active.

**Which contexts are evaluated.** By default, every valid context: each
group's choice (or none) times each `Discrete` on or off. That is 4 × 2 ×
2 = 16 contexts in the first example. `CrolConfig::contexts` lists the contexts
the application uses instead; it is required when the product exceeds
`CrolConfig::max_contexts` (default 4096). Only evaluated contexts are
checked and fingerprinted.

### 8.2 Conditions

On an entry, a variant selector is a **condition**: the entry applies in
every context where the condition holds.

| Condition | Holds when |
|---|---|
| `<a>` | `a` is active |
| `<!a>` | `a` is not active |
| `<a & b>` | both hold |
| `<a \| b>` | either holds |
| `<a ^ b>` | exactly one holds |
| `<(a \| b) & !c>` | parentheses group |

Precedence, tightest first: `!`, `&`, `^`, `|`, as in Rust.

`<a + b>` is not a condition but a list: the entries are written once
for each condition, as if under `<a>` and again under `<b>`. It is only
allowed as the outermost operator: `<a + (b & c)>`. The difference with
`|` shows when both hold: `x<a | b>.n: & + 1` adds 1, `x<a + b>.n: & + 1`
adds 2.

**Where conditions are written:**

- `x<c>: e`, `x<c>.p: e`, `-x<c>` and so on. The selector may appear
  anywhere in the target's path: `x.a.b<c>` means the same as
  `x<c>.a.b`; a condition always applies to the whole root key.
- `<c> { entries }`: every entry inside has condition `c`. At the root
  this is a group of root entries; inside a block, its entries are
  fields of that block.
- Inside a block, `field<c>: e` (or `<c> { field: e }`) is a variant
  operation at the block's location: `+web: Service { replicas<prod>: 4, .. }`
  adds `web<prod>.replicas: 4`, ordered after the entry the block is in.
- **Nesting is conjunction:** `<a> { <b> { ... } }`, `x<a>.y<b>` and
  `<a> { x<b>: ... }` all mean `<a & b>`. A `+` list distributes:
  `<a + b> { <c> { ... } }` means `<(a & c) + (b & c)>`.

Variant entries inside blocks are lifted out when the document is read.
They are allowed only in blocks whose location is fixed: blocks that are
an entry's value or the right side of `~` there, and blocks nested in
those as field values. Inside a list, a `|` operand, an arithmetic
operand or a spread, they are an error.

A condition that cannot hold in any valid context (`<dev & prod>`, with
both in one group) is an error.

### 8.3 Evaluating a key in a context

The chain of `x` in context `C` is:

1. all of `x`'s base operations (no condition), as in section 7.2;
2. then every operation with a condition that holds in `C`, ordered by
   **specificity**, lowest first. Operations of equal specificity keep
   the order of section 7.2: layers in load order, and within a layer
   the definition first, then document and source order.

All of them are evaluated in context `C`. So a later layer's change to
the base reaches every context, unless a conditional definition
(`web<prod>: Service { .. }`) replaces the key in that context, which
replaces the base value with every base change in it; and more specific
operations win over less specific ones, wherever they are written.

A key with no operations whose condition holds in `C` is `x`'s base
chain evaluated in `C`. That differs from the base value only if `x`
references something that does have such operations.

**Specificity** of a condition, like CSS selectors:

| Condition | Specificity |
|---|---|
| `a` | 1, plus 1 for each ancestor of `a` |
| `!a` | 0 |
| `a & b` | specificity of `a` + specificity of `b` |
| `a \| b`, `a ^ b` | the lower of the two |

With `prod_eu` under `prod`: `prod` is 1, `prod_eu` is 2, `prod & cache`
is 2, `prod_eu & cache` is 3, `prod & !cache` is 1.

Within one layer, a key may have at most one definition per condition
(conditions compared after normalising order and parentheses);
definitions under different conditions are ordered by specificity like
any other operation.

**Example**, with `prod_eu` under `prod`, and `cache` a `Discrete`:

```rust
+database: Service { host: "db.internal", replicas: 1, .. },
+web: Service {
    replicas: 2,
    database_url: "postgres://" + &root.database.host,
    cache_url: None,
    ..
},

<prod> {
    web.replicas: & * 4,
},
<prod_eu> {
    database.host: "db.eu.internal",
    web.replicas: & + 2,
},
<cache> {
    web.cache_url: Some("redis://cache:6379"),
},
<prod & !cache> {
    web.replicas: & + 1,    // one more web replica when there is no cache
},
```

| | base | `{prod}` | `{prod_eu}` | `{prod_eu, cache}` |
|---|---|---|---|---|
| `web.replicas` | 2 | 9 | 11 | 10 |
| `web.database_url` | `…db.internal` | `…db.internal` | `…db.eu.internal` | `…db.eu.internal` |
| `web.cache_url` | `None` | `None` | `None` | `Some(…)` |

In `{prod_eu}`: `prod` (1) gives 8, `prod & !cache` (1, later in the
file) gives 9, then `prod_eu` (2) gives 11.

Copying another context's value (`web<prod_eu>: &root.web<prod> ~ {...}`)
is not the same as a parent: the copy is evaluated in `{prod}`, so its
`database_url` would read `prod`'s database.

Applying one key's variant operations to another key is deliberately not
supported: it would make operations into reusable functions. A patch
without `&` can be shared as a template instead: `shared_patch := { ... }`,
then `x<b>: & ~ &shared_patch`.

### 8.4 Context in references

Late references inherit the context: while evaluating `web` in
`{prod, cache}`, `..&service_defaults` and `&root.database.port` read
those values in `{prod, cache}`. `&` is early and stays in the chain.

In a reference, a variant selector does not test a condition but
**changes the context** for that reference:

- `&root.db<dev>` makes `dev` active. In a group, it replaces the
  group's current choice: from `{prod_eu, cache}` it gives
  `{dev, cache}`.
- `&root.db<!cache>` makes `cache` (and its descendants) inactive.
- `&root.db<dev & !cache>` makes both changes.

Only names, `!` and `&` are allowed there; `|`, `^`, `+` and
parentheses are errors, since they do not name one context. A variant
selector may follow any late reference (`&root.db<dev>`, `&self<dev>`,
a bind); on `&` (early) and `&default` (fixed) it is an error.

### 8.5 Keys only in some contexts

A conditional operation may declare keys the base lacks
(`<dev> { +debug_tools: ... }`) and remove keys (`-cache<dev>`). Whether
that is allowed is the application's rule; `CrolConfig::variant_keys` is
`Any` (default), `NoNewKeys` (every key in a context must exist in the
base), or `SameKeys` (every context has exactly the base's keys).

---

## 9. Grammar

```ebnf
document     ::= attribute* ( entries | block | expr )   (* section 4 *)
attribute    ::= "#" "!" "[" ( use_attr | schema_attr | type_attr | extends_attr ) "]"
use_attr     ::= "use" "(" use_path ( "," use_path )* ","? ")"
schema_attr  ::= "schema" "(" ( module_path | string ) ")"
type_attr    ::= "type" "(" identifier ")"
extends_attr ::= "extends" "(" module_path ")"
module_path  ::= ( "::" | ( "super" "::" )* ) identifier ( "::" identifier )*   (* section 4.2 *)
use_path     ::= identifier ( "::" identifier )* type_args? ( "as" identifier )?
type_args    ::= "<" type_expr ( "," type_expr )* ">"   (* section 10.10's type expressions *)

block        ::= identifier? "{" entries "}"
entries      ::= ( entry ( "," entry )* ","? )?
entry        ::= "-" target
               | bind? ( "+" | "?" )? target ":" expr
               | identifier ":=" expr                (* value bind, 6.1.2 *)
               | variant "{" entries "}"
               | ".." postfix?
target       ::= key selector*
key          ::= identifier | string | integer | accessor
accessor     ::= "[" ( string | "-"? integer ) "]"
selector     ::= "." identifier | "." tuple_index | accessor | variant
variant      ::= "<" condition ( "+" condition )* ">"
condition    ::= cond_xor ( "|" cond_xor )*
cond_xor     ::= cond_and ( "^" cond_and )*
cond_and     ::= cond_not ( "&" cond_not )*
cond_not     ::= "!" cond_not | identifier | "(" condition ")"
tuple_index  ::= decimal digits, no underscore or sign

expr         ::= merge ( "|" merge )*
merge        ::= sum ( "~" sum )*
sum          ::= product ( ( "+" | "-" ) product )*
product      ::= unary ( ( "*" | "/" | "%" ) unary )*
unary        ::= "-" unary | postfix
postfix      ::= primary selector*
primary      ::= literal
               | id
               | reference
               | block                             (* Service { ... }, { ... } *)
               | list
               | tuple
               | identifier "(" exprs ")"          (* Some(1), Rgb(1, 2, 3) *)
               | identifier                        (* None, Red *)
               | "(" expr ")"
reference    ::= "&" ( identifier | "self" | "root" | "default" | parents )?
parents      ::= "parent" ( "." "&" "parent" )*
bind         ::= identifier "="
list         ::= "[" exprs "]"
tuple        ::= "(" ")" | "(" expr "," exprs ")"
exprs        ::= ( expr ( "," expr )* ","? )?

id           ::= "@" ( identifier | string )
literal      ::= string | char | integer | float | "true" | "false"
```

Notes:

- `identifier "{"` starts a block, `identifier "("` a named tuple, any
  other identifier is a unit name. `"(" expr ")"` is grouping; with a
  comma it is a tuple.
- `variant` in a selector: after a target or a postfix expression, `<`
  can only start a variant, since CROL has no comparisons. Inside
  `<...>`, `&`, `|`, `^`, `!` and `+` are condition operators, never
  references or arithmetic.
- In a reference, a `variant` may only use names, `!` and `&`
  (section 8.4).
- In `entry`, `"-" target` is removal. An expression never starts an
  entry, so `-1` cannot be confused with it.
- In `selector`, `.0.1` is lexed as `.` `0` `.` `1`, not as the float
  `0.1`, after a `.`.
- `string`, `char`, `integer`, `float` and `tuple_index` are the
  lexical forms of section 2.4.
- In `key`, an `integer` has no sign; a negative key needs the
  `accessor` form (section 5.1).
- A `[` at the start of an entry is an accessor key, never a list.

---

## 10. Rust integration

The `crol` crate provides a serde `Deserializer` over evaluated values,
and a `Serializer` that writes plain value documents.

### 10.1 Mapping

| serde request | Accepts |
|---|---|
| `bool` | boolean |
| integers (`i8`…`u128`) | integer within the type's range |
| `f32`, `f64` | float or integer (`f32` rounds to nearest) |
| `char` | char |
| `str`, `String` | string or id (the id's text) |
| `bytes`, `byte_buf` | list of integers 0–255 |
| `option` | `None` → none; `Some(x)` → some(`x`); any other value → some(value) if `implicit_some` (default on) |
| `unit` | `()` |
| `unit_struct` | the struct's name, or `()` |
| `newtype_struct` | `Name(x)` or `x` |
| `seq` | list |
| `tuple` | tuple |
| `tuple_struct` | `Name(a, b)` or `(a, b)` |
| `map` | keyed value. String keys deserialize into strings, unit enum variants (by name), `crol::Id` (section 10.5) and newtypes of these; integer keys into integer types |
| `struct` | keyed value whose keys are all strings (section 10.2) |
| `enum` | `V`, `V(x)`, `V(a, b)`, `V { ... }` (section 10.4) |
| `identifier` | field and variant names |
| `any` | the value's natural form: keyed values as maps (type name dropped), named tuples as single-entry maps from name to payload, ids as strings |
| `ignored_any` | anything |

### 10.2 Structs

For `deserialize_struct(name, fields, visitor)` the deserializer checks:

- **Type name.** If the keyed value has one, it must equal `name` (the
  Rust name after `#[serde(rename)]`), or the enclosing newtype
  variant's name (section 10.4).
- **String keys.** An integer key is an error.
- **Unknown fields.** A key not in `fields` is an error, unless
  `CrolConfig::allow_unknown_fields`.
- **Completeness.** Every field in `fields` must be present, including
  `Option` fields, unless the value has the defaults flag or the field
  is in its defaulted fields (section 10.3). `CrolConfig::require_complete`
  turns this check off.
- **Aliases.** A field written by one of its `#[serde(alias)]` names is
  present. serde puts a field's aliases in `fields` without saying which
  field they belong to, so the deserializer groups them from the schema
  given to the composer (`CrolComposer::schema`, section 10.9). Without
  a schema it cannot, and every name in `fields` counts as a field of
  its own. `crol::check` and tools read the aliases from the schema.

Only the final value is checked, so a patch block may leave out
whatever it does not change.

`#[serde(flatten)]` makes serde request a map instead of a struct. This
works, since every keyed value deserializes as a map, but the struct
checks above then do not apply to that struct.

### 10.3 Defaults

Without `..`, a struct must name every field, `Option` fields included.
With `..` (or for a field removed with `-field`), a field the value does
not hold is left out, and serde decides what it becomes:

- an `Option` field becomes `None`;
- a field with `#[serde(default)]`, or in a struct with it, takes that
  default;
- any other field is serde's "missing field" error, to which the
  deserializer adds the span of the value.

So the data says "I am leaving fields out on purpose", and the type says
what they become.

### 10.4 Enums

`deserialize_enum(name, variants, visitor)` accepts:

| Value | Variant | Payload |
|---|---|---|
| `V` | `V` | unit |
| `V(x)` | `V` | newtype `x`, or tuple `(x)` |
| `V(a, b)` | `V` | tuple |
| `V { ... }` | `V` | struct variant, **or** newtype variant whose payload is deserialized from `{ ... }` |
| `V { ... }` as a newtype variant | | the payload struct's type name check accepts `V` |

The last rule lets a root map of mixed types deserialize into a Rust
enum:

```rust
#[derive(Deserialize)]
enum Definition { Service(Service), Volume(Volume) }
```

```rust
+web: Service { replicas: 2, .. },   // Definition::Service(Service { replicas: 2, .. })
```

Internally tagged, adjacently tagged and untagged enums go through
`deserialize_any`, which loses type names. They work as they would with
`serde_json::Value`, with that restriction.

### 10.5 Ids

`crol::Id` is a newtype over `String` that accepts only an id
(`@web`), not a string. A plain `String` accepts both. As a map key,
`crol::Id` accepts a string key, since keys cannot be written with `@`
(`HashMap<crol::Id, u32>` is written `{ web: 1 }`). Every id the
deserializer produces is recorded as an `IdUse { id, span, rust_path }`,
returned beside the value, so the application can check that ids exist
and name the right kind:

```rust
let (svc, ids): (Service, Vec<IdUse>) = data.get_with_ids("web")?;
```

The library checks nothing about ids itself; `CrolData::contains(id)` and
`CrolData::type_name(id)` help the application do it.

### 10.6 Errors from deserialization

Evaluated values keep the span and layer of the expression that produced
them, so a deserialization error points at source:
`expected u16, found -1 (local, overrides.crol:3:12, while deserializing
web.port)`.

### 10.7 API sketch

The names below are normative in intent, not in every detail.

```rust
use crol::{CrolComposer, CrolConfig, CrolError, CrolVariants};

fn load(root_path: PathBuf, local: &Path) -> Result<Config, CrolError> {
    let variants = CrolVariants::new()
        .add_grouped(["dev", "prod", "prod_eu"])
        .add_discrete("cache")
        .add_parent("prod_eu", "prod");
    let composer = CrolComposer::with_cfg(CrolConfig {
        variants,
        root_path,             // where `::` module paths start (4.2)
        load_extends: true,    // follow #![extends(...)] (4.1)
        ..Default::default()
    });

    let config: Config = composer
        .schema(crol::schema::build::<Definition>())   // optional; for &default
        .load("::data::base")?     // by module path
        .load(local)?              // by file system path
        .with_variant("prod_eu")?
        .with_variant("cache")?
        .compose()?;               // evaluate in {prod_eu, cache}, deserialize
    Ok(config)
}
```

**The composer.** `CrolComposer` is a builder, consumed by `compose` or
`compose_data`. Elsewhere this specification calls it, with the
evaluator and deserializer behind it, **the loader**, as opposed to
tools:

- `load(source)` adds a layer on top of those loaded so far (section
  7.1). A `&str` is a module path, a `&Path` or `PathBuf` a file system
  path (relative ones from the working directory). It fails only when the
  source names nothing or cannot be read; syntax and every other error
  come from `compose`, all together. `load_layer(CrolLayer)` adds a
  layer built in code.
- `with_variant(v)` makes `v` active in the context to compose (section
  8.1). It fails for an undeclared variant (when variants are declared), or a second choice in a group
  that is not an ancestor or descendant of the first. Without any, the
  context is the base.
- `compose::<T>()` evaluates every key in that context and deserializes
  the root as `T` (section 10.4): a struct whose fields are the root
  keys, or a map such as `IndexMap<String, Definition>`. (The schema's
  root type is still the type of each root value, here `Definition`,
  section 4.1.) The error
  carries every diagnostic; warnings are dropped (`compose_data` keeps
  them).
- `compose_data()` evaluates every key in every evaluated context
  (section 8.1), as `crol check` does, and returns `CrolData`, for
  reading several values, other contexts, provenance and the fingerprint:

```rust
let data: CrolData = composer.load("::data::base")?.compose_data()?;
for warning in data.warnings() { /* ... */ }

let web: Service = data.get("web")?;                 // in the composed context
let ctx = data.context(["prod_eu", "cache"])?;       // checks it is valid and evaluated
let web_eu: Service = data.get_in("web", &ctx)?;
let all: IndexMap<String, Definition> = data.deserialize_root(None)?;  // None: the composed context
let hash: [u8; 32] = data.fingerprint();
let raw: &crol::Value = data.value("web", &ctx).unwrap();
let history = data.explain("web.limits.memory_mb", &ctx); // section 12.3

// Single documents (value mode)
let config: Config = crol::from_str(text)?;
let text: String = crol::to_string_pretty(&config, crol::PrettyConfig::default())?;
```

With typed variants (section 8.1), `CrolComposer<Env>`, `with_variant`,
`CrolData<Env>` and `context` take `Env` values instead of names.
`CrolData` stores values identical to the base value once (shared), so
memory grows with what variants change, not with their number.

`CrolConfig<T>`:

| Field | Default | Section |
|---|---|---|
| `variants: CrolVariants<T>` | none declared (any name, as `Discrete`) | 8.1 |
| `root_path: PathBuf` | the working directory | 4.2 |
| `load_extends: bool` | `false` | 4.1 |
| `contexts: Option<Vec<Vec<T>>>` | `None` (every valid context) | 8.1 |
| `max_contexts: usize` | `4096` | 8.1 |
| `variant_keys: VariantKeys` | `Any` | 8.5 |
| `implicit_some: bool` | `true` | 10.1 |
| `allow_unknown_fields: bool` | `false` | 10.2 |
| `require_complete: bool` | `true` | 10.2 |
| `warn_cross_file_patches: bool` | `true` | 7.2 |

### 10.8 Serializer

`crol::to_string` and `to_string_pretty` write a value-mode document from
any `Serialize` value: structs as `Name { field: ... }` (type names
optional, on by default), maps as `{ key: ... }` with keys written as
section 3.1 shows, `Option` as `Some(x)`/`None` (or bare `x` with
`implicit_some`), floats with section 6.6's formatting, strings with
escapes. Map keys that are not strings, integers or unit variants are an
error. A struct whose serialization leaves fields out (`skip_serializing`,
`skip_serializing_if`) gets `..`. The output has no operators; it is
plain data that reads back to the same value.

### 10.9 Schema

Evaluation and deserialization need no schema, except `&default`
(section 6.1.3), which reads defaults from it. Tools do: a language
server cannot complete field names, show docs or check types without
knowing the Rust types. The schema describes them; it is built from the
Rust types and written to a file.

**Marking types.** `#[derive(crol::Schema)]` (cargo feature `derive`)
marks a type as part of the schema, and allows the `#[crol(...)]`
attributes on it and its fields:

```rust
#[derive(Deserialize, Serialize, crol::Schema)]
#[crol(defaults)]                     // record default values (below)
/// A service that runs one container image.
pub struct Service {
    /// Number of copies to run.
    pub replicas: u32,
    #[crol(refers(Service, Volume))]  // ids must name a Service or a Volume
    pub depends_on: Vec<crol::Id>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub limits: Limits,
}
```

`#[crol(refers(...))]` applies to an `Id` field, or to the ids inside a
list, option or map of them, and names the root types an id may name.
`#[crol(defaults)]` on a type records its default values in the schema,
for `&default` (section 6.1.3) and tools: the struct's
`Default::default()` if it has `#[serde(default)]`, and each field's own
`#[serde(default)]` (`<T as Default>::default()`) or
`#[serde(default = "path")]` (`path()`). The values are serialized, so
the types involved must implement `Serialize`.

**How the schema is built.** The schema comes from the type system, not
from parsing source, so it is complete: the compiler has resolved every
path, alias, generic and macro. `crol::Schema` is a trait, composed like
serde's:

```rust
pub trait Schema {
    fn describe(registry: &mut crol::schema::Registry) -> crol::schema::TypeRef;
}
```

- The derive implements it for a type from its fields and serde
  attributes (`rename`, `rename_all`, `alias`, `default`, `skip`,
  `flatten`, `tag`, `content`, `untagged`, `transparent`,
  `deny_unknown_fields`), with doc comments and the source file and line
  of the type (`file!()`, `line!()`).
- `crol` implements it for the scalars, `String`, `Option`, `Box`, `Rc`,
  `Arc`, `Cow`, std's collections, tuples, arrays, `PathBuf`, `Duration`
  and `crol::Id`, and, behind cargo features, for common crates' types
  (`indexmap`, `glam`, `chrono`, ...).
- A field whose type has no implementation, or uses
  `#[serde(with = "...")]`, takes `#[crol(schema = OtherType)]`, naming a
  type with the same serialized shape, or `#[crol(schema = any)]`, which
  tools accept without checks. A hand-written `Deserialize` can come
  with a hand-written `Schema`.

**Writing the schema file.** Building the schema runs code, so it is
written by the application, not by `build.rs`. The usual way is a test,
which `cargo test` and CI run anyway:

```rust
#[test]
fn crol_schema() {
    crol::schema::check_file::<Definition>("crol/schema.crol").unwrap();
}
```

`check_file` builds the schema of `Definition` and everything it uses.
It fails if the file differs, and writes it instead when
`CROL_UPDATE_SCHEMA=1` is set (or the file does not exist). So the file
is never silently out of date in CI. `crol::schema::build::<T>()` returns
the schema as a value, for `CrolComposer::schema` (section 6.1.3) and
`crol::check`. `crol::schema::write::<T>(path)`
writes unconditionally, for an application's own `--export-schema`
flag. The file goes into the crate (`crol/schema.crol`), where tools
find it and from where an application ships it with its data.

**Model.** A `crol::schema::Schema` is plain data: its root type (the
`T` it was built for) and the named types it uses. Section 10.10 gives
its file format.

**Names.** Data refers to types by name (`Service { ... }`), and the
deserializer checks names, not paths (section 10.2). Two schema types
with the same name are allowed; a document that uses an ambiguous name
must say which type it means with `#![use(...)]` (section 4.1), and
tools report an ambiguous name without one.

**Checking without the types.** `crol::check(&data, &schema, root)`
(`root`: the path of the root values' type, or `None` for the schema's
root type) applies the deserializer's rules (sections 10.1 to 10.5) to evaluated
data using only the schema, and checks ids: every id must name a root
key in the same context, whose type is one of the allowed ones. It gives
the same errors, with the same spans, as deserializing would, and is
what the language server and `crol check --schema` use. The conformance
suite runs both on every case to keep them in agreement.

### 10.10 Schema file format

A schema file is a value-mode CROL document of the type
`crol::schema::Schema`. Its first line names its own schema, so editors
check schema files too. An excerpt, for the `Service` of section 10.9
(the definitions of `app::Limits` and `app::Volume` are left out):

```rust
#![schema("crate:crol@1")]

Schema {
    format: 1,
    root: "app::Definition",
    types: {
        "app::Definition": Enum {
            name: "Definition",
            doc: "Everything the data can define.",
            source: ("src/model.rs", 8),
            repr: External,
            variants: {
                Service: { shape: Newtype(Named("app::Service")), .. },
                Volume: { shape: Newtype(Named("app::Volume")), .. },
            },
        },
        "app::Service": Struct {
            name: "Service",
            doc: "A service that runs one container image.",
            source: ("src/model.rs", 15),
            fields: {
                replicas: { ty: U32, doc: "Number of copies to run.", .. },
                depends_on: { ty: List(Id(["app::Service", "app::Volume"])), .. },
                tags: { ty: List(String), default: Value([]), .. },
                limits: { ty: Named("app::Limits"), default: Value(Limits { cpu: 1.0, memory_mb: 512 }), .. },
            },
            ..
        },
    },
}
```

**Top level.** `format` is the schema format's version; tools reject a
schema whose `format` they do not know. `root` is the path of the root
type. `types` maps each named type's path to its definition, in a fixed
order (sorted by path), so the file is stable for version control.

**Paths** are the Rust module path followed by the type's name as data
writes it (`app::net::Config`, section 4.1). A generic type used with
arguments gets one entry per instantiation, with the arguments as type
expressions: `"app::Pair<U32, String>"`.

**Type definitions.** Every definition has `name` (the serde name),
`doc` (the doc comment, empty if none), `source` (file, relative to the
Cargo workspace root as `file!()` gives it, and line) and:

| Definition | Rust | Extra fields |
|---|---|---|
| `Struct` | struct with named fields | `fields`, `deny_unknown_fields` (default `false`), `default` (the struct's recorded default value, if any) |
| `TupleStruct` | `struct P(A, B)` | `fields`: list of type expressions |
| `Newtype` | `struct N(A)` | `inner`; data writes `N(x)` or `x` |
| `Transparent` | `#[serde(transparent)]` | `inner`; data writes only `x` |
| `UnitStruct` | `struct U;` | |
| `Enum` | enum | `repr`, `variants` |

`repr` is `External` (serde's default), `Internal { tag }`,
`Adjacent { tag, content }` or `Untagged`. `variants` maps each variant's
serde name to `{ shape, doc, aliases }`, where `shape` is `Unit`,
`Newtype(ty)`, `Tuple([ty, ...])` or `Struct({ fields })`.

**Fields** map each field's serde name to:

| Field | Meaning | Default |
|---|---|---|
| `ty` | its type expression | (required) |
| `doc` | doc comment | `""` |
| `aliases` | serde aliases | `[]` |
| `default` | `Required` (no default: must be written, even with `..`), `Unrecorded` (has a default, value not recorded), or `Value(v)` (the default value, written as CROL) | `Required` |
| `flatten` | `#[serde(flatten)]` | `false` |

`Option` fields without a `#[serde(default)]` have
`default: Value(None)`: serde makes them `None` when absent, so with
`..` they may be left out. Without `..`, CROL's completeness rule
(section 10.2) requires every field, whatever its default.

**Type expressions:**

| Expression | Rust |
|---|---|
| `Bool`, `Char`, `String`, `Unit` | `bool`, `char`, `String`/`str`/`PathBuf`, `()` |
| `I8` … `I128`, `U8` … `U128`, `F32`, `F64` | the numeric types; integer ranges follow from the type |
| `Bytes` | `serde_bytes` and the like |
| `Id(["path", ...])` | `crol::Id`, with the types it may refer to (`Id([])`: any root key) |
| `Option(ty)` | `Option<T>` |
| `List(ty)`, `Set(ty)` | `Vec`, `VecDeque`, slices; `HashSet`, `BTreeSet` (elements unique) |
| `Array(ty, n)` | `[T; n]` (a tuple of `n` in serde) |
| `Tuple([ty, ...])` | tuples |
| `Map(key, value)` | `HashMap`, `BTreeMap`, `IndexMap` |
| `Named("path")` | a type in `types` |
| `Any` | anything; not checked |

`Box`, `Rc`, `Arc`, `Cow` and other wrappers that serde makes
transparent are left out: `Box<Service>` is `Named("app::Service")`.

The generated file writes only the fields that differ from their
defaults and ends those blocks with `..`, so it stays readable.

---

## 11. Command-line tool

`crol` (crate `crol-cli`):

| Command | Does |
|---|---|
| `crol check [<layer>...]` | read and evaluate the layers (each `name=source` or a source, named after it, where a source is a module path (section 4.2) or a file system path, the module path if both name something; without any, those of the project file, section 12.4); check them against the schema if one is given or configured; print diagnostics; exit 1 on errors |
| `crol eval [<layer>...] [--context a,b] [key[.path]]` | print evaluated data as a plain data-mode document (or one value); with a schema, struct fields that take their defaults are written out with the defaults the schema records, each marked `// default` (`--no-defaults` leaves them as the data does) |
| `crol eval [<layer>...] --all-contexts` | print the base data, then for every other evaluated context a block `<a & b & !c & ...> { ... }` whose condition holds in that context only, with the keys whose value differs (`key: value`) or that are missing (`-key`). The output is valid CROL and evaluates back to the same data |
| `crol explain [<layer>...] [--context a,b] key.path` | print the operations that produced a value (section 12.3) |
| `crol fmt [--check] <file>...` | format files in place (or check) |
| `crol fingerprint [<layer>...]` | print the fingerprint in hex |
| `crol from-ron <file>` | convert a RON value file to a CROL value document |
| `crol lsp` | run the language server on standard input and output (section 12.5) |

Common flags: `--discrete a,b`, `--group a,b,c` (repeatable), `--parent child=parent`, `--color auto|always|never`,
`--message-format human|json`, `--project <file>`, `--schema <file>`,
`--offline` (section 4.1).
Without layer arguments, every command that needs layers reads them, with
the variants and options, from the project file.

---

## 12. Tooling support

### 12.1 Syntax tree

The parser produces a **lossless concrete syntax tree**: every byte of
the input, including whitespace, comments and invalid text, is in the
tree, and printing the tree gives the input back. The parser recovers
from errors (a missing comma, an unclosed brace) and keeps parsing, so an
editor gets a tree and all syntax errors for a half-written file. A
typed AST layer gives access to entries, targets and expressions over the
tree.

### 12.2 Formatter

`crol fmt` is idempotent and keeps comments. Style:

- 4-space indentation; a maximum line width of 100 (configurable);
- a block, list or tuple goes on one line if it fits and has no
  comments, else one entry per line with a trailing comma. A block
  written with a line break directly after its `{` stays broken, so
  authors can keep a short block on several lines; lists and tuples
  always reflow;
- spaces around binary operators and after `:` and `,`; none inside
  `&name`, `@id`, selectors or `..&x`; binds have a space before and
  after `=` and `:=`;
- blank lines between entries are kept, but at most one;
- a `..` marker is moved to the end of its block, and duplicates are
  removed;
- the outer braces of a data-mode document are removed.

Where these rules leave a choice open, the formatter follows rustfmt's
style.

### 12.3 Provenance

For any evaluated path and context, `CrolData::explain` returns the list of
operations that contributed to it, in order, with span, layer, entry
kind and the value after each step. This answers "why is this value
what it is?" in the CLI, and lets the language server show the evaluated
value and its history on hover. For the language server, the library
API must also provide:

- incremental re-evaluation is not required; re-evaluating everything
  after an edit must be fast enough for interactive use (target: 10 000
  root keys and 16 contexts in under 200 ms);
- spans map back to files and positions;
- the definition of a key or bind, and every reference and id use of it, can be
  found from the chains (go to definition, find references).

### 12.4 Project file

Tools that are not the application, the CLI and the language server,
learn the layers, variants and options from a **project file**,
`project.crol`, found by searching from the current directory (or
the editor's workspace folder) upwards. It is a value-mode CROL document
of the type `Project`, whose schema `crol` itself provides. The project file
itself is never read as data, even when it lies inside a layer's
directory; other files named `project.crol` are ordinary data.

```rust
Project {
    layers: List([
        { name: "base", dir: "data::base", read_only: true },
        { name: "extension", dir: "data::extension", .. },
        { name: "local", dir: "data::local", .. },
    ]),
    variants: [Grouped(["dev", "prod", "prod_eu"]), Discrete("cache")],
    parents: { prod_eu: "prod" },
    options: { implicit_some: true, allow_unknown_fields: false, .. },
    schema: Some("crol::schema"),
    root: Some("app::Definition"),     // the type of every root value
    files: [                           // value-mode documents and their types
        { glob: "config::*", type: "app::Config" },
    ],
}
```

- `dir` is a module path (section 4.2), written as a string; the root
  directory is the project file's. `glob` is a module path whose
  segments may hold `*` (any part of one name) or be `**` (any number of
  directories). `read_only` layers
  (generated data, vendored data) are loaded but get no diagnostics or
  code actions in the editor.
- `layers` is either `List([...])` or `Command(["my-app", "list-layers"])`.
  A command prints the layer list as a CROL value, for
  applications whose load order is itself data (extensions enabled by
  the user, for instance). It runs in the project file's directory, once
  at start and again when the language server is asked to reload.
- `schema` takes the same forms as the `schema` attribute (section 4.1),
  a module path written as a string:
  the file the application wrote (section 10.9), a URL, or a crate. An
  application that ships its data also ships this file, so people
  editing the data without the source get the same checks. Without a
  schema, tools check syntax, evaluation and ids' existence, but not
  types.
- `root` is the path of the type of root values, typically an enum of
  the application's definition kinds (section 10.4). It defaults to the
  schema's root type. Type names in `root` and `files` are schema paths;
  a plain name is accepted when it is unambiguous.
- `files` gives value-mode documents a type. A `.crol` file that is in
  no layer and matches no glob is evaluated as a value-mode document and
  checked against its `#![schema(...)]` if it has one; otherwise it gets
  syntax and evaluation checks only.

### 12.5 Language server

The language server (`crol lsp`, library crate `crol-lsp`) gives an
editor what the loader knows, while typing. It speaks LSP over standard
input and output, through `lsp-server` and `lsp-types`, and is
editor-neutral; section 12.7 is the Zed client.

**Workspace.** Without a project file, it serves each open document on
its own: syntax, evaluation (as a value-mode document, or on top of its
`#![extends(...)]` layers in data mode), and checks against its
`#![schema(...)]` if it has one. With one, at start it reads the
project file, loads every layer and
the schema, and evaluates every context, keeping the provenance
(section 12.3). Open buffers replace their files on disk, saved or not.
After an edit it parses the edited file again at once (for syntax errors
and the outline), and re-evaluates everything after a pause in typing
(default 150 ms), cancelling an evaluation that a newer edit makes stale.
Requests are answered from the latest finished evaluation, so the
editor never waits for one. Changes to files on disk, the project file
and the schema file are watched.

**Diagnostics.** Everything `crol check` reports, on the spot: syntax,
reading and evaluation errors, schema errors on final values (unknown
fields, wrong kinds, missing fields, type names), ids that name nothing
or the wrong type, and warnings. An error that occurs only in some
contexts says in which (`in {prod_eu, cache}: ...`), listing at most a
few and a count. Fields left out of a patch block or path entry are never an error, since
only final values are checked. Diagnostics are published for all files
of writable layers, not only open ones, and every diagnostic that has
secondary spans (cycles, two definitions) links to them as related
information.

**Hover.**

- On a target or key: the type and doc comment from the schema; the
  value *below* this entry (what `&` is here) and the *final* value,
  each with where it came from (file, line, layer); for a key that
  differs between contexts, the value per context, up to a limit.
- On `&`, `&self`, `&parent`, `&root...`, `&default` or a bind: the value it
  refers to, and the definition it comes from.
- On an id: a summary of what it names (type name, first lines of the
  definition, the layer).
- On a variant name: its declaration (group, parent).
- On an operator: the values of both sides and the result.

**Inlay hints** (optional, off by default): the result after an
expression that computes something (`& * 4` shows `= 2048`), in the
context chosen with the `context` setting.

**Completion.**

- Keys: in a block of a known type, the fields not written yet, each
  with its doc; accepting one fills in the value below if there is one
  (typing `rep` in a patch of `web` gives `replicas: 2`), else a
  placeholder of the right shape.
- Values: enum variants and type names that fit the field's type;
  `true`/`false`; ids filtered by `#[crol(refers(...))]`; `None`/`Some`.
- After `&root.`, `&self.`, `&parent.` and binds: the keys of the
  evaluated value at that path. After `&` (and `..&`): bind and
  template names, `self`, `parent`, `root`, `default`.
- In `<...>`: variant names, and in conditions `!`.
- After `..&root.`: root keys whose value is keyed.
- Hover on a template use shows the template and where it is defined;
  go to definition jumps there.

**Navigation.**

- Go to definition: on a root key, its definition entry in the nearest
  layer below or at the cursor (with every layer's as alternatives); on
  a bind, the bound entry; on an id, the key's definition; on a
  reference path, the entry that set the final value there.
- "Go to value below": the operation that produced `&` at the cursor,
  through go to definition on a bare `&`.
- Find references: for a root key, every operation on it in every
  layer, every reference to it, and every id naming it; for a bind, its
  uses.
- Document symbols (the outline): root entries with their entry kind
  and type name (`+web: Service`, `<prod> › web.replicas`), nested
  blocks as children. Workspace symbols: every root key.

**Code actions.**

- Write the values below as comments, for one entry: the fields of the
  value below that the entry does not set, inserted as `// field: value`
  lines.
- Add the missing fields of a struct, filled from the value below or
  with placeholders; or add `..`.
- Quick fixes from diagnostics: a misspelt field or id (by edit
  distance), `+x` on an existing key to a patch, `&name` without a bind
  to `&self.name`.

Nothing is written to a file unless the user accepts an action.

**Formatting** of a whole document, through the formatter (section
12.2). **Rename** of binds (within their scope) in the first version;
renaming root keys across files and layers comes later. **Semantic
tokens** tell apart what the grammar alone cannot: keys, type names and
enum variants (from the schema), early and late references, binds, and
operations that are definitions or patches.

**Settings** (initialisation options, changeable at run time):
`context` (the context for inlay hints and the first context shown on
hover; default the base), `inlay_hints`, `debounce_ms`, `max_contexts_in_hover`,
`remote_schemas` (section 4.1)
and `project` (a project file path, overriding the search).

**Application-specific checks.** Checks the format cannot do belong to
the application (section 0). An application may build its own server
binary from the `crol-lsp` library, passing a function that receives the
evaluated `CrolData` and returns diagnostics with spans; they are published
with the others. `crol lsp` is that library with no extra checks.

### 12.6 Tree-sitter grammar

`tree-sitter-crol` is a tree-sitter grammar of section 9, for editors'
highlighting, folding, indentation and structural selection, which work
without the language server and before a project file exists. It
accepts exactly what the Rust parser accepts: the conformance suite's
syntax cases are run through both, and CI fails when one accepts what
the other rejects. Its node names follow the AST's (`entry`, `target`,
`block`, `reference`, `bind`, `condition`, ...). Queries for
highlights, brackets, indents, outline, folds and text objects are
kept with the grammar, so every editor can use them.

### 12.7 Zed extension

The Zed extension lives in `editors/zed/`:

- `extension.toml`: id `crol`, the grammar (`tree-sitter-crol` from
  `https://github.com/DreamLogics/crol`, pinned to a commit; moved to a
  repository of its own if Zed's grammar loading needs one), and the language server for the language `CROL`.
- `languages/crol/config.toml`: the language (`.crol` files, `//` line
  comments and `/* */` block comments, brackets `{}`, `[]`, `()`, `<>`
  for conditions, auto-closing quotes), and the queries from 12.6:
  `highlights.scm`, `brackets.scm`, `indents.scm`, `outline.scm` (used
  until the server runs), `textobjects.scm`.
- `src/lib.rs`, compiled to WebAssembly: the extension's
  `language_server_command`. It uses, in order: the `binary.path` from
  the user's Zed settings for the `crol` server; a `crol` on the
  `PATH` of the worktree; a release binary for the platform, downloaded
  from the latest GitHub release of `DreamLogics/crol` and cached by
  the extension. It passes the
  `initialization_options` and `settings` from Zed's settings through
  (section 12.5).

The extension is first installed as a dev extension from the repository,
and published to Zed's extension registry once the grammar and server
are stable. Clients for other editors (VS Code, Neovim, Helix) need only
small configurations, which can follow later.

---

## 13. Errors at a glance

**Syntax**: invalid token, unterminated string or comment, missing
comma, unbalanced brackets, a file that is not UTF-8, `...`, a space
after `&` or `@`, `1u8`, `1.`, a
type name on the outer braces of a data-mode document; an identifier in
brackets (`[port]`); `.&parent` other than after `&parent`.

**Reading**: an unknown document attribute, one after the first
entry, or more than one `schema` or `type`; `type` in data mode, with a
top-level type name, or on an expression; an alias that is a reserved
word; a spread or `..` at the data-mode root, or a data-mode document
that is not entries; `extends` in value mode; a `schema` string that
is not a URL or crate; a module path that names nothing, both a file and
a directory, or goes above the root directory; a cycle of `extends`; two `use`s giving one name to different types; two definitions of a key under one condition in one layer;
a variant entry in a block without a fixed location; an unknown variant;
a condition that can never hold; `|`, `^`, `+` or parentheses in a
reference's variant selector; a `+` list inside a condition;
two documents with one path in a layer; `&name` with no visible bind;
two visible binds with one name, or a bind named like a template or
`self`, `parent`, `root` or `default`; a template defined twice; a value bind
inside a variant block; an entry bind at the top level of a data-mode
document; a bind on a remove, variant block,
spread, `..`, variant entry or inside a variant block; a variant
selector on `&` or `&default`.

**Evaluation**: `+` on something that exists; `-` or a path on something
that does not; declare or ensure on a list index or tuple element, set
on a missing index, removing a tuple element; a missing value referenced
outside `|`, or spread; `&default` without a schema; operators on the wrong kinds; integer overflow;
division by zero; a non-finite float; `~` on lists, scalars, different
type names; `.0` on a keyed value; an
integer key at the root; a spread of something that is not keyed; `&self`, `&parent` or a bind in a block
without a location; `&parent` above the top level; a cycle.

**Composer**: `with_variant` with an undeclared variant or a second
choice in a group; more contexts than `max_contexts` without
`contexts`; an invalid or unevaluated context in `contexts` or
`CrolData::context`; variants declared twice; a parent cycle, or a parent
of another kind or group.

**Application** (deserializer and options): an integer key in a struct;
unknown fields; missing
fields; a type name that does not match; a value of the wrong kind or
out of range; a plain string where `crol::Id` is required; variant key
rules (8.5). And the application's own checks, such as ids naming
nothing.

**Warnings**: patches to one key from several documents of one layer.
In tools also: a document whose `schema` differs from the project's,
and an `extends` naming something that is not a lower layer.

---

## 14. Project structure and quality

### 14.1 Crates

| Crate | Contents |
|---|---|
| `crol-syntax` | lexer, error-recovering parser, lossless CST, typed AST, formatter |
| `crol-eval` | lowering to operations and chains, the evaluator, `Value`, diagnostics, fingerprint |
| `crol` | facade: `CrolComposer`, `CrolConfig`, `CrolVariants`, `CrolLayer`, `CrolData`, serde `Deserializer` and `Serializer`, `from_str`, `to_string` |
| `crol-derive` | `#[derive(Schema)]` and `#[derive(VariantName)]`, re-exported by `crol` with the feature `derive`; the `Schema` trait and model live in `crol` (10.9) |
| `crol-lsp` | the language server, as a library (section 12.5) |
| `crol-cli` | the `crol` binary, including `crol lsp` |
| `tree-sitter-crol` | the tree-sitter grammar and its queries (12.6); not a Rust crate |
| `editors/zed` | the Zed extension (12.7) |

Dependencies are kept small: `serde`, `blake3`, `indexmap`; a CST
library such as `rowan` in `crol-syntax`; `clap` and a diagnostic
renderer (e.g. `codespan-reporting`) in `crol-cli` only; `lsp-server`
and `lsp-types` in `crol-lsp` only; `syn` and `quote` in `crol-derive`. Minimum
supported Rust version: the latest stable at the first release, then
raised only in minor releases. `#![forbid(unsafe_code)]` in every crate.

### 14.2 Tests

- **Conformance suite** in `tests/cases/<name>/`: `layers/<n>-<name>/`
  directories of `.crol` files, optional `options.crol` (the `CrolConfig`
  fields, itself in CROL value mode), and either `expected.crol` (the
  evaluated data as `crol eval --all-contexts` prints it) or
  `expected.errors` (diagnostics, one per line, as
  `file:line:col: message`). Every rule in this document should have at
  least one passing case and one error case.
- **Round trips**: parse → print gives the input back byte for byte;
  `fmt` is idempotent; `to_string` → `from_str` gives back the value,
  for property-tested serde types.
- **Fuzzing** (`cargo fuzz`): the parser never panics; the evaluator
  never panics or loops on any parsed input.
- **Schema checks**: every conformance case with a schema is checked
  both by deserializing into test types and by `crol::check`, and both
  must give the same errors.
- **Grammars**: the tree-sitter grammar and the Rust parser accept and
  reject the same syntax cases (12.6).
- **Language server**: tested in memory (`lsp-server`'s in-memory
  connection) on fixtures with cursor markers (`$0`): diagnostics,
  hovers, completions, definitions, references and code actions at given
  positions, and edits followed by re-evaluation.
- **Determinism**: fingerprints of the conformance suite are checked in,
  and CI checks them on x86-64 and AArch64, Linux, macOS and Windows,
  and on `wasm32`.

### 14.3 Milestones

1. Lexer, parser, CST, AST, formatter; `crol fmt`.
2. Value mode: evaluation of one document without layers or variants,
   `from_str`, `to_string`; `crol from-ron`.
3. Layers and chains, late references, laziness and cycle detection;
   `crol check`, `crol eval`.
4. Variants; fingerprint; `crol fingerprint`.
5. Provenance and `crol explain`; diagnostics polish; performance target
   (12.3).
6. Tree-sitter grammar and the Zed extension with syntax support only
   (highlighting, brackets, indents, outline). It needs only milestone
   1's grammar, so it can be done alongside 2 to 5.
7. `Schema` trait, derive, schema file and `crol::check`; document
   attributes; the project file; `crol check` with a schema.
8. Language server: diagnostics, document symbols, formatting; the Zed
   extension starts it.
9. Hover, inlay hints and completion.
10. Navigation, code actions, rename of binds, semantic tokens;
    publishing the Zed extension.

### 14.4 Releases

The project is hosted at `https://github.com/DreamLogics/crol`. A tag
`vX.Y.Z` publishes the crates to crates.io and makes a GitHub release
with the `crol` binary for x86-64 and AArch64 Linux (`musl`, static),
macOS and Windows, as `crol-vX.Y.Z-<target triple>.tar.gz` (`.zip` on
Windows), with SHA-256 checksums. The Zed extension finds the asset for
its platform by that name. All crates share one version number.

### 14.5 Licence

All code, the grammar, the Zed extension and this specification are
dual-licensed under MIT and Apache-2.0, at the user's option, as is usual
for Rust crates. Dependencies must have licences compatible with both.

### 14.6 Versioning

The format is versioned with the crates. Before 1.0, any release may
change the format. From 1.0:

- a **patch** release fixes bugs: a document that the implementation
  accepted, rejected or evaluated contrary to this specification
  may change behaviour;
- a **minor** release may add syntax, attributes, options and
  operators, so that documents that were errors become valid, but never
  changes the meaning of a valid document or its fingerprint;
- a **major** release may change anything, with the changes listed in
  the changelog and, where possible, a `crol migrate` that rewrites
  documents.

The schema file format (`format`, section 10.10) and the fingerprint
encoding (`CROL1`, appendix B) have their own version numbers, raised
only in major releases.

---

## 15. Open questions

1. **Clamping.** Patches stacked over several layers (`& - 2` three
   times) can push a value out of its range. `min`, `max` and `clamp`
   would cover this without comparisons in the language, but
   `min(a, b)` already parses as a named tuple, so they need their own
   syntax.

---

## Appendix A. Design decisions

What this specification settled beyond the original concept, and why
where it is not obvious.

- Data mode and value mode (section 4); value mode makes CROL usable for
  single config files.
- Chars, `0o`/`0b` literals, `.0` tuple selectors.
- `self`, `parent`, `root` and `default` are keywords only after `&`.
- One notation for structs and maps: keys are strings or integers,
  written bare (`port`, `"cache url"`, `12`), with `[key]` as the
  accessor in paths. Unit-variant, id, boolean and char keys are gone;
  enum and id map keys are written as strings (3.1, 5.1).
- Root keys are ids, written `web` or `"odd id"`.
- Definitions versus patches (7.2): a set that uses `&` is a patch, so
  several files may patch one key with `x: & ~ {...}`.
- `&` binding inside blocks, block locations, and `self`/`parent`
  defined (6.1).
- Merging is shallow, and `~` takes any keyed value on the right (6.4).
- The defaults flag and defaulted fields replace "complete unless it
  spreads a value": a spread passes on the flag of what it spreads
  instead of always allowing defaults (3.1, 5.2). `Option` fields must
  be written unless the block has `..` (10.3).
- Laziness with shape and contents levels, and what counts as a cycle
  (7.5); `|` checks its left side to its own level only (6.5).
- String concatenation with numbers, chars and booleans (6.6).
- Variants are combinable: `Discrete` flags and `Grouped` alternatives,
  declared with `CrolVariants<T>`, where `T` may be the application's
  own type through `crol::VariantName` (8.1),
  contexts as sets of active variants, conditions with `!`, `&`, `|`,
  `^` and the `+` list, CSS-like specificity ordering, nesting as
  conjunction, and selectors in references that change the context (8).
- Variant selectors anywhere in a path, the `variant_keys` option, and
  where variant entries may appear in blocks (8).
- Layer and file ordering, and the fingerprint's encoding (7, appendix B).
- Serde mapping, including the newtype-variant rule for mixed-type root
  maps, `crol::Id`, and id use reporting (10).
- Decided: the name is CROL (Composable Rust Object Language), the extension `.crol`.
- Decided: variant parents, declared by the application only (8.1, 8.3).
  This lets one variant's patch extend another's for the same key;
  applying another *key's* patch stays out.
- Decided: a field written by a serde alias is present for the
  completeness rule; the deserializer learns aliases from the schema
  (10.2).
- Decided: no comparisons or conditionals, now or later; input
  validation belongs on the Rust side (wrapper types, derive
  attributes). `<v>` is final as the variant selector.
- Decided: editor support is part of the project: a schema built from
  the type system through a `Schema` trait and derive, and written to a
  file by a test (10.9), not generated from source in `build.rs`, which
  could not see other crates' types, macros or defaults;
  `#![use(...)]` resolves clashing type names in a document and
  `#![schema(...)]` names a document's schema: a file, a URL, or a
  crate's published schema; `#![type(Name)]` types a braceless
  value-mode document; `#![extends(...)]` names the documents a
  document is written on top of, always followed by tools and by the
  loader with `load_extends` (4.1); a project file for tools (12.4), a language
  server shipped as `crol lsp` in the one binary (12.5), a tree-sitter
  grammar kept in step with the Rust parser (12.6), and a Zed extension
  (12.7).
- Decided: files are named by module paths (`::from::root`,
  `from::here`, `super::x`), never by file system paths, so documents
  are platform-agnostic (4.2). Code may also load by file system path.
- Decided: the Rust API is a builder, `CrolComposer`, configured by
  `CrolConfig`, that loads layers, chooses a context and composes
  straight into a Rust type (10.7).
- Decided: `&default` refers to a target's Rust default, recorded in the
  schema (6.1.3). The project file is `project.crol` (12.4). Licence
  MIT/Apache-2.0; the format is versioned with the crates (14.5, 14.6).
- Decided: no operator for removing several keys; a patch does it
  (`& ~ { -a, -b }`).
- Decided: no implicit `&name` lookup. `&self`, `&parent` (chained as
  `&parent.&parent`), `&root` and binds (`name = key: ...` for an entry,
  `name := expr` for a value; templates are top-level value binds)
  instead; entries copied by a spread are evaluated at their new
  location (6.1).

## Appendix B. Canonical encoding

Used for the fingerprint only. All integers in lengths and counts are
unsigned LEB128. Each value is a tag byte and a payload:

| Tag | Value | Payload |
|---|---|---|
| `0x00` | `()` | none |
| `0x01` | boolean | `0x00` or `0x01` |
| `0x02` | integer | 16 bytes, two's complement, little-endian |
| `0x03` | float | 8 bytes, IEEE bits, little-endian |
| `0x04` | char | 4 bytes, scalar value, little-endian |
| `0x05` | string | length, UTF-8 bytes |
| `0x06` | id | length, UTF-8 bytes |
| `0x07` | list | count, values |
| `0x08` | tuple | count, values |
| `0x09` | unit name | name |
| `0x0A` | named tuple | name, count, values |
| `0x0B` | keyed value | name (empty for none), flag byte, count of defaulted keys and the keys (integers ascending, then strings in byte order), count of entries, then per entry its key and value in insertion order |

A name is a length and UTF-8 bytes. A key is encoded as a value (tag
`0x02` or `0x05`). The data is encoded as: the byte string `CROL1`; the
count of root keys, then each key (as a string) and value in root order;
the count of evaluated non-base contexts, then for each context, ordered
by its sorted list of chosen variant names (ancestors left out): that
list (count, names), the count of keys that differ from the base or
exist only on one side, and each such key with a byte `0x00` (removed in
the context) or `0x01` and its value, in root order with context-only
keys after.
