#!/usr/bin/env bash
set -euo pipefail

# Write the job summary of one run in Markdown.
#
# Required env: ACTION_JQ_DIR, RESULTS_FILE, FALLOW_COMMAND, EXIT_CODE,
#   MAX_FINDINGS
# Optional env: SARIF_FILE, GITHUB_STEP_SUMMARY (default: stdout)

: "${SARIF_FILE:=}" "${GITHUB_STEP_SUMMARY:=/dev/stdout}"

if ! [[ "$MAX_FINDINGS" =~ ^[0-9]+$ ]]; then
  echo "::error::Invalid 'max-annotations' input: ${MAX_FINDINGS}. Use a whole number."
  exit 1
fi
if [ ! -f "$RESULTS_FILE" ]; then
  echo "No JSON report, so no job summary."
  exit 0
fi

sarif_args=(--argjson sarif '[]')
if [ -n "$SARIF_FILE" ] && [ -f "$SARIF_FILE" ]; then
  sarif_args=(--slurpfile sarif "$SARIF_FILE")
fi
jq -r \
  --arg command "$FALLOW_COMMAND" \
  --arg exit_code "$EXIT_CODE" \
  --arg max "$MAX_FINDINGS" \
  --arg prefix "" \
  "${sarif_args[@]}" \
  -f "${ACTION_JQ_DIR}/summary.jq" "$RESULTS_FILE" | tr -d '\r' >> "$GITHUB_STEP_SUMMARY"
