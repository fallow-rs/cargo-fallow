#!/usr/bin/env bash
set -euo pipefail

# Clone each corpus workspace of the verdict file at its pinned commit, and
# write the output of `cargo fallow check --format json` to
# <folder>/results/<workspace>.json. A second run with `public-crates = []`
# makes each library internal and writes <workspace>.internal.json. In the
# first run, the API of a library that a registry can publish is an entry
# point, so the second run is the one that checks the visibility rules there.
#
# Usage: bash scripts/corpus-run.sh <verdicts.json> <folder> <cargo-fallow binary>
# A workspace folder that exists already is used as it is, so a local run can
# reuse its clones.

VERDICTS=$1
FOLDER=$2
BINARY=$3
mkdir -p "$FOLDER/results"

# The child commands read no stdin, so they cannot take the list of the loop.
while read -r name repository commit; do
  if ! [[ "$name" =~ ^[A-Za-z0-9._-]+$ ]] || [[ "$repository" != https://* ]] || ! [[ "$commit" =~ ^[0-9a-f]{40}$ ]]; then
    echo "::error::The verdict file has an invalid workspace entry: ${name}"
    exit 1
  fi
  workspace="$FOLDER/$name"
  if [ ! -d "$workspace" ]; then
    git clone --quiet --filter=blob:none --no-checkout "$repository" "$workspace" < /dev/null
  fi
  if ! git -C "$workspace" cat-file -e "${commit}^{commit}" 2> /dev/null; then
    git -C "$workspace" fetch --quiet origin < /dev/null
  fi
  git -C "$workspace" checkout --quiet --force --detach "$commit" < /dev/null
  echo "Checking ${name} at ${commit}"
  status=0
  # `--locked` keeps the resolve of the pinned Cargo.lock.
  (cd "$workspace" && "$BINARY" check --format json --quiet --locked < /dev/null) \
    > "$FOLDER/results/$name.json" || status=$?
  # Exit 1 only follows an enforced gate. The run has no gate flag, so only
  # exit 2, an execution error, stops the job.
  if [ "$status" -gt 1 ]; then
    echo "::error::cargo fallow check failed on ${name} with exit code ${status}"
    cat "$FOLDER/results/$name.json"
    exit 1
  fi
  # The key goes in a new table at the end of the root manifest. A root with
  # no `[workspace]` table, or with its own fallow configuration, would get a
  # different meaning, so the run stops for it.
  manifest="$workspace/Cargo.toml"
  if ! grep -q '^\[workspace\]' "$manifest" || grep -qE '^\s*\[workspace\.metadata\.fallow' "$manifest"; then
    echo "::error::${name}: the root manifest needs a [workspace] table and no [workspace.metadata.fallow] table"
    exit 1
  fi
  printf '\n[workspace.metadata.fallow.visibility]\npublic-crates = []\n' >> "$manifest"
  status=0
  (cd "$workspace" && "$BINARY" check --format json --quiet --locked < /dev/null) \
    > "$FOLDER/results/$name.internal.json" || status=$?
  git -C "$workspace" checkout --quiet -- Cargo.toml < /dev/null
  if [ "$status" -gt 1 ]; then
    echo "::error::cargo fallow check with each library internal failed on ${name} with exit code ${status}"
    cat "$FOLDER/results/$name.internal.json"
    exit 1
  fi
done < <(jq -r '.workspaces | to_entries[] | "\(.key) \(.value.repository) \(.value.commit)"' "$VERDICTS")
