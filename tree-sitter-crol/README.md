# tree-sitter-crol

A [tree-sitter](https://tree-sitter.github.io) grammar for
[CROL](https://github.com/DreamLogics/crol), the Composable Rust Object
Language (`SPEC.md` 12.6), with queries for highlights, brackets, indents,
the outline, folds and text objects in `queries/`. Helix has its own, in
`editors/helix/queries/crol/`, as it names captures differently.

It accepts what the Rust parser (`crates/crol-syntax`) accepts: CI runs
`check-corpus.sh`, which parses the shared syntax cases in `tests/syntax/`
and the conformance suite with both.

```sh
tree-sitter generate      # after changing grammar.js
tree-sitter test          # the cases in test/corpus/
TREE_SITTER=tree-sitter ./check-corpus.sh
./check-pins.sh           # the editors pin this grammar
```

`src/` holds the generated parser, committed so that editors can build the
grammar without generating it; `src/scanner.c` is hand-written. It lexes raw
strings and nested block comments, and decides where a line break ends an
entry (`SPEC.md` 5.3).

Licensed under either of Apache License, Version 2.0 or MIT licence at your
option.
