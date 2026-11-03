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
