#!/usr/bin/env bash
set -euo pipefail

# Write `available=true|false`: can this repository take a SARIF upload?
#
# A public repository has code scanning without GitHub Advanced Security, and
# its first upload turns code scanning on, so the alerts endpoint can give 404
# before that upload. A private or an internal repository needs GitHub Advanced
# Security, so the step reads the alerts endpoint for it.
#
# A pull request from a fork gets a read-only token, so its upload fails.
#
# Required env: GH_REPO (owner/repo)
# Optional env: GH_TOKEN (gh reads it), IS_FORK (true for a pull request from
#   a fork), GITHUB_OUTPUT (default: stdout)

: "${GITHUB_OUTPUT:=/dev/stdout}" "${IS_FORK:=false}"

if [ "$IS_FORK" = "true" ]; then
  echo "available=false" >> "$GITHUB_OUTPUT"
  echo "::notice::The action did not upload the SARIF log: a pull request from a fork has a read-only token."
  exit 0
fi

visibility="$(gh api "repos/${GH_REPO}" --jq '.visibility' 2> /dev/null || true)"
if [ "$visibility" = "public" ]; then
  echo "available=true" >> "$GITHUB_OUTPUT"
elif gh api "repos/${GH_REPO}/code-scanning/alerts?per_page=1" > /dev/null 2>&1; then
  echo "available=true" >> "$GITHUB_OUTPUT"
else
  echo "available=false" >> "$GITHUB_OUTPUT"
  echo "::warning::The action did not upload the SARIF log: code scanning on a private or internal repository needs GitHub Advanced Security. Set sarif: false to remove this warning."
fi
