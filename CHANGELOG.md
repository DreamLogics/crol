# Changelog

All notable changes to this project are documented in this file. All crates
share one version number.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Before 1.0, any release may change the format (`SPEC.md` 14.6).

## [Unreleased]

### Added

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
