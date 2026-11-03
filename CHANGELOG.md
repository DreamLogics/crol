# Changelog

All notable changes to this project are documented in this file. All crates
share one version number.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Before 1.0, any release may change the format (`SPEC.md` 14.6).

## [Unreleased]

### Added

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
