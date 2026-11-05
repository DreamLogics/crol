# Optimising evaluation: a brief

This is a brief for an agent working on the evaluator's speed and memory. Read
[`CONTRIBUTING.md`](CONTRIBUTING.md) first, and the parts of
[`SPEC.md`](SPEC.md) you touch: 7.5 (laziness), 7.6 (determinism), 8.1 and 8.3
(contexts).

## Why

CROL is about to load the data of a real-time strategy game, swgb-rs: about 14,000
definitions, 17 MB of text, 3,200 files, and 9 civilisations as a variant group, so
10 contexts. It runs at every game start, and on every save in the game's test mode.
The RON loader it replaces peaks at 205 MB and takes about 1.2 s, including that
game's own checks. CROL should not be worse than that on either.

## How to measure

```sh
cargo bench -p crol-eval --bench game   # phases and peak memory, game-shaped data
cargo bench -p crol-eval --bench big    # the spec's target: under 200 ms (SPEC.md 12.3)
```

`game` writes its own data, so it needs no game files, and it was tuned to behave
like the real data. Use it, not your intuition: an earlier guess here that parsing
dominated was wrong, and only measuring each phase showed it. Peak memory comes from
`/proc/self/status`, so measure on Linux. Run each benchmark at least three times,
and on a quiet machine: differences of 5% between runs are normal.

Compare against a build of the commit you started from, on your machine, in the same
sitting. Absolute numbers from elsewhere do not transfer.

## Where it stands

These are the numbers from before the work below, on the development machine, for
both the real data and `game`:

| Phase | Real data | `game` |
| --- | --- | --- |
| Compile (parse and lower) | 0.5 s, 234 MB | 0.45 s, 246 MB |
| Evaluate, 10 contexts | 0.7 s, peak 592 MB | 0.61 s, peak 593 MB |
| One context later (`evaluate_contexts`) | 96 ms | 91 ms |
| Parse alone | 160 ms, 110 MB | |
| Compile with the base layer from `.crob` | | 92 ms |

After evaluation, 248 MB stays live on the real data: 124 MB of `Compiled` (the IR and
the texts) and the values. `big` is at about 245 ms, over its 200 ms target.

Since then, on a machine about 1.75 times slower, so compare the columns and not with
the table above, three runs each:

| `game` and `big` | Before | After |
| --- | --- | --- |
| Peak of `evaluate` | 594 MB | about 295 MB (281 to 320) |
| Peak of `compile` | 246 MB | 220 MB |
| Compile | 746 ms | 437 ms |
| Evaluate, 10 contexts | 1.08 s | 592 ms |
| One context later | 150 ms | 99 ms |
| Compile with the base layer from `.crob` | 139 ms | 97 ms |
| `big` | 443 ms | 255 ms |

`big` at 255 ms here is about 145 ms on the development machine, by that ratio, though
it has not been measured there. Measured with a counting allocator, 199 MB stays live
after evaluation: 17 MB of sources, 80 MB of `Compiled`, and 102 MB of values. The heap
peaks 36 MB above that.

Parsing on several threads (`parse_all`) is why the process peak is so far above the
heap's. With glibc's allocator, about 50 MB that parse trees took on other threads is
not reused by the main thread, whatever the number of threads, and the peak varies from
run to run. On one thread, the peak is 244 MB, compile 662 ms and evaluate 550 ms. It
was kept for the 190 ms it saves in all at every game start. Another allocator may not
have this cost: measure before relying on that. `big` has too little text to use threads.

Already done, so do not look there first: cells of a finished context are freed
(`Evaluator::forget`), the base is not cloned, `Shape::Keyed` and `ValueKind::Keyed` are
boxed, and a context evaluated later does not evaluate the base. The cells of a root
key that nothing reads are freed once its value is computed (`Evaluator::full_base`),
field names and strings are shared, literals have a cell of their own, and a context
only looks at the keys that can differ.

Freeing a root key's cells is off when any key reads the whole root or another context
(`evict` in `evaluate_data`), so data that does keeps the old peak. Check that first if
the peak of the real data is far from that of `game`.

Tried and dropped:

- A faster hasher for `KeyedShape::slots`: no difference in `game`, and `big` was
  slightly slower.
- Fewer threads for parsing: the cost in memory is the same with 2 threads as with 8,
  and compile is slower (528 ms with 2, 465 ms with 4, 437 ms with 8).

## Goals, in order

1. Peak memory of `evaluate` on `game` well below 593 MB. 300 MB would be very good.
2. `big` back under 200 ms, without giving up the memory.
3. Time of `evaluate` and `compile`.
4. Live memory after evaluation.

## Where the memory is

The cells of one root key at a time now, so what they cost matters much less than it
did, unless freeing them is off (see above). What stays is the live memory, measured
with a counting `#[global_allocator]` in a throwaway program outside the workspace, as
`forbid(unsafe_code)` applies to everything in it:

- The values, 102 MB. `Keyed` has a fixed cost for each block, and a map entry takes
  96 bytes, as `Key` and `Value` are 16-aligned by their `i128`.
- `Compiled`, 80 MB: expressions are 48 bytes each, `Entry` is 96, and the text of each
  string literal.
- `Cell` holds `location: Option<Location>` for messages, one `Rc<PathLink>` each.
  That no longer matters for memory, but it is the largest allocation left for a literal.

Ideas, roughly by what they may give:

- Make `Location` lazy or smaller: derive it for messages rather than store it per cell.
- Keep `i128` out of the common case, so that `Value` and `Key` lose their 16-byte
  alignment. `Expr` does this with `Wide`; for `Value` it means changing a public type,
  for perhaps 15% of the values, by estimate.
- Lower each document as it is parsed, which would take the peak of `compile` well
  below that of `evaluate`. Templates are collected from every document first, so
  this needs two passes.
- `plan.varying` decides what a context re-evaluates. See if it is wider than it must
  be: `reads.whole` makes every key vary.

## Rules

- **The fingerprint must not change.** `cargo test --workspace --all-features
  --no-fail-fast` checks every conformance case's `expected.fingerprint`, and `game`
  prints one. A different fingerprint means different data, so it is a bug, however fast.
- Behaviour changes need `SPEC.md` first. Pure optimisations do not change it.
- `#![forbid(unsafe_code)]` stays. New dependencies need a reason (`SPEC.md` 14.1).
- The parser and evaluator never panic or loop, on any input. Run the fuzz targets in
  [`fuzz/`](fuzz/README.md) after changing `eval.rs`; they have not been run lately.
- `CrolData` stays `Send + Sync`: a test checks that.
- `cargo fmt --all`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  and the tests must pass. Note them with the results.
- Keep changes small: one idea per commit, with the numbers before and after in the
  commit message. Drop an idea that gives little, rather than keeping the complexity.

## Working beside others

`.crob`, a binary form of layers, reads and writes every type in `ir.rs`, in
`crates/crol-eval/src/binary.rs`. If you change those types, change it too, raise
`binary::FORMAT`, and keep `compiled_layers_evaluate_the_same` passing. `game` checks
that a compiled layer gives the same fingerprint. Work on a branch of your own,
`perf/<what>`.

## What to hand back

A short report: for each change kept, what it did, and the numbers before and after
from the same machine (`game` and `big`, three runs each); what you tried and dropped,
and why; and where the memory is now, measured.
