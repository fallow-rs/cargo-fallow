#!/usr/bin/env bash
set -euo pipefail

# Apply the exit code of the JSON run, after the report steps ran.
#
# Required env: EXIT_CODE, FALLOW_COMMAND
# Optional env: GATES_FAILED, ERROR_CODE

: "${GATES_FAILED:=}" "${ERROR_CODE:=}"

if ! [[ "$EXIT_CODE" =~ ^[0-9]+$ ]]; then
  echo "::error::cargo fallow ${FALLOW_COMMAND} gave no exit code."
  exit 2
fi

case "$EXIT_CODE" in
  0) echo "cargo fallow ${FALLOW_COMMAND}: no enforced gate failed." ;;
  1)
    echo "::error::cargo fallow ${FALLOW_COMMAND}: an enforced gate failed (${GATES_FAILED:-see the report})."
    exit 1
    ;;
  *)
    echo "::error::cargo fallow ${FALLOW_COMMAND} could not finish (exit code ${EXIT_CODE}${ERROR_CODE:+, ${ERROR_CODE}})."
    exit "$EXIT_CODE"
    ;;
esac
