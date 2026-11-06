# Changelog

All notable changes to this project are documented in this file. All crates
share one version number.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Before 1.0, any release may change the format (`SPEC.md` 14.6).

## [Unreleased]

### Changed

- The language server evaluates on a thread of its own, and answers requests at once
  from the latest evaluation. It keeps what it compiled, so hover, go to definition and
  completion look at one key rather than everything, and keeps read-only layers in their
  binary form between evaluations.
- A completion list of more than 500 items holds those that match what is typed, the
  closest first, and is marked incomplete, so that the editor asks again.
- The language server publishes each file's diagnostics the first time, and after that
  those that changed and those of open files.
- `crol check` is about ten times as fast on large data: a context is checked by what
  differs from the base, on threads, and a directory is read without looking at each
  file in it. Documents are not parsed to find attributes they do not have.

## [0.1.0] - 2026-11-05

The first release, with the format as `SPEC.md` describes it:

- `crol`, the library: layers with `CrolComposer`, and serde `Deserializer` and
  `Serializer` implementations, with `#[derive(crol::Schema)]` behind the `derive`
  feature and schemas by URL or crate behind the `remote-schemas` features.
- `crol-syntax` and `crol-eval`, the parser, formatter and evaluator on their own.
- `crol-cli`, the `crol` tool, also as binaries for x86-64 and AArch64 Linux, macOS
  and Windows, and `crol-lsp`, the language server.
- The tree-sitter grammar and the Zed extension, from the repository.

## Before 0.1.0

What changed during development, before anything was published.

### Added

- Hover on a built-in macro shows how a call is written and what it does.
  `crol_eval::macro_help` gives the same.
- Doc comments in data (`SPEC.md` 2.2): `///` above an entry and `//!` at the top of
  a file. Hover shows those of every layer, lowest first, and completion after `@`
  those of root keys. `DataWriter::doc` writes one. Evaluation does not see them.
- `crol::Expression`, the text of one expression, which the serializer and
  `DataWriter` write as it is where a value goes, for references (`SPEC.md` 10.8).
- Compiled layers (`SPEC.md` 7.8): `crol compile` and `CrolLayer::to_binary` write a
  `.crob` file, a layer lowered ahead of time and compressed, that loads without
  parsing, about five times faster on large data. It evaluates exactly as its
  documents do, and `CrolLayer::compiled_in` says whether a file still matches its
  sources.
- `CrolData::reload`, which applies changes to documents and evaluates again, says
  which root keys changed, and leaves the data as it was if the result does not load
  (`SPEC.md` 10.8).
- `CrolData::key_fingerprints` and `crol::changed_keys`, which say which root keys
  differ between two data sets (`SPEC.md` 7.6).
- Constraints (`SPEC.md` 10.12): `#[crol(constraint = "...")]` and `key_constraint`
  name one, `#[crol(constraints(...))]` and `open_constraints` define `Min`, `Max`,
  `Range`, `OneOf` and `Refers`, and a `schema.crod` at the root of a layer defines
  and extends them. `crol check`, `CrolComposer` and the language server enforce them.
  The schema file format gains `constraints`, and fields gain `constraint`,
  `key_constraint` and `sealed`, all optional.
- Sealed fields (`SPEC.md` 10.13): with `#[crol(sealed)]`, only the layer that defines
  a value can set the field, however a higher layer would change it. Hover shows
  constraints and sealed fields, and completion leaves sealed fields out above the
  defining layer.
- Views and queries (`SPEC.md` 10.11): `#[derive(crol::View)]` reads fields by path,
  `CrolData::distinct` and `variations` give a key's distinct values across contexts,
  and `CrolData::query` chooses keys by type or by what they hold.
- `CrolData::evaluate`, which evaluates another context later without reading the
  layers again, and `each_variant`, which evaluates the base and one context for
  each variant instead of every combination (`SPEC.md` 8.1). `CrolData` is `Send`
  and `Sync`.
- `crol::DataWriter`, which writes data-mode documents: declarations, patches,
  diffs between two values and variant blocks (`SPEC.md` 10.8).
- The macros `round!`, `floor!`, `ceil!` and `clamp!` (`SPEC.md` 6.7).
- `~^`, a shallow merge, and `^key: value`, which replaces a value where a block
  would merge (`SPEC.md` 5, 5.2, 6.4).
- `init_defaults`, in `CrolConfig` and the project file's `options`: with a schema,
  a path goes through a field that was left out, or an `Option` that is `None`, from
  its default, and `&` and merging blocks start from it (`SPEC.md` 5.1).

### Changed

- Errors from `+`, `-` and path entries say whether a key is set, not whether it
  exists, which also reads right for a struct field: ``+: `name` is already set``,
  ``-: `name` is not set``, `` `web` is not set, so it cannot be patched`` (`SPEC.md` 5).
- The formatter's default line width, and `PrettyConfig`'s, is 80 instead of 100,
  and more long lines break (`SPEC.md` 12.2). A binary expression breaks after an
  operator, with what follows indented one more level, and a `#![use(...)]` goes one
  path per line. A named tuple whose only argument is a block, list or tuple breaks
  inside it, as `List([` and `])`. Run `crol fmt` on your files, or pass `--max-width 100` to keep the
  old width.
- Loading holds each document's text once, and `CrolConfig::keep_lowered: false` leaves
  out what only `reload` needs of a `.crob` layer: 8 and 14 MB less on large data.
- `~` and merging blocks merge named tuples of one name element by element, so
  `& ~ Active({ range: 5 })` changes `range` and keeps the rest, where it used to
  replace the whole payload without a word (`SPEC.md` 5.2, 6.4). Write `^key` to replace.
- `f32`s are written as the shortest text that reads back the same: `0.1`, not
  `0.10000000149011612`.
- The warning about patches from several documents only comes where their order
  really depends on file names: not for patches under conditions that never hold
  together in the evaluated contexts, or that specificity puts in order.
- Evaluation takes about 45% less memory at its peak on large data with many
  contexts, and a context evaluated later no longer evaluates the base again.
- Evaluation takes about 45% less memory at its peak on large data and 60% less time,
  as what a root key holds is freed once its value is computed.
- Large data is parsed on up to 8 threads, which takes about a third off compiling, for
  about 50 MB more at the peak with glibc's allocator.
- On large data, the contexts after the base are evaluated on up to 3 threads, which
  takes about a third off evaluating 10 contexts, for about 25 MB more at the peak.
  `threads` in `CrolConfig` and `DataOptions` sets how many, and `Some(1)` uses none. The
  result is the same either way.
- **Breaking:** `Key::Str` holds an `Arc<str>` rather than a `String`, so that values
  with the same field share its name. Write `Key::Str("port".into())`.
- **Breaking:** `ValueKind::String`, `Id`, `UnitName` and `NamedTuple`, and
  `Keyed::name`, hold an `Arc<str>`, so that a value shares its text with what it was
  evaluated from. Write `ValueKind::String("text".into())`.
- **Breaking:** `ValueKind::Keyed` holds a `Box<Keyed>`, which makes every other value
  smaller.
- **Breaking:** `~` merges deep: a block inside a patch merges with what the key
  holds, at any depth, where it used to replace it. So do a path that ends in a
  block, a spread, and a block after a spread. A block of another type replaces,
  as a value that is no block does. To keep the old meaning, write `^key` or `~^`
  (`SPEC.md` 5.2, 6.4). Data that relied on the old meaning changes value, and so
  does its fingerprint.
- **Breaking:** `&` in an element of a tuple, or of a named tuple of the same name
  as the value below, reads the same position of it, so `OneOf(& + ["x"])` adds to
  the list in the `OneOf` below. Below any other value it is still that whole value
  (`SPEC.md` 6.1).
- **Breaking:** `&` in an entry with nothing below where it was written, as in a
  template, reads the value below where the entry lands, so a template can be a
  patch: `stronger := { hp: & + 5 }`, `unit: & ~ &stronger` (`SPEC.md` 6.1).
- **Breaking:** in layered data, a struct as the schema's root type is the whole
  document: a root key is a field of it, in `crol check`, `crol::check`,
  `fill_defaults`, `&default` and the language server, and completeness is that of
  the composed root, never of one file. A map is the whole document as well, its
  keys the root keys. An enum, as `Definition` is, stays the type of each root
  value, and so does a type with no keys; a newtype or transparent type goes by
  what it wraps; `root` of the project file goes by the type it names; value mode
  does not change (`SPEC.md` 4.1). So a schema built from a struct for data whose
  every root value is that struct now reports `Service has no field web`: build
  it from `IndexMap<String, Service>`, or a transparent wrapper of that, instead.
  `crol::schema::build` now names a root type with no name, such as a map, with an
  entry in `types`, so it is checked; before, nothing was. The schema file format
  does not change.
- **Breaking:** `ContextData` has the fields `defaults` and `defaulted`, for the
  marks of its root, and `crol::check` gives `missing field` for a required field
  removed with `-field`, as the deserializer always did.
- `..` is allowed at the root of a data-mode document, also in a root variant
  block, so `compose::<T>()` of a struct can leave fields out. One in any layer
  counts, in the contexts its condition holds in. A spread at the root stays an
  error. The fingerprint changes only for data that writes such a `..` (`SPEC.md`
  7.4, 10.3, appendix B), and `crol eval` and `--all-contexts` print it, so they
  read back.
- `-key` at the root is as `-field` in a block for a root struct: the field takes
  its default without `..`, and a required one is `missing field` (`SPEC.md` 7.4,
  10.3). What it does to the data, the fingerprint, and any other root is unchanged.
- `DefaultsProvider` has a method `root_is_document`, and `&default` at a root key
  of a struct root is the default of that field. `SchemaFile::defaults_for` takes
  the root type a project chooses, which `&default` did not look at before.
- Language server: at the top of a `.crod` document with a struct root, completion
  offers the root's fields, except those the document writes, with the value of a
  lower layer filled in, hover shows each field's doc, on key and value, and every
  place below the root has the right type, by one rule (`SPEC.md` 12.5). The quick
  fixes for a missing root field add it, or `..`, at the end of the document.
- Language server: hover shows the doc comment first, then the type on its own
  line, in Markdown that renders. A value has the doc of the field it is assigned
  to, also in a list, a map or `Some(...)`, at any depth, and a variant its own
  doc. A field with no doc shows its type, and where nothing is known there is no
  hover. The evaluated value is gone from hover, except on a reference or id, on
  one line. The layers are shown after the doc, for a key in data with more than
  one operation (`SPEC.md` 12.5).
- A schema file now has a real schema: `crate:crol` describes `Schema` and every
  type below it, with docs, instead of taking `types` as `Any`, and
  `#![schema("crate:crol@...")]` makes a `.crol` document a schema file, which
  `crol check` and the language server check against the schema built in, with no
  lookup, cache or network (`SPEC.md` 10.10). So a schema file with a mistake below
  `types` that went unnoticed in `crol check` is now reported. The format and what
  `build` and `to_text` write for your types are unchanged; `build::<SchemaFile>()`
  and the docs in `build::<Project>()` are what changed.
- The language server's `max_contexts_in_hover` setting is no longer documented; it
  never did anything, and is still accepted.
- A file's extension now states its mode (`SPEC.md` 4): `.crol` is value mode, the
  plain default, and `.crod` is data mode, the opt-in for layers and the more
  advanced features, so the name of a file tells how it behaves. Two modes stay,
  as they serve different purposes. Rename layers to `.crod`; single values, schema
  files and the project file stay `.crol`.
  Value documents named `.crov` on the way here are `.crol` again: rename them,
  as `.crov` is now an extension like any other, with no hint or fallback.
- A file whose extension is wrong for how it is loaded is an error, never a
  silent skip: a `.crol` file given as a layer or found in a layer's directory,
  and a `.crod` file read as a project or schema file. File loaders' sources are
  held to the same rules when their names have one of these extensions; other
  names follow the API that loaded them. `crol::from_str` is always value mode
  and the composer always data mode, also for `CrolLayer::from_sources`.
- Module paths resolve by what they are for: `data::base` and `extends` find
  `.crod` files and directories, and `schema` attributes and `Project.schema` find
  `.crol` files. `crol::resolve_module_path` takes a `Mode`, `ModulePath::mode()`
  tells a file loader which is looked for, and `FileLoaders::resolve_value`
  resolves a value document.
- `crol check`, `crol eval` and `crol fingerprint` read one `.crol` file in value
  mode, with its schema. `crol explain` refuses one, and points to `crol eval`.
- `crol fmt` takes `.crod` and `.crol` files.
- The language server chooses the mode by the extension, in a project or not. A
  `.crod` file outside the layers is evaluated as data, with its variants and
  patches, and its own variants are completed. A problem with the project is now
  shown on the file to blame.
- `crol --init` writes `project.crol` and `data/base/main.crod`.
- `crol eval` and `crol explain` tell a key from a layer by the form of the last
  argument, not by what exists on disk (`SPEC.md` 11), so `crol eval web.port` and
  `crol explain web.port` work in a project. A bare name in the last place is now
  always a key: write `./base` or `::base` for a layer there.
- `crol eval` and `crol fingerprint` of layers exit with status 1 on schema errors,
  and print no data or fingerprint, as they already did for a value document and
  for every other error (`SPEC.md` 11).
- The language server follows `#![extends(...)]`, in a layer and in a document on
  its own, through the composer `crol check` uses, with open buffers over the
  files. Hover, go to definition, completion and inlay hints see what is extended,
  a document follows changes to it, and an `extends` that fails is shown on the
  attribute. It asks the client to watch the workspace's CROL files.
  `CrolComposer::load_layer_tolerant`, `FileLoaders::with_file_reader` and
  `FileLoaders::resolve_path` are what it uses.
- A layer that does not load no longer stops the language server loading the
  ones after it. A stray `.crol` file is left out of its layer and has
  the error; a layer that cannot be loaded adds nothing, and has the error on the
  file to blame. Errors above what is missing are warnings that say why, and every
  problem is shown, not only the first (`SPEC.md` 12.5).
- `Project.files` now does what it says: a `.crol` file that a glob matches is
  checked as a value of that type against the project's schema, by `crol check`,
  `crol eval`, `crol fingerprint` and the language server, with completion and
  hover to match. Its globs are specified (`SPEC.md` 12.4), and one with a wrong
  form is an error in the project file. A `.crol` file that names a schema or that
  no glob matches is treated as before.
- A document on its own keeps its last good evaluation while it does not evaluate,
  as layers did, so accepting a field fills in its value and hover shows one, saying
  that it is from the last evaluation. Hover in a layer does this too.
- In the language server, a block of an internally tagged, adjacently tagged or
  untagged enum offers its fields, hover on a dotted target is about the segment
  under the cursor, and inlay hints cover elements of lists, tuples and
  `Name(...)`, such as `[1 + 1]`.
- `&default` works in `crol` and in the language server, not only through
  `CrolComposer::schema`: they read defaults from the schema they check against
  (`SPEC.md` 6.1.3). `SchemaFile::defaults` gives them to a tool of your own.
- `CrolLayer::without_file` is gone: `FileLoaders::with_project_file` names the
  one `.crol` file a layer's directory may hold.

### Added

- Language server: completion and hover at every level of a schema file, and the
  paths in `types` offered in `root` and `Named("...")`, in the quotes too.
- `crol::schema::is_schema_file` and `crol::schema::SCHEMA_PATH`.
- `crol::Mode`, `crol::value_type`, and
  `crol_eval::fingerprint::fingerprint_value`.
- A line break separates entries, in addition to the comma (`SPEC.md` 5.3).
  An entry goes on across lines until the next line begins an entry; lists
  and tuples stay comma-separated. `crol fmt` and `to_string_pretty` leave
  out the commas after top-level entries and keep them in nested structures.
  One change of reading: `a: &x` then `-b` on the next line, a subtraction
  that could only fail, is now two entries.
- `crol-syntax`: lexer, error-recovering parser producing a lossless rowan
  syntax tree, typed AST, and line/column index.
- `crol-syntax`: the formatter (`SPEC.md` 12.2).
- `crol-cli`: `crol fmt [--check] [--max-width N] <file>...`.
- `crol-eval`: evaluation of value-mode documents: literals, operators,
  blocks with spreads, removes and path entries, `&`, `&self`, `&parent`,
  `&root`, binds and value binds, merge and fallback, lazy evaluation with
  cycle detection, and diagnostics that do not stop evaluation.
- `crol`: serde `Deserializer` and `Serializer`, `from_str`,
  `from_str_with_ids`, `to_string`, `to_string_pretty`, and `crol::Id`.
- `crol-cli`: `crol from-ron <file>`.
- Limits on nesting, reference depth and evaluation work (`SPEC.md` 7.7).
- Layers (`SPEC.md` 7): data-mode documents, chains of definitions and
  patches per root key, templates shared by every layer, and the warning
  for patches from several documents of one layer.
- `crol`: `CrolComposer`, `CrolConfig`, `CrolLayer` and `CrolData`; layers
  by module path (`SPEC.md` 4.2) or file system path, and `load_extends`.
- `crol-cli`: `crol check` and `crol eval`, and `--message-format json`.
- The conformance suite in `tests/cases/` (`SPEC.md` 14.2).
- Variants (`SPEC.md` 8): `Discrete` and `Grouped` variants with parents,
  contexts, conditions with specificity, variant entries inside blocks,
  references that read other contexts, the variant keys rules, and
  diagnostics that say in which contexts they occur.
- The fingerprint (`SPEC.md` 7.6, appendix B), checked in for every
  conformance case.
- `crol`: `CrolVariants<T>`, the `VariantName` trait, `with_variant`, and
  contexts in `CrolData`.
- `crol-cli`: `crol fingerprint`, `crol eval --context` and
  `--all-contexts`, and `--discrete`, `--group` and `--parent`.
- Provenance (`SPEC.md` 12.3): `CrolData::explain` and `crol explain` list
  the operations behind a value, with the value after each.
- Contexts evaluate only the keys that can differ from the base, and store
  only their differences.
- Schemas (`SPEC.md` 10.9, 10.10): the `Schema` trait, `crol::schema::build`,
  `write` and `check_file`, and schema files read and written as CROL.
- `crol-derive`: `#[derive(Schema)]` and `#[derive(VariantName)]`, behind
  the `derive` feature of `crol`.
- `crol::check`: schema checks of evaluated data without the Rust types,
  including ids.
- `&default` (`SPEC.md` 6.1.3), with `CrolComposer::schema`.
- The language server (`SPEC.md` 12.5): `crol lsp` and the `crol-lsp`
  library, with diagnostics (syntax at once, evaluation after a pause in
  typing), document symbols, formatting, hover with the value and its
  history, completion of root keys, references and variants, and go to
  definition.
- The Zed extension starts the language server, from the settings, the
  `PATH`, or a downloaded release.
- The project file (`SPEC.md` 12.4); `crol` commands read their layers,
  variants, options and schema from it when given no layers, and take
  `--project` and `--schema`.
- Language server: find references, rename of binds, workspace symbols,
  semantic tokens, inlay hints (off by default), and code actions (quick
  fixes for diagnostics, missing fields, the values below as comments);
  completion and hover from the schema.
- `#![schema(...)]` names the schema a document is checked against, in
  the language server and `crol check`; `crol::check_value` checks a
  value-mode document.
- `crol_eval::operations` and `crol_eval::operation_values`, for editors.
- Conformance cases with a schema are checked both by deserializing and by
  `crol::check`.
- Fuzz targets in `fuzz/` for the parser and both evaluation modes.
- A limit of 256 conditional operations for one entry (`SPEC.md` 7.7).
- `crol` provides the schema of the project file (`Schema` for `Project`
  and the types in it), and the language server checks the project file
  against it.
- A field written by a serde alias counts as present for completeness,
  in `crol::check` and, given `CrolComposer::schema`, in the deserializer.
- Schemas from URLs and crates (`SPEC.md` 4.1), fetched over HTTPS and
  cached: `remote-schemas` (blocking, with ureq) and `remote-schemas-async`
  (Tokio and reqwest) features of `crol`; `crol-cli` and `crol-lsp` use the
  async one by default, and the language server fetches in the background.
  The language server's `remote_schemas` setting and `crol --offline` turn
  fetching off. `crate:crol` is built in.
- `crol eval` with a schema writes out the defaults that fields removed
  with `-field` or left out with `..` take, marked `// default`
  (`--no-defaults` turns this off); `crol::fill_defaults` and
  `crol_eval::print::to_document_noted`.
- A warning for a layer document whose `#![schema(...)]` names another
  schema than the project's (or `--schema`), in `crol check` and the
  language server; `crol::schema::attribute_disagrees`.
- Language server: go to definition on `&` goes to the value below.
- Language server: completion and hover in documents on their own come
  from the schema they name, and in the project file from crol's.
- File loaders (`SPEC.md` 10.7): the `FileLoader` trait resolves module
  paths to sources and loads them, so that layers and `extends` can come
  from anywhere. `CrolComposer::with_file_loader` registers one, chained by
  order with fallthrough to `FsLoader`, the code that read files before and
  still the default; `without_file_system` turns that off.
  `CrolConfig::file_loaders`, `FileLoaders`, `ResolvedSource`, `SourceId`
  and `CrolError::source_id`. Exhaustive `CrolConfig` literals need the new
  field. The language server's open buffers are a file loader now.
- `crol --init` (or `crol init`) sets up a Cargo project: a `project.crol`
  next to `Cargo.toml` and a first layer in `data/base` (`SPEC.md` 11).

### Fixed

- A key with an operation whose condition holds in the base but not in a context, such
  as `x<!b>`, kept its base value in that context, or stayed where the context should
  remove it. It now has the value of its chain there (`SPEC.md` 8.3).
- A key that copied a reference to a block around it, as `tank: { ..&root.unit.stats }`
  does with `stats: { defense: &parent.armor }`, kept its base value in a context where
  what the reference reads where it landed differs (`SPEC.md` 6.1).
- `crate:crol` no longer stays on what an earlier build of the same version left
  in the cache.
- A cycle between root keys (`+a: &root.b`, `+b: &root.a`) and a root key that
  holds the root (`+a: &root`) are reported with their spans. `crol` exited with
  status 1 and no message, and panicked with `--message-format json`.
- No diagnostic is lost or panics because it is about no place in a file, such as
  a path that is not one given to `crol explain`: it has no snippet, and in JSON
  its `file`, `line` and `column` are `null` (`SPEC.md` 7.7). `Sources::get` is
  new, and `Sources::location` and `Diagnostic::display` accept a detached span.
- `crol_eval::parse_path` refuses text that only holds a path, such as `+a`.
- A bare file name is in the working directory: `crol check x.crod` finds a
  `crate:` schema as `./x.crod` does, also from below the Cargo package, and so
  does `--project project.crol`, whose layer command runs too. `super::` works
  from a relative path, such as `crol check sub/x.crod`. `crol::directory_of` is
  new.
- A step of a path entry that does not exist says so as a key does: `web.nope.x:
  `web.nope` does not exist, so it cannot be patched`, not ``:: `.nope` does not
  exist``.
- Errors in a project file or a schema file point at that file, not at
  `<string>:1:1`, and errors in what a layer command prints at `<output>`.
- `crol --init` says to add `crol` from the repository, as it is not on crates.io
  yet, and its project file suggests `crol/schema.crol`, as the guide does.
- ``unexpected `:` after an entry`` says what to look for: a key was read as part
  of the value before it, as in `+a: 1 +b: 2`.
- `SPEC.md` 10.9 now shows the four arguments of `crol::check`, says that
  `IndexMap` needs no feature, and no longer promises `Schema` implementations
  for `glam` and `chrono`, which never existed: such fields take
  `#[crol(schema = ...)]`.
- A directory named by two `extends` is loaded once, and an `extends` of a
  directory leaves out the documents already loaded (`SPEC.md` 4.1).
- The command line reports an `extends` that fails. It dropped the layer and went
  on, so a cycle of `extends` printed nothing and exited with status 0.
- The documents of a directory layer, and so the order of the `extends`
  they load, no longer depend on the order the platform lists files in
  (`SPEC.md` 7.1).
- `#[crol(explicit_defaults)]` and `#[crol(explicit_defaults = false)]`
  (`SPEC.md` 10.3): a struct, enum or struct variant decides whether fields
  left out take their defaults only with `..`, before `require_complete`.
  It is recorded in the schema as `explicit_defaults`, which leaves the
  format's version and schemas that do not use it as they were, and
  honoured by `crol::check`, `crol check`, the language server, `crol eval`
  and, given the schema, the deserializer.
- `crol::from_str_with_schema`: `from_str` with a schema, for aliases and
  `explicit_defaults`.

### Fixed

- Fields of a struct with `#[serde(default)]` are no longer `Required` in
  the schema, so `crol::check` accepts `Limits { .. }` as serde does.
- `crol::check` with `require_complete` off reports a missing field that
  has no default, as deserializing does.
- `#![schema(::path)]` resolves `::` from the root directory (the project
  file's, else the working directory or workspace folder), and relative
  paths from the document's directory (`SPEC.md` 4.2).
- Unterminated string and char literals no longer make evaluation panic.
- The parser reports tokens it skips while recovering, as in
  `<a * b>`; the formatter used to accept and garble them.
- The formatter keeps the space in `1 .0`, which would become a float.
- Evaluation no longer leaks memory through cells that hold each other:
  a value bind that is never read, or a value that holds itself.
- Language server: completion finds the type at the cursor at any depth:
  in list and tuple elements, `Some(...)` and other payloads, struct
  variants, after a `.` in a target, and with unfinished lines at the
  cursor or above it. Values are offered in lists and payloads too, and
  variant blocks offer the fields set in the block around them.
- Language server: hover, inlay hints, code actions and `&self` work
  inside list elements, tuples and payloads; hover shows the type in
  struct variants; integer map keys are found.
- Language server: completion no longer ends the server after text
  outside ASCII, and offers nothing in strings and comments.
- Language server: completion items carry a `textEdit` for the word at the
  cursor, keep an existing `:`, and are triggered where keys and values
  start; a document just opened is evaluated at once.

[Unreleased]: https://github.com/DreamLogics/crol/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/DreamLogics/crol/releases/tag/v0.1.0
