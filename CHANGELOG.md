# Changelog

All notable changes to this project are documented in this file. All crates
share one version number.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Before 1.0, any release may change the format (`SPEC.md` 14.6).

## [Unreleased]

### Changed

- A file's extension now states its mode (`SPEC.md` 4): `.crod` is data mode and
  `.crov` is value mode, so the name of a file tells how it behaves. Two modes
  stay, as they serve different purposes. `.crol` is retired: rename layers to
  `.crod`, and single values, schema files and the project file to `.crov`.
  A `.crol` file is an error that names the extension to use, everywhere and with
  no fallback.
- A file whose extension is wrong for how it is loaded is an error, never a
  silent skip: a `.crov` file given as a layer or found in a layer's directory,
  and a `.crod` file read as a project or schema file. File loaders' sources are
  held to the same rules when their names have one of these extensions; other
  names follow the API that loaded them. `crol::from_str` is always value mode
  and the composer always data mode, also for `CrolLayer::from_sources`.
- The project file is `project.crov`. A leftover `project.crol` is a rename
  error, not "no project". `Project::find` returns a `Result`.
- Module paths resolve by what they are for: `data::base` and `extends` find
  `.crod` files and directories, and `schema` attributes and `Project.schema` find
  `.crov` files. `crol::resolve_module_path` takes a `Mode`, `ModulePath::mode()`
  tells a file loader which is looked for, and `FileLoaders::resolve_value`
  resolves a value document.
- `crol check`, `crol eval` and `crol fingerprint` read one `.crov` file in value
  mode, with its schema. `crol explain` refuses one, and points to `crol eval`.
- `crol fmt` takes `.crod` and `.crov` files, and refuses a `.crol` file.
- The language server chooses the mode by the extension, in a project or not. A
  `.crod` file outside the layers is evaluated as data, with its variants and
  patches, and its own variants are completed. A problem with the project is now
  shown on the file to blame.
- `crol --init` writes `project.crov` and `data/base/main.crod`.
- The language server follows `#![extends(...)]`, in a layer and in a document on
  its own, through the composer `crol check` uses, with open buffers over the
  files. Hover, go to definition, completion and inlay hints see what is extended,
  a document follows changes to it, and an `extends` that fails is shown on the
  attribute. It asks the client to watch the workspace's CROL files.
  `CrolComposer::load_layer_tolerant`, `FileLoaders::with_file_reader` and
  `FileLoaders::resolve_path` are what it uses.
- A layer that does not load no longer stops the language server loading the
  ones after it. A stray `.crov` or `.crol` file is left out of its layer and has
  the error; a layer that cannot be loaded adds nothing, and has the error on the
  file to blame. Errors above what is missing are warnings that say why, and every
  problem is shown, not only the first (`SPEC.md` 12.5).
- `Project.files` now does what it says: a `.crov` file that a glob matches is
  checked as a value of that type against the project's schema, by `crol check`,
  `crol eval`, `crol fingerprint` and the language server, with completion and
  hover to match. Its globs are specified (`SPEC.md` 12.4), and one with a wrong
  form is an error in the project file. A `.crov` file that names a schema or that
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
  one `.crov` file a layer's directory may hold.

### Added

- `crol::Mode`, `crol::is_retired`, `crol::value_type`, and
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
