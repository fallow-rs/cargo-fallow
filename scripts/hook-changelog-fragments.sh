#!/bin/sh
# Check that the branch does not edit the [Unreleased] section of CHANGELOG.md
# and that each changelog.d/ fragment is valid. hk.pkl calls this script. The
# base is origin/main, or upstream/main in a fork clone. A clone without
# either ref skips the check, because CI still runs it.
set -e

for base in origin/main upstream/main; do
  if git rev-parse --verify --quiet "${base}^{commit}" >/dev/null 2>&1; then
    exec node scripts/changelog-fragments.mjs check --base "$base"
  fi
done

echo "pre-commit: no origin/main or upstream/main ref, so the changelog fragment check is skipped (CI still runs it)." >&2
exit 0
