#!/usr/bin/env bash
# Checks that the grammar agrees with the Rust parser (SPEC.md 12.6): it must
# accept every file in tests/syntax/valid and every conformance case, and
# reject every file in tests/syntax/invalid. The Rust side is
# crates/crol-syntax/tests/syntax_corpus.rs.
set -euo pipefail
cd "$(dirname "$0")"
TREE_SITTER="${TREE_SITTER:-tree-sitter}"
status=0
while IFS= read -r file; do
  if ! "$TREE_SITTER" parse --quiet "$file" >/dev/null 2>&1; then
    echo "the grammar rejects a valid file: $file"
    status=1
  fi
done < <(find ../tests/syntax/valid ../tests/cases \( -name '*.crod' -o -name '*.crov' \) | sort)
for file in ../tests/syntax/invalid/*.cro[dv]; do
  if "$TREE_SITTER" parse --quiet "$file" >/dev/null 2>&1; then
    echo "the grammar accepts an invalid file: $file"
    status=1
  fi
done
exit "$status"
