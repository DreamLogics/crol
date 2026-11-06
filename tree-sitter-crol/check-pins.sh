#!/usr/bin/env bash
# Checks that the editors build the grammar in this repository: the commit each
# one pins must be in the history, and hold the same parser. It may be older than
# the last commit to the grammar, as a commit cannot name itself, so a change to
# the grammar needs a commit after it that updates the pins.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
status=0
for config in editors/zed/extension.toml editors/helix/languages.toml; do
  rev="$(grep -oE 'rev = "[0-9a-f]{40}"' "$config" | grep -oE '[0-9a-f]{40}' || true)"
  if [ -z "$rev" ]; then
    echo "$config: no pinned commit found"
    status=1
  elif ! git merge-base --is-ancestor "$rev" HEAD 2>/dev/null; then
    echo "$config: $rev is not in the history"
    status=1
  elif ! git diff --quiet "$rev" HEAD -- tree-sitter-crol/src; then
    echo "$config: $rev does not have the current grammar; pin a commit that does"
    status=1
  fi
done
exit "$status"
