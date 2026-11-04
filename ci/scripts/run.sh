#!/usr/bin/env bash
set -euo pipefail

# Run one cargo-fallow command in a GitLab CI job.
#
# The script writes the code quality report for the merge request widget, the
# JSON report, and with CARGO_FALLOW_COMMENT a markdown report that
# comment.sh posts as one merge request note. The JSON run gives the verdict:
# the job exits with its exit code.
#
# Optional env: CARGO_FALLOW_COMMAND (default: check), CARGO_FALLOW_ARGS,
#   CARGO_FALLOW_FAIL_ON_ISSUES (default: true), CARGO_FALLOW_CHANGED_SINCE,
#   CARGO_FALLOW_AUTO_CHANGED_SINCE (default: true), CARGO_FALLOW_COMMENT
#   (default: false), CARGO_FALLOW_COMMENT_KEY (default: the command),
#   CARGO_FALLOW_CODE_QUALITY_FILE, CARGO_FALLOW_RESULTS_FILE,
#   CARGO_FALLOW_REPORT_FILE, CI_MERGE_REQUEST_IID,
#   CI_MERGE_REQUEST_DIFF_BASE_SHA, and the env of comment.sh

: "${CARGO_FALLOW_COMMAND:=check}" "${CARGO_FALLOW_ARGS:=}"
: "${CARGO_FALLOW_FAIL_ON_ISSUES:=true}" "${CARGO_FALLOW_CHANGED_SINCE:=}"
: "${CARGO_FALLOW_AUTO_CHANGED_SINCE:=true}" "${CARGO_FALLOW_COMMENT:=false}"
: "${CARGO_FALLOW_COMMENT_KEY:=$CARGO_FALLOW_COMMAND}"
: "${CARGO_FALLOW_CODE_QUALITY_FILE:=gl-code-quality-report.json}"
: "${CARGO_FALLOW_RESULTS_FILE:=cargo-fallow-results.json}"
: "${CARGO_FALLOW_REPORT_FILE:=cargo-fallow-report.md}"
: "${CI_MERGE_REQUEST_IID:=}" "${CI_MERGE_REQUEST_DIFF_BASE_SHA:=}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

fail() {
  echo "ERROR: $1"
  exit 1
}

check_boolean() {
  case "$2" in
    true | false) ;;
    *) fail "Invalid $1: $2. Use true or false." ;;
  esac
}

# The static commands. `mutants` builds and runs the tests of each mutant, so
# a code quality job does not run it.
case "$CARGO_FALLOW_COMMAND" in
  check | deps | files | visibility | boundaries | size | complexity | dupes) ;;
  *) fail "Invalid CARGO_FALLOW_COMMAND: ${CARGO_FALLOW_COMMAND}. Use check, deps, files, visibility, boundaries, size, complexity or dupes." ;;
esac
check_boolean CARGO_FALLOW_FAIL_ON_ISSUES "$CARGO_FALLOW_FAIL_ON_ISSUES"
check_boolean CARGO_FALLOW_AUTO_CHANGED_SINCE "$CARGO_FALLOW_AUTO_CHANGED_SINCE"
check_boolean CARGO_FALLOW_COMMENT "$CARGO_FALLOW_COMMENT"

# The extra arguments split on white space, also on line breaks. Quotes have
# no meaning.
EXTRA=()
read -r -a EXTRA <<< "${CARGO_FALLOW_ARGS//$'\n'/ }" || true

SCOPE=()
if [ -n "$CARGO_FALLOW_CHANGED_SINCE" ]; then
  SCOPE=(--changed-since "$CARGO_FALLOW_CHANGED_SINCE")
elif [ "$CARGO_FALLOW_AUTO_CHANGED_SINCE" = "true" ] && [ -n "$CI_MERGE_REQUEST_DIFF_BASE_SHA" ]; then
  # The merge request base: report only the findings in the changed files.
  SCOPE=(--changed-since "$CI_MERGE_REQUEST_DIFF_BASE_SHA")
fi

GATE=()
[ "$CARGO_FALLOW_FAIL_ON_ISSUES" = "true" ] && GATE=(--fail-on-issues)

# Runs the command in one format, with the changed-file scope. A finding exits
# 1 only with the gate flag, and exit 2 is an execution error.
render() {
  local format=$1
  shift
  cargo-fallow "$CARGO_FALLOW_COMMAND" --format "$format" --quiet "$@" \
    ${SCOPE[@]+"${SCOPE[@]}"} ${EXTRA[@]+"${EXTRA[@]}"}
}

# The code quality report has no changed-file scope. GitLab compares it with
# the report of the target branch, so a scoped report would show each finding
# outside the changed files as fixed. A failed run leaves an empty list.
if ! cargo-fallow "$CARGO_FALLOW_COMMAND" --format codeclimate --quiet \
  ${EXTRA[@]+"${EXTRA[@]}"} > "$CARGO_FALLOW_CODE_QUALITY_FILE"; then
  if ! jq -e 'type == "array"' "$CARGO_FALLOW_CODE_QUALITY_FILE" > /dev/null 2>&1; then
    printf '[]\n' > "$CARGO_FALLOW_CODE_QUALITY_FILE"
  fi
fi

status=0
render json ${GATE[@]+"${GATE[@]}"} > "$CARGO_FALLOW_RESULTS_FILE" || status=$?
# Without the gate flag a finding does not fail the job.
[ "$CARGO_FALLOW_FAIL_ON_ISSUES" = "false" ] && [ "$status" = "1" ] && status=0

if [ "$CARGO_FALLOW_COMMENT" = "true" ]; then
  if [ -z "$CI_MERGE_REQUEST_IID" ]; then
    echo "NOTE: CARGO_FALLOW_COMMENT applies to merge request pipelines. This job has no merge request, so it posts no note."
  else
    markdown_status=0
    render markdown > "$CARGO_FALLOW_REPORT_FILE" || markdown_status=$?
    if [ "$markdown_status" -gt 1 ] || [ ! -s "$CARGO_FALLOW_REPORT_FILE" ]; then
      # The note keeps the report of the last good run.
      echo "WARNING: The markdown run failed with exit code ${markdown_status}, so the job does not change the merge request note."
    else
      REPORT_FILE="$CARGO_FALLOW_REPORT_FILE" COMMENT_KEY="$CARGO_FALLOW_COMMENT_KEY" \
        bash "$SCRIPT_DIR/comment.sh" || echo "WARNING: The merge request note failed."
    fi
  fi
fi

case "$status" in
  0) echo "cargo fallow ${CARGO_FALLOW_COMMAND}: no enforced gate failed." ;;
  1) echo "cargo fallow ${CARGO_FALLOW_COMMAND}: an enforced gate failed. See ${CARGO_FALLOW_RESULTS_FILE}." ;;
  *) echo "cargo fallow ${CARGO_FALLOW_COMMAND}: the run failed with exit code ${status}. See ${CARGO_FALLOW_RESULTS_FILE}." ;;
esac
exit "$status"
