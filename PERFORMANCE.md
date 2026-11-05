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

Tools that worked:

- **Exact heap.** A counting `#[global_allocator]`. `forbid(unsafe_code)` applies to the
  whole workspace, so put it in a small crate outside it, with a path dependency on
  `crol-eval` and `debug = 1`. Copy the data generator out of `benches/game.rs`.
- **Profiles.** `samply record --save-only --unstable-presymbolicate -r 3000 -o out.json.gz
  -- <binary>`. The bench binaries have no symbols, so profile that outside crate too.
  In glibc, `malloc` and `free` show up as unnamed `fun_` symbols.
- **No threads, without changing code.** `taskset -c 0 <binary>` makes
  `available_parallelism` return 1.
- **Page faults and system time.** `minflt`, `utime` and `stime` from `/proc/self/stat`,
  read before and after a phase.
- **Whether an arena is the cause.** `GLIBC_TUNABLES=glibc.malloc.arena_max=1`.
- **Worktrees.** Under `.claude/worktrees/`, set `CARGO_WORKSPACE_DIR` to the worktree when
  running tests, or `expect-test` reads and writes the main checkout's snapshots.

## Where it stands

### Before this work, on the development machine

For both the real data and `game`, at `c6d8861`:

| Phase | Real data | `game` |
| --- | --- | --- |
| Compile (parse and lower) | 0.5 s, 234 MB | 0.45 s, 246 MB |
| Evaluate, 10 contexts | 0.7 s, peak 592 MB | 0.61 s, peak 593 MB |
| One context later (`evaluate_contexts`) | 96 ms | 91 ms |
| Parse alone | 160 ms, 110 MB | |
| Compile with the base layer from `.crob` | | 92 ms |

After evaluation, 248 MB stayed live on the real data: 124 MB of `Compiled` (the IR and
the texts) and the values. `big` was at about 245 ms.

### Now, on a slower machine

| | |
| --- | --- |
| CPU | AMD Ryzen 9 3900X, 12 cores, 24 threads, 64 MiB L3 |
| Memory | 62 GB |
| System | Arch Linux, kernel 7.2.11, glibc 2.44, `schedutil` governor |
| Compiler | rustc 1.99.0, `cargo bench` profile (release, thin LTO) |
| Allocator | glibc's |
| Load | not idle: a load average of about 2.5 from other work |

`big` took 1.8 times as long here as on the development machine at the same commit, so
compare within these tables, and not with the one above. Everything below is from one
sitting, three runs of each, on `game` unless it says `big`. The real data has not been
measured since `c6d8861`.

| | `c6d8861` | Text, no threads | Text, threads (default) | `.crob` base, 3 threads |
| --- | --- | --- | --- | --- |
| Compile or load | 765 ms | 650 ms | 438 ms | 102 ms |
| Evaluate, 10 contexts | 1.09 s | 559 ms | 419 ms | 360 ms |
| **Together** | **1.86 s** | **1.21 s** | **857 ms** | **462 ms** |
| **Peak of the process** | **594 MB** | **244 MB** | **335 MB** (331 to 341) | **320 MB** |
| Peak after compile or load | 246 MB | 220 MB | 220 MB | 164 MB |
| One context later | 149 ms | | 90 ms | |
| `big`, 16 contexts | 445 ms | 248 ms | 192 ms | |

The last column is a fresh process that reads the file, compiles the mod layer over it,
drops what it read, and evaluates. `game` itself does not time this path.

By a counting allocator, 199 MB stays live after evaluation: 17 MB of sources, 80 MB of
`Compiled`, and 102 MB of values. With the default threads the heap peaks at 278 MB, so
about 55 MB of the process's peak is what the allocator holds on to.

Against the goals: 1 is met, unless freeing cells is off (below); 2 is met, even on this
machine. The RON loader's 1.2 s is beaten. Its 205 MB is not: 244 MB is the lowest peak
so far, and that is without threads.

### Threads

Two kinds, with different costs. Parsing takes up to 8 threads, one for each MB of text
in a layer (`parse_all`). Contexts take up to 3, from at least 20 000 expressions
(`DataOptions::threads`, `CrolConfig::threads`; `Some(1)` uses none).

| Threads for contexts | Evaluate, from text | Peak, from text | Load and evaluate, `.crob` | Peak, `.crob` |
| --- | --- | --- | --- | --- |
| 1 | 622 ms | 324 MB | 641 ms | 270 MB |
| 2 | 506 ms | 329 MB | 537 ms | 293 MB |
| **3** | **425 ms** | **350 MB** | **462 ms** | **320 MB** |
| 4 | 422 ms | 374 MB | 459 ms | 348 MB |
| 6 | 387 ms | 413 MB | | |
| 8 | 388 ms | 450 MB | | |

This table has its own sitting and program, so its peaks run a little above the table
before. 3 is where the time stops paying for the memory, partly because 9 contexts divide
evenly over 3 threads: 4 threads take as many rounds. Data with another number of
contexts may want another number. Beyond 6 there is nothing to gain.

| Threads for parsing | Compile | Peak of `evaluate`, no context threads |
| --- | --- | --- |
| 1 | 663 ms | 244 MB |
| 2 | 529 ms | 293 MB |
| 3 | 485 ms | 308 MB |
| 4 | 467 ms | 295 to 319 MB |
| 6 | 444 ms | 310 MB |
| 8 | 435 ms | 281 to 320 MB, usually 295 |

Memory that parse trees took on another thread stays in that thread's glibc arena, and
the main thread cannot reuse it. That costs about 50 MB however many threads there are,
so fewer threads save nothing, and the peak varies from run to run. It also costs
evaluation about 45 ms: 22 000 more page faults and 40 ms more system time, as the main
thread gets about 88 MB of new pages rather than reusing what was freed.
`arena_max=1` gives the memory back, but compile takes 1.24 s, on the lock, and evaluate
stays about 35 ms slower, for reasons not measured. Another allocator may not have this
cost; none has been tried.

### What to do next

1. **Load the base layer from `.crob`, in the game's loader.** It is the largest gain
   left: 857 ms to 462 ms, and a lower peak. It needs a loader that writes the file when
   the sources change. Drop the file's bytes and `read_layer`'s result after
   `compile_binary`: holding them through evaluation costs 36 MB.
2. **The base is now the critical path**, 245 ms of the 360 ms, on one thread, as the
   evaluator's `Rc`s tie it to one. The ideas below are for that.
3. **A context from a known base has a fixed cost.** Of 107 ms for one context, about a
   third is evaluating it. `DataPlan::new` was 22%, and is now made once for all threads
   but still once for each `evaluate_contexts`. `context_state` is about 13%, applying
   every key's chain in the base. Much of the rest, by the profile, is allocating and
   freeing.
4. **The `.crob` link step**, about 78 ms, has not been profiled.

### Already done

So do not look there first:

- Cells of a finished context are freed (`Evaluator::forget`), the base is not cloned,
  `Shape::Keyed` and `ValueKind::Keyed` are boxed, and a context evaluated later does not
  evaluate the base.
- The cells of a root key that nothing reads are freed once its value is computed
  (`Evaluator::full_base`). A read that the static analysis misses computes the key again
  (`Job::Key`), so that this only ever costs time.
- Field names and strings are shared (`Arc<str>`), interned by the lowerer.
- Literals have a cell and recipe of their own, without an environment.
- A context only looks at the keys that can differ.
- `Expr` is 48 bytes, which a `const` assertion keeps, and the IR's spare capacity is
  given back.

Freeing a root key's cells is off when any key reads the whole root or another context
(`evict` in `evaluate_raw`), so data that does keeps a far higher peak. Check that first
if the peak of the real data is far from that of `game`.

Contexts are evaluated in order, without threads, if any key reads another context, as
contexts then depend on each other, or if the base failed, as each context then reports
its own errors. So is one context on its own.

### Tried and dropped

- A faster hasher for `KeyedShape::slots`: no difference in `game`, and `big` was
  slightly slower. Hashing looked like 8% of evaluation in the profile, and was not.
- `arena_max=1` to keep parsing's memory: see above.
- A plan for each thread: with 8 threads, evaluate took 448 ms, against 388 ms with one
  plan for all of them.

### A bug found on the way, not fixed

It gives wrong data silently. `evaluate_every_key` gives the right answer, so it is in
deciding what can differ, and `leaving_out_keys_that_cannot_differ_changes_nothing`
compares the two, on data without such spreads. `game` does not have it: no spread moves
a reference. **Fixing it changes results, so that is not an optimisation: it needs its
own commit and a test.**

- **A reference that a spread moves.** With
  `y: 1, y<v>: 2, a: { inner: { x: &parent.y }, y: 5 }, b: { ..&root.a.inner }`, `b.x` is
  `1` in `{v}`, and should be `2`. `collect_reads` sees `&parent.y` where it is written, in
  `a`, not at `b`, where it ends up reading the root. This one may need a `SPEC.md` decision.

## Goals, in order

1. Peak memory of `evaluate` on `game` well below 593 MB. 300 MB would be very good.
2. `big` back under 200 ms, without giving up the memory.
3. Time of `evaluate` and `compile`.
4. Live memory after evaluation.

## Where the memory is

The cells of one root key at a time now, so what they cost matters much less than it
did, unless freeing them is off (see above). What stays:

- The values, 102 MB. `Keyed` has a fixed cost for each block, and a map entry takes
  96 bytes, as `Key` and `Value` are 16-aligned by their `i128`.
- `Compiled`, 80 MB: expressions are 48 bytes each, `Entry` is 96, and the text of each
  string literal.
- What the allocator holds, about 55 MB with the default threads.
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
- `plan.varying` decides what a context re-evaluates. A key with an operation that holds
  in the base varies in every context, which is wider than it must be for `x<!b>` in a
  context without `b`. `reads.whole` makes only the key that reads the whole root vary,
  which may be too narrow: see the bug above before relying on it for anything new.

## Rules

- **The fingerprint must not change.** `cargo test --workspace --all-features
  --no-fail-fast` checks every conformance case's `expected.fingerprint`, and `game`
  prints one. A different fingerprint means different data, so it is a bug, however fast.
- Behaviour changes need `SPEC.md` first. Pure optimisations do not change it.
- `#![forbid(unsafe_code)]` stays. New dependencies need a reason (`SPEC.md` 14.1).
- The parser and evaluator never panic or loop, on any input. Run the fuzz targets in
  [`fuzz/`](fuzz/README.md) after changing `eval.rs`. Their inputs are too small to use
  threads, so what guards those are `threads_change_neither_values_nor_messages` and the
  conformance test, which evaluates every case on threads as well.
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
