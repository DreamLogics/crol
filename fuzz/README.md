# Fuzzing

Fuzz targets for CROL (`SPEC.md` 14.2), run with
[`cargo fuzz`](https://github.com/rust-fuzz/cargo-fuzz) on nightly:

| Target | What it checks |
|---|---|
| `parse` | The parser never panics and gives its input back byte for byte; the formatter is idempotent on what it accepts. |
| `evaluate_value` | Evaluating a value-mode document never panics or loops. |
| `evaluate_data` | Evaluating layers never panics or loops. The input is split at `\0` into documents, one layer each. |

```sh
cargo install cargo-fuzz
cargo +nightly fuzz run evaluate_data fuzz/corpus/evaluate_data tests/syntax/valid tests/cases \
    -- -timeout=10 -rss_limit_mb=2048 -max_len=4096
```

The test files make a good seed corpus. Limits matter: a stall shows up
as a timeout and runaway memory as an out-of-memory report, and both are
bugs, since the limits of `SPEC.md` 7.7 should stop every input.

CI runs each target for a minute on every push. Add an input that
found a bug to the `fuzz_findings` test in
`crates/crol-eval/tests/robustness.rs`, or to the syntax corpus in
`tests/syntax/`.

This directory is a workspace of its own and is not published.
