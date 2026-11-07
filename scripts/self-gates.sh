#!/bin/sh
# Run the gates of the Action self-test (.github/workflows/github-action.yml)
# on this repository, with the same flags as action/scripts/analyze.sh.
# A plain `cargo fallow check` exits 0 on a gate failure, so it does not show
# a function over the complexity limits. This script does.
#
# Usage: sh scripts/self-gates.sh [base]
# The size gate compares with `base`. The default is `origin/main`.
set -eu

base="${1:-origin/main}"

cargo build --quiet --locked
bin="$(cargo metadata --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')/debug/cargo-fallow"

status=0
gate() {
  name="$1"
  shift
  if ! "$bin" fallow "$name" --quiet --fail-on-issues "$@" >/dev/null; then
    echo "self-gates: the ${name} gate failed. Run: cargo run -- fallow ${name} $*" >&2
    status=1
  fi
}

gate deps
gate boundaries --fail-on-violations
gate size --fail-on-growth --base "$base"
gate complexity --fail-on-complexity
gate files
exit "$status"
