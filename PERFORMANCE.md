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

On the development machine, for both the real data and `game`:

| Phase | Real data | `game` |
| --- | --- | --- |
| Compile (parse and lower) | 0.5 s, 234 MB | 0.45 s, 246 MB |
| Evaluate, 10 contexts | 0.7 s, peak 592 MB | 0.61 s, peak 593 MB |
| One context later (`evaluate_contexts`) | 96 ms | 91 ms |
| Parse alone | 160 ms, 110 MB | |
| Compile with the base layer from `.crob` | | 92 ms |

After evaluation, 248 MB stays live on the real data: 124 MB of `Compiled` (the IR and
the texts) and the values. `big` is at about 245 ms, over its 200 ms target.

Already done, so do not look there first: cells of a finished context are freed
(`Evaluator::forget`), the base is not cloned, `Shape::Keyed` and `ValueKind::Keyed` are
boxed, and a context evaluated later does not evaluate the base.

## Goals, in order

1. Peak memory of `evaluate` on `game` well below 593 MB. 300 MB would be very good.
2. `big` back under 200 ms, without giving up the memory.
3. Time of `evaluate` and `compile`.
4. Live memory after evaluation.

## Where the memory is

Roughly 500 bytes for each field while the base is evaluated. These are estimates
from reading the code, not measurements, so check them first; a counting
`#[global_allocator]` in a throwaway program gives exact live and peak heap.

- `Cell`: `RefCell<State>`, `expr`, and `location: Option<Location>`, whose
  `Location` holds a `Weak`, an anchor, a name and a path, about 48 bytes for what is
  mostly used for messages (`eval.rs`).
- `PathLink`: one `Rc` for each location, holding a `Step` with an owned `String` key.
- `KeyedShape::slots` and `Keyed::entries`: an `IndexMap` with an owned `Key::Str(String)`
  for every field of every block, cloned per block and per location. `Key` holds an
  `i128`, which makes it 16-aligned, and `Value` too.
- `EntryRecipe`: one `Rc` for each entry, kept so that a spread can re-evaluate it.
- `Evaluator::cells` keeps every cell of the base alive until the end, to break cycles.

Ideas, roughly by what they may give:

- Intern keys, in the IR and in shapes, instead of owned `String`s. `Key` is public, and
  breaking it is allowed before 1.0, if the guide and changelog say so.
- Make `Location` lazy or smaller: derive it for messages rather than store it per cell.
- Free base cells once nothing can read them. Scalars need no `Cell` at all when they
  are literals: a slot could hold the value directly.
- Keep `i128` out of the common case, so that `Value` and `Key` lose their 16-byte
  alignment.
- Evaluate and convert a root key at a time, rather than holding every cell and every
  `Value` at once. Late references make this subtle: read `SPEC.md` 7.5.
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
