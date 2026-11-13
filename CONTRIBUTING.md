# Contributing

## The specification comes first

[`SPEC.md`](SPEC.md) is normative. A change to the format's behaviour changes
the specification in the same pull request; a behaviour the specification
does not describe is a bug in one of the two. Code comments refer to it by
section (`SPEC.md` 6.1.2).

The guide in [`docs/`](docs/README.md) is written for users, and describes what
the code does today. A change that users can see updates the guide as well.

`crates/crol/tests/guide.rs` builds and runs every Rust block of the guide and the
READMEs, and compares what it prints with the `$ cargo run` block after it. So a block
is a whole program, and the files it reads are in `// path` blocks above it. Write
`rust,no_run` for one that cannot run there, and `rust,ignore` for one that cannot be
built.

## Checks

CI runs these on every pull request; run them before pushing:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

Snapshot tests use [`expect-test`](https://docs.rs/expect-test). After an
intended change, run `UPDATE_EXPECT=1 cargo test` and review the diff before
committing it.

## Guidelines

- `#![forbid(unsafe_code)]` in every crate.
- Keep dependencies small (`SPEC.md` 14.1); new dependencies need a reason and
  must be licensed compatibly with both MIT and Apache-2.0 (`cargo deny check`).
- Every rule in the specification should get at least one passing and one
  error test case.
- The parser and evaluator must never panic or loop, on any input. The fuzz
  targets in [`fuzz/`](fuzz/README.md) look for inputs that make them.

## Releasing

All crates share one version number (`SPEC.md` 14.4).

1. Update `version` in the root `Cargo.toml` and the `crol-*` entries in
   `[workspace.dependencies]`, and move the `Unreleased` section of
   `CHANGELOG.md` under the new version, with the date. The workflow refuses a
   tag without that section, and uses it as the release notes.
2. Update the version in `crol = "0.4"` in the guide and the READMEs if the
   minor version changed.
3. Commit, wait for CI to pass on `main`, then tag `vX.Y.Z` and push the tag. The
   release workflow builds the binaries, publishes to crates.io whatever is not
   there yet, and then makes the GitHub release. It is safe to run again if it
   stops part of the way.

A pull request that changes `release.yml` runs the same builds and publishes
nothing, so a change to the workflow is tried before a tag depends on it.
