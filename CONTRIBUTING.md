# Contributing

## The specification comes first

[`SPEC.md`](SPEC.md) is normative. A change to the format's behaviour changes
the specification in the same pull request; a behaviour the specification
does not describe is a bug in one of the two. Code comments refer to it by
section (`SPEC.md` 6.1.2).

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
   `CHANGELOG.md` under the new version.
2. Commit, then tag `vX.Y.Z` and push the tag. The release workflow publishes
   the crates to crates.io and makes a GitHub release with the `crol` binaries.

The workflow publishes with crates.io trusted publishing. A crate's first
version must be published by hand (`cargo publish -p <crate>`), after which
trusted publishing is configured for `DreamLogics/crol` and the `release.yml`
workflow in the crate's settings on crates.io.
