#!/usr/bin/env bash
# Run the Hawk workspace visibility audit with the pinned version.
# Ported from fallow. fallow excludes its facade and contract library crates
# with `--exclude-crate`. The library crate of this package exports only
# `run`, and the binary calls it, so the list is empty.
set -euo pipefail

readonly EXPECTED_HAWK_VERSION="cargo hawk 0.1.17"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
readonly REPO_ROOT

if [[ "$(cargo hawk --version)" != "${EXPECTED_HAWK_VERSION}" ]]; then
  printf 'error: expected %s\n' "${EXPECTED_HAWK_VERSION}" >&2
  exit 2
fi

hawk_args=(
  check
  --manifest-path "${REPO_ROOT}/Cargo.toml"
  --config "${REPO_ROOT}/hawk.toml"
  --color never
)

if [[ "${1:-dead-public}" == "dead-public" ]]; then
  hawk_args+=(
    -A hawk::unnecessary_public
    -A hawk::unnecessary_restricted_visibility
  )
elif [[ "${1:-}" != "all" ]]; then
  printf 'usage: %s [dead-public|all]\n' "$0" >&2
  exit 2
fi

if [[ -n "${FALLOW_HAWK_TARGET_DIR:-}" ]]; then
  hawk_args+=(--target-dir "${FALLOW_HAWK_TARGET_DIR}")
fi

if [[ -n "${FALLOW_HAWK_GRAPH_DIR:-}" ]]; then
  hawk_args+=(--graph-dir "${FALLOW_HAWK_GRAPH_DIR}")
fi

exec cargo hawk "${hawk_args[@]}"
