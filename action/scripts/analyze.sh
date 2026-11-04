#!/usr/bin/env bash
set -euo pipefail

# Run one cargo-fallow command and write the report files and step outputs.
#
# The JSON run is the main run: its exit code is the verdict of the action.
# The step itself exits 0 after a run, so the summary, annotation and upload
# steps can read the reports. The `gate` step applies the exit code later.
# Only an invalid input stops this step.
#
# Static commands run again for the SARIF log and for the human report.
# `mutants` runs the tests of each mutant, so it runs once: the SARIF log comes
# from action/jq/mutants-sarif.jq.
#
# Required env: INPUT_COMMAND, ACTION_JQ_DIR
# Optional env: INPUT_WORKING_DIRECTORY, INPUT_MANIFEST_PATH, INPUT_FORMAT,
#   INPUT_FAIL_ON_ISSUES, INPUT_ARGS, INPUT_BASE, INPUT_ARTIFACTS_DIR,
#   INPUT_PACKAGE, INPUT_MAX_MUTANTS, INPUT_TIME_BUDGET, INPUT_JOBS,
#   INPUT_TIMEOUT, PR_BASE_SHA, GITHUB_WORKSPACE, GITHUB_OUTPUT, RUNNER_TEMP,
#   CARGO_FALLOW_BIN

: "${INPUT_WORKING_DIRECTORY:=.}" "${INPUT_MANIFEST_PATH:=}" "${INPUT_FORMAT:=human}"
: "${INPUT_FAIL_ON_ISSUES:=true}" "${INPUT_ARGS:=}" "${INPUT_BASE:=}"
: "${INPUT_ARTIFACTS_DIR:=}" "${INPUT_PACKAGE:=}" "${INPUT_MAX_MUTANTS:=}"
: "${INPUT_TIME_BUDGET:=}" "${INPUT_JOBS:=}" "${INPUT_TIMEOUT:=}" "${PR_BASE_SHA:=}"
: "${GITHUB_WORKSPACE:=$PWD}" "${GITHUB_OUTPUT:=/dev/stdout}"
: "${RUNNER_TEMP:=${TMPDIR:-/tmp}}" "${CARGO_FALLOW_BIN:=cargo-fallow}"

fail() {
  echo "::error::$1"
  exit 1
}

# The flags that turn the gate of each command into exit code 1. Each command
# accepts `--fail-on-issues`, and the action gives that flag. The other flag is
# the older name of the same gate.
gate_flags() {
  case "$1" in
    check | deps | files | visibility) echo "--fail-on-issues" ;;
    boundaries) echo "--fail-on-issues --fail-on-violations" ;;
    size) echo "--fail-on-issues --fail-on-growth" ;;
    complexity) echo "--fail-on-issues --fail-on-complexity" ;;
    dupes) echo "--fail-on-issues --fail-on-clones" ;;
    mutants) echo "--fail-on-issues --fail-on-survivors" ;;
    *) return 1 ;;
  esac
}

check_boolean() {
  case "$2" in
    true | false) ;;
    *) fail "Invalid '$1' input: $2. Use true or false." ;;
  esac
}

check_number() {
  if [ -n "$2" ] && ! [[ "$2" =~ ^[1-9][0-9]*$ ]]; then
    fail "Invalid '$1' input: $2. Use a positive whole number."
  fi
}

# The path in the form of the runner. On Windows, git bash gives `/d/a/...`,
# and a step that is not bash (for example upload-sarif) needs `D:/a/...`.
native_path() {
  if [ -n "$1" ] && command -v cygpath > /dev/null 2>&1; then
    cygpath -m "$1"
  else
    printf '%s' "$1"
  fi
}

# Write `name=value` to the step outputs. A value never holds a line break.
output() {
  printf '%s=%s\n' "$1" "${2//[$'\r\n']/ }" >> "$GITHUB_OUTPUT"
}

# The path from the repository root to the workspace root, with a trailing
# `/`. Empty when the workspace root is the repository root or is outside it.
path_prefix() {
  local manifest root workspace
  if [ -n "$INPUT_MANIFEST_PATH" ]; then
    manifest="$(cargo locate-project --workspace --message-format plain --manifest-path "$INPUT_MANIFEST_PATH" 2> /dev/null)" || return 0
  else
    manifest="$(cargo locate-project --workspace --message-format plain 2> /dev/null)" || return 0
  fi
  # On Windows, Cargo gives `D:\a\repo\Cargo.toml`. `dirname` reads only `/`.
  manifest="${manifest//\\//}"
  root="$(cd "$(dirname "$manifest")" && pwd -P)" || return 0
  workspace="$(cd "$GITHUB_WORKSPACE" && pwd -P)" || return 0
  case "$root" in
    "$workspace") ;;
    "$workspace"/*) printf '%s/' "${root#"$workspace"/}" ;;
    *) echo "::warning::The workspace root ${root} is outside the repository, so annotation paths can be wrong." >&2 ;;
  esac
}

COMMAND="$INPUT_COMMAND"
GATE_FLAGS="$(gate_flags "$COMMAND")" \
  || fail "Invalid 'command' input: ${COMMAND}. Use check, deps, files, visibility, boundaries, size, complexity, dupes or mutants."
case "$INPUT_FORMAT" in
  human | json | sarif) ;;
  *) fail "Invalid 'format' input: ${INPUT_FORMAT}. Use human, json or sarif." ;;
esac
check_boolean fail-on-issues "$INPUT_FAIL_ON_ISSUES"
check_number max-mutants "$INPUT_MAX_MUTANTS"
check_number time-budget "$INPUT_TIME_BUDGET"
check_number jobs "$INPUT_JOBS"
check_number timeout "$INPUT_TIMEOUT"

# `read -a` splits on white space and does no glob or command expansion, so
# a value such as `$(id)` stays one literal argument. `-d ''` reads past a line
# break, and then `read` returns 1 at the end of the input.
EXTRA=()
if [ -n "$INPUT_ARGS" ]; then
  read -r -d '' -a EXTRA <<< "$INPUT_ARGS" || true
fi
for arg in "${EXTRA[@]+"${EXTRA[@]}"}"; do
  case "$arg" in
    --format | --format=* | --quiet)
      fail "The 'args' input cannot hold ${arg}: the action sets it. Use the 'format' input."
      ;;
  esac
done

ARGS=("$COMMAND")
if [ -n "$INPUT_MANIFEST_PATH" ]; then
  ARGS+=(--manifest-path "$INPUT_MANIFEST_PATH")
fi
if [ "$INPUT_FAIL_ON_ISSUES" = "true" ]; then
  read -r -a FLAGS <<< "$GATE_FLAGS"
  has_flag=false
  for arg in "${EXTRA[@]+"${EXTRA[@]}"}"; do
    for flag in "${FLAGS[@]}"; do
      [ "$arg" = "$flag" ] && has_flag=true
    done
  done
  [ "$has_flag" = "true" ] || ARGS+=(--fail-on-issues)
fi

BASE="$INPUT_BASE"
if [ "$BASE" = "auto" ]; then
  BASE="$PR_BASE_SHA"
  if [ -z "$BASE" ]; then
    echo "::notice::base: auto needs a pull request event. The run compares with no base."
  fi
fi
if [ -n "$BASE" ]; then
  case "$COMMAND" in
    size) ARGS+=(--base "$BASE") ;;
    mutants) ARGS+=(--in-diff "$BASE") ;;
    *) echo "::warning::The 'base' input applies only to size and mutants. The ${COMMAND} run ignores it." ;;
  esac
fi

MUTANTS_INPUTS="${INPUT_PACKAGE}${INPUT_MAX_MUTANTS}${INPUT_TIME_BUDGET}${INPUT_JOBS}${INPUT_TIMEOUT}"
if [ "$COMMAND" = "mutants" ]; then
  [ -z "$INPUT_PACKAGE" ] || ARGS+=(--package "$INPUT_PACKAGE")
  [ -z "$INPUT_MAX_MUTANTS" ] || ARGS+=(--max-mutants "$INPUT_MAX_MUTANTS")
  [ -z "$INPUT_TIME_BUDGET" ] || ARGS+=(--time-budget "$INPUT_TIME_BUDGET")
  [ -z "$INPUT_JOBS" ] || ARGS+=(--jobs "$INPUT_JOBS")
  [ -z "$INPUT_TIMEOUT" ] || ARGS+=(--timeout "$INPUT_TIMEOUT")
elif [ -n "$MUTANTS_INPUTS" ]; then
  echo "::warning::The mutants-* inputs apply only to command: mutants. The ${COMMAND} run ignores them."
fi
ARGS+=("${EXTRA[@]+"${EXTRA[@]}"}")

# Each step gets its own folder, so a second step in the job does not replace
# the files that the outputs of the first step name.
if [ -n "$INPUT_ARTIFACTS_DIR" ]; then
  OUT_DIR="$INPUT_ARTIFACTS_DIR"
  mkdir -p "$OUT_DIR"
else
  mkdir -p "$RUNNER_TEMP"
  OUT_DIR="$(mktemp -d "${RUNNER_TEMP}/cargo-fallow.XXXXXX")"
fi
OUT_DIR="$(cd "$OUT_DIR" && pwd -P)"
RESULTS="${OUT_DIR}/cargo-fallow-results.json"
SARIF="${OUT_DIR}/cargo-fallow-results.sarif"
HUMAN="${OUT_DIR}/cargo-fallow-report.txt"
STDERR_LOG="${OUT_DIR}/cargo-fallow-stderr.log"

cd "$INPUT_WORKING_DIRECTORY" || fail "The working-directory ${INPUT_WORKING_DIRECTORY} does not exist."

echo "Running: cargo fallow ${ARGS[*]} --format json"
# stdout goes to the report file and stderr goes through `tee`, so the log
# shows the progress lines while the run is busy.
set +e
"$CARGO_FALLOW_BIN" "${ARGS[@]}" --format json 2>&1 > "$RESULTS" | tee "$STDERR_LOG" >&2
EXIT_CODE="${PIPESTATUS[0]}"
set -e

if ! jq -e 'type == "object"' "$RESULTS" > /dev/null 2>&1; then
  echo "::error::cargo fallow ${COMMAND} exited with ${EXIT_CODE} and wrote no JSON report. ${STDERR_LOG} holds its stderr."
  jq -n --arg message "cargo fallow wrote no JSON report" --argjson code "$EXIT_CODE" \
    '{error: true, message: $message, exit_code: $code, code: "ACTION_NO_REPORT"}' > "$RESULTS"
  [ "$EXIT_CODE" -ne 0 ] || EXIT_CODE=2
fi
if jq -e '.error == true' "$RESULTS" > /dev/null; then
  echo "::error::cargo fallow ${COMMAND}: $(jq -r '.message // "unknown error"' "$RESULTS" | tr '\r\n' '  ')"
  # An error document is an execution error, whatever the exit code says.
  if [ "$EXIT_CODE" -lt 2 ]; then
    EXIT_CODE=2
  fi
fi

# The other forms. Their exit code is not the verdict, but exit code 2 means
# that the form is missing.
PREFIX="$(path_prefix)"
WROTE_SARIF=false
if jq -e '.error == true' "$RESULTS" > /dev/null; then
  :
elif [ "$COMMAND" = "mutants" ]; then
  jq -f "${ACTION_JQ_DIR}/mutants-sarif.jq" "$RESULTS" > "$SARIF"
  WROTE_SARIF=true
else
  set +e
  "$CARGO_FALLOW_BIN" "${ARGS[@]}" --format sarif --quiet > "$SARIF" 2>> "$STDERR_LOG"
  sarif_exit=$?
  set -e
  if [ "$sarif_exit" -le 1 ] && jq -e '.runs' "$SARIF" > /dev/null 2>&1; then
    WROTE_SARIF=true
  else
    echo "::warning::The SARIF run exited with ${sarif_exit}. The action has no SARIF log for annotations and upload."
  fi
fi
if [ "$WROTE_SARIF" = "true" ] && [ -n "$PREFIX" ]; then
  jq --arg prefix "$PREFIX" -f "${ACTION_JQ_DIR}/sarif-prefix.jq" "$SARIF" > "${SARIF}.tmp"
  mv "${SARIF}.tmp" "$SARIF"
fi

REPORT="$RESULTS"
case "$INPUT_FORMAT" in
  sarif)
    REPORT=""
    [ "$WROTE_SARIF" = "false" ] || REPORT="$SARIF"
    ;;
  human)
    if [ "$COMMAND" = "mutants" ]; then
      echo "The mutants run has no second human run. The progress lines above and the job summary show its result."
    elif ! jq -e '.error == true' "$RESULTS" > /dev/null; then
      set +e
      "$CARGO_FALLOW_BIN" "${ARGS[@]}" --format human --quiet > "$HUMAN" 2>> "$STDERR_LOG"
      set -e
      cat "$HUMAN"
      REPORT="$HUMAN"
    fi
    ;;
esac

output exit-code "$EXIT_CODE"
output results "$(native_path "$RESULTS")"
output report "$(native_path "$REPORT")"
if [ "$WROTE_SARIF" = "true" ]; then
  output sarif "$(native_path "$SARIF")"
else
  output sarif ""
fi
output path-prefix "$PREFIX"
jq -r -f "${ACTION_JQ_DIR}/gates.jq" "$RESULTS" >> "$GITHUB_OUTPUT"
