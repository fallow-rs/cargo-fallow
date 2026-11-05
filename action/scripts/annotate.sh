#!/usr/bin/env bash
set -euo pipefail

# Write each SARIF result as a workflow command, so GitHub shows it as an
# annotation on the line.
#
# Required env: ACTION_JQ_DIR, MAX_ANNOTATIONS
# Optional env: SARIF_FILE (empty: no SARIF log, so no annotations),
#   PATH_PREFIX (paths in SARIF_FILE already carry it, see analyze.sh)

: "${SARIF_FILE:=}" "${PATH_PREFIX:=}"

if ! [[ "$MAX_ANNOTATIONS" =~ ^[0-9]+$ ]]; then
  echo "::error::Invalid 'max-annotations' input: ${MAX_ANNOTATIONS}. Use a whole number."
  exit 1
fi
if [ -z "$SARIF_FILE" ] || [ ! -f "$SARIF_FILE" ]; then
  echo "No SARIF log, so no annotations."
  exit 0
fi

# jq on Windows ends each line with CRLF, so `tr` removes the CR.
total="$(jq '[.runs[]?.results[]?] | length' "$SARIF_FILE" | tr -d '\r')"
# analyze.sh wrote the prefix into the SARIF paths, so this step adds none.
jq -r --arg max "$MAX_ANNOTATIONS" --arg prefix "" -f "${ACTION_JQ_DIR}/annotations.jq" "$SARIF_FILE" \
  | tr -d '\r'
if [ "$total" -gt "$MAX_ANNOTATIONS" ]; then
  echo "::notice::cargo-fallow wrote ${MAX_ANNOTATIONS} of ${total} findings as annotations, errors first. The job summary and the report files hold all of them."
fi
