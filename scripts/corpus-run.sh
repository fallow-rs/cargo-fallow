#!/usr/bin/env bash
set -euo pipefail

# Clone each corpus workspace of the verdict file at its pinned commit, and
# write the output of `cargo fallow check --format json` to
# <folder>/results/<workspace>.json.
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
done < <(jq -r '.workspaces | to_entries[] | "\(.key) \(.value.repository) \(.value.commit)"' "$VERDICTS")
