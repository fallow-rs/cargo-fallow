#!/usr/bin/env bash
# Tests for the shell scripts and the jq filters of the GitHub Action.
# Run: bash action/tests/run.sh
#
# The fixtures under action/tests/fixtures are real cargo-fallow output.
# mutants.json and mutants.sarif come from one run on
# tests/fixtures/action-mutants. A stub binary stands in for cargo-fallow, so
# the tests need only bash and jq.

# The tests compare literal backticks and `$` in single quotes on purpose.
# shellcheck disable=SC2016

set -uo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
ACTION_DIR="$(cd "$DIR/.." && pwd)"
JQ_DIR="$ACTION_DIR/jq"
SCRIPTS="$ACTION_DIR/scripts"
FIXTURES="$DIR/fixtures"
PASSED=0
FAILED=0
FAILURES=()

WORK="$(mktemp -d "${TMPDIR:-/tmp}/cargo-fallow-action-test.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

pass() { PASSED=$((PASSED + 1)); echo "  ok   $1"; }
fail() { FAILED=$((FAILED + 1)); FAILURES+=("$1: $2"); echo "  FAIL $1: $2"; }

assert_contains() {
  if [[ "$1" == *"$2"* ]]; then pass "$3"; else fail "$3" "expected to contain: $2"; fi
}

assert_not_contains() {
  if [[ "$1" == *"$2"* ]]; then fail "$3" "must not contain: $2"; else pass "$3"; fi
}

assert_equals() {
  if [ "$1" = "$2" ]; then pass "$3"; else fail "$3" "expected '$2', got '$1'"; fi
}

line_count() {
  if [ -z "$1" ]; then echo 0; else printf '%s\n' "$1" | awk 'END { print NR }'; fi
}

# A PATH with jq and the base tools, without cargo, cargo-fallow and gh.
TOOLS="$WORK/tools"
mkdir -p "$TOOLS"
ln -s "$(command -v jq)" "$TOOLS/jq"
BASE_PATH="$TOOLS:/usr/bin:/bin"

# --- The stub binary ---------------------------------------------------------
#
# The stub writes each argument list to $STUB_LOG, one argument for each line
# and an empty line after each call. It prints the fixture $STUB_<FORMAT> and
# exits with $STUB_EXIT.

STUB_DIR="$WORK/stub"
mkdir -p "$STUB_DIR"
cat > "$STUB_DIR/cargo-fallow" << 'STUB'
#!/usr/bin/env bash
format=human
previous=""
for arg in "$@"; do
  printf '%s\n' "$arg" >> "$STUB_LOG"
  [ "$previous" = "--format" ] && format="$arg"
  previous="$arg"
done
printf '\n' >> "$STUB_LOG"
echo "stub progress line" >&2
case "$format" in
  json) cat "$STUB_JSON" ;;
  sarif) cat "$STUB_SARIF" ;;
  human) echo "stub human report" ;;
esac
exit "${STUB_EXIT:-0}"
STUB
chmod +x "$STUB_DIR/cargo-fallow"

# Runs analyze.sh with the stub. Sets OUT (stdout and stderr), CODE (the exit
# code of the step), OUTPUTS (the GITHUB_OUTPUT file) and CALLS (the stub log).
run_analyze() {
  local case_dir="$WORK/case-$RANDOM$RANDOM"
  mkdir -p "$case_dir/artifacts"
  : > "$case_dir/outputs"
  : > "$case_dir/calls"
  OUT="$(cd "$case_dir" && env -i \
    PATH="$STUB_DIR:$BASE_PATH" HOME="$case_dir" \
    STUB_LOG="$case_dir/calls" STUB_JSON="${STUB_JSON:-$FIXTURES/complexity.json}" \
    STUB_SARIF="${STUB_SARIF:-$FIXTURES/complexity.sarif}" STUB_EXIT="${STUB_EXIT:-0}" \
    ACTION_JQ_DIR="$JQ_DIR" GITHUB_OUTPUT="$case_dir/outputs" GITHUB_WORKSPACE="$case_dir" \
    RUNNER_TEMP="$case_dir" INPUT_ARTIFACTS_DIR="$case_dir/artifacts" "$@" \
    bash "$SCRIPTS/analyze.sh" 2>&1)"
  CODE=$?
  OUTPUTS="$(cat "$case_dir/outputs")"
  CALLS="$(cat "$case_dir/calls")"
  CASE_DIR="$case_dir"
}

# The value of one step output.
output_of() {
  printf '%s\n' "$OUTPUTS" | awk -v key="$1=" 'index($0, key) == 1 { print substr($0, length(key) + 1); exit }'
}

# The argument list of call $1 (1-based), with the arguments on one line.
call_args() {
  printf '%s\n' "$CALLS" | awk -v n="$1" 'BEGIN { call = 1 } /^$/ { call++; next } call == n { printf "%s ", $0 }'
}

# `$(...)` drops the empty line after the last call, so the count of empty
# lines is one less than the count of calls.
calls_made() {
  if [ -z "$CALLS" ]; then echo 0; return; fi
  printf '%s\n' "$CALLS" | awk '/^$/ { n++ } END { print n + 1 }'
}

# --- annotations.jq ----------------------------------------------------------

echo "annotations.jq"
OUT="$(jq -r --arg max 50 --arg prefix "" -f "$JQ_DIR/annotations.jq" "$FIXTURES/complexity.sarif")"
assert_equals "$(line_count "$OUT")" "3" "one line for each result"
assert_contains "$OUT" "::warning file=src/deps/manifest.rs,line=70,col=8,title=cargo-fallow complexity::\`entries\` has cyclomatic complexity 20" "a warning has file, line, column and title"
OUT="$(jq -r --arg max 1 --arg prefix "" -f "$JQ_DIR/annotations.jq" "$FIXTURES/complexity.sarif")"
assert_equals "$(line_count "$OUT")" "1" "max limits the lines"
OUT="$(jq -r --arg max 5 --arg prefix "" -f "$JQ_DIR/annotations.jq" "$FIXTURES/dupes.sarif")"
assert_contains "$OUT" "line=47,col=1,endLine=68," "an end line goes into endLine"
OUT="$(jq -r --arg max 5 --arg prefix "crates/app/" -f "$JQ_DIR/annotations.jq" "$FIXTURES/mutants.sarif")"
assert_contains "$OUT" "file=crates/app/src/lib.rs,line=1,title=cargo-fallow surviving-mutants::" "the prefix goes before the path"
OUT="$(jq -r --arg max 50 --arg prefix "" -f "$JQ_DIR/annotations.jq" "$FIXTURES/check-clean.sarif")"
assert_equals "$OUT" "" "a log with no result gives no line"

HOSTILE='{"runs":[{"results":[
  {"ruleId":"r,1","level":"error","message":{"text":"100% bad\nline two\r::error::x"},
   "locations":[{"physicalLocation":{"artifactLocation":{"uri":"a,b:c.rs"},"region":{"startLine":3}}}]},
  {"ruleId":"r","level":"note","message":{"text":"n"},"locations":[]},
  {"ruleId":"r","message":{"text":"no level"}}]}]}'
OUT="$(printf '%s' "$HOSTILE" | jq -r --arg max 50 --arg prefix "" -f "$JQ_DIR/annotations.jq")"
assert_equals "$(line_count "$OUT")" "3" "a line break in a message does not make a new line"
assert_contains "$OUT" "::error file=a%2Cb%3Ac.rs,line=3,title=cargo-fallow r%2C1::100%25 bad%0Aline two%0D::error::x" "properties and data are escaped"
assert_contains "$OUT" "::notice title=cargo-fallow r::n" "note becomes notice, and no location gives no file"
assert_contains "$OUT" "::warning title=cargo-fallow r::no level" "a result with no level is a warning"

ORDER='{"runs":[{"results":[
  {"ruleId":"n","level":"note","message":{"text":"first"}},
  {"ruleId":"w","level":"warning","message":{"text":"second"}},
  {"ruleId":"e","level":"error","message":{"text":"third"}}]}]}'
OUT="$(printf '%s' "$ORDER" | jq -r --arg max 2 --arg prefix "" -f "$JQ_DIR/annotations.jq")"
assert_equals "$OUT" "::error title=cargo-fallow e::third
::warning title=cargo-fallow w::second" "errors come first, then warnings, then notices"
OUT="$(printf '%s' '{"runs":[{"results":[{"ruleId":"r","level":"warning","message":{"text":"m"},"locations":[{"physicalLocation":{"artifactLocation":{"uriBaseId":"X"}}}]}]}]}' \
  | jq -c --arg prefix "sub/" -f "$JQ_DIR/sarif-prefix.jq")"
assert_not_contains "$OUT" '"uri"' "the prefix adds no uri where the log has none"

# --- summary.jq --------------------------------------------------------------

echo "summary.jq"
summary() {
  local json="$1" sarif="$2" exit_code="$3" max="$4"
  if [ -n "$sarif" ]; then
    jq -r --arg command x --arg exit_code "$exit_code" --arg max "$max" --arg prefix "" \
      --slurpfile sarif "$sarif" -f "$JQ_DIR/summary.jq" "$json"
  else
    jq -r --arg command check --arg exit_code "$exit_code" --arg max "$max" --arg prefix "" \
      --argjson sarif '[]' -f "$JQ_DIR/summary.jq" "$json"
  fi
}
OUT="$(summary "$FIXTURES/complexity.json" "$FIXTURES/complexity.sarif" 0 2)"
assert_contains "$OUT" "### cargo fallow complexity" "the title names the kind"
assert_contains "$OUT" "3 issues, exit code 0." "the issue count and the exit code"
assert_contains "$OUT" "| \`health-findings\` | fail | no | 3 |" "a gate row"
assert_contains "$OUT" "#### Findings (2 of 3)" "the findings title shows the cap"
assert_contains "$OUT" "| \`src/deps/manifest.rs:70\` | \`complexity\` |" "a finding row"
OUT="$(summary "$FIXTURES/check-clean.json" "$FIXTURES/check-clean.sarif" 0 50)"
assert_contains "$OUT" "| \`error-severity-findings\` | pass | yes | 0 |" "an enforced gate"
assert_not_contains "$OUT" "#### Findings" "a clean run has no findings table"
OUT="$(summary "$FIXTURES/visibility.json" "$FIXTURES/visibility.sarif" 0 50)"
assert_contains "$OUT" "1 issue, exit code 0." "one issue is singular"
OUT="$(summary "$FIXTURES/mutants.json" "$FIXTURES/mutants.sarif" 0 50)"
assert_contains "$OUT" "Mutation score 66.7%: 2 killed, 1 survived, 3 no coverage, 0 timeout, 0 pending." "the mutation score line"
OUT="$(summary "$FIXTURES/error.json" "" 2 50)"
assert_contains "$OUT" "The run failed with exit code 2: the manifest" "an error document gives the message"
assert_contains "$OUT" "- Code: \`CARGO_FALLOW_MANIFEST_NOT_FOUND\`" "an error document gives the code"
OUT="$(printf '%s' '{"kind":"x","total_issues":1,"gate_outcomes":{}}' > "$WORK/pipe.json"
  printf '%s' '[{"runs":[{"results":[{"ruleId":"r","message":{"text":"a | b\nc"},"locations":[{"physicalLocation":{"artifactLocation":{"uri":"f.rs"}}}]}]}]}]' > "$WORK/pipe.sarif"
  jq -r --arg command x --arg exit_code 0 --arg max 5 --arg prefix "" --argjson sarif "$(cat "$WORK/pipe.sarif")" -f "$JQ_DIR/summary.jq" "$WORK/pipe.json")"
assert_contains "$OUT" "| \`f.rs\` | \`r\` | a \\| b c |" "a pipe and a line break in a cell are escaped"
OUT="$(printf '%s' '{"kind":"x","total_issues":1,"gate_outcomes":{}}' > "$WORK/pipe.json"
  jq -r --arg command x --arg exit_code 0 --arg max 5 --arg prefix "" \
    --argjson sarif '[{"runs":[{"results":[{"ruleId":"r","message":{"text":"m"},"locations":[{"physicalLocation":{"artifactLocation":{"uri":"a|b.rs"},"region":{"startLine":3}}}]}]}]}]' \
    -f "$JQ_DIR/summary.jq" "$WORK/pipe.json")"
assert_contains "$OUT" "| \`a\\|b.rs:3\` |" "a pipe in a path is escaped"

# --- gates.jq ----------------------------------------------------------------

echo "gates.jq"
OUT="$(jq -r -f "$JQ_DIR/gates.jq" "$FIXTURES/mutants.json")"
assert_contains "$OUT" "issues=1" "issues from total_issues"
assert_contains "$OUT" "gates-failed=surviving-mutants" "failed gates"
assert_contains "$OUT" "mutation-score=66.7" "the mutation score has one decimal"
OUT="$(jq -r -f "$JQ_DIR/gates.jq" "$FIXTURES/check-clean.json")"
assert_contains "$OUT" "gates-passed=error-severity-findings,parse-error" "passed gates, sorted"
assert_contains "$OUT" "mutation-score=" "no score for a static command"
OUT="$(jq -r -f "$JQ_DIR/gates.jq" "$FIXTURES/error.json")"
assert_contains "$OUT" "error-code=CARGO_FALLOW_MANIFEST_NOT_FOUND" "the error code"
OUT="$(printf '%s' '{"error":true,"code":"A\nB=C"}' | jq -r -f "$JQ_DIR/gates.jq")"
assert_contains "$OUT" "error-code=A B=C" "a line break in a value does not make a new output"

# --- mutants-sarif.jq --------------------------------------------------------

echo "mutants-sarif.jq"
if diff <(jq -S . "$FIXTURES/mutants.sarif") <(jq -S -f "$JQ_DIR/mutants-sarif.jq" "$FIXTURES/mutants.json") > "$WORK/drift.diff"; then
  pass "the filter gives the SARIF log of the real run"
else
  fail "the filter gives the SARIF log of the real run" "$(head -20 "$WORK/drift.diff")"
fi
OUT="$(jq -c '.mutants[0].reason = "test t failed alone" | .gate_outcomes["surviving-mutants"].enforced = true' "$FIXTURES/mutants.json" \
  | jq -c -f "$JQ_DIR/mutants-sarif.jq")"
assert_contains "$OUT" '"text":"Each test passed one at a time when `{ a == 2 }` became `{ true }` (return-value): test t failed alone."' "a survivor with a reason"
assert_contains "$OUT" '"level":"error"' "an enforced failure is an error"
assert_contains "$OUT" '"text":"The `surviving-mutants` gate failed (enforced)."' "the enforced notification"
OUT="$(jq -c '.mutants |= map(select(.status != "survived")) | .gate_outcomes["surviving-mutants"] = {status: "warn", enforced: false}' "$FIXTURES/mutants.json" \
  | jq -c -f "$JQ_DIR/mutants-sarif.jq")"
assert_contains "$OUT" '"rules":[]' "no survivor gives no rule"
assert_contains "$OUT" '"results":[]' "no survivor gives no result"
assert_contains "$OUT" '"text":"The `surviving-mutants` gate warned (not enforced)."' "a warn gate notification"

# --- sarif-prefix.jq ---------------------------------------------------------

echo "sarif-prefix.jq"
OUT="$(jq -r --arg prefix "sub/" -f "$JQ_DIR/sarif-prefix.jq" "$FIXTURES/dupes.sarif" | jq -r '[.. | objects | .artifactLocation?.uri // empty] | unique | join(" ")')"
assert_equals "$OUT" "sub/tests/cli.rs sub/tests/structure.rs" "each location and related location gets the prefix"

# --- analyze.sh --------------------------------------------------------------

echo "analyze.sh"
run_analyze INPUT_COMMAND=check STUB_JSON="$FIXTURES/check-clean.json" STUB_SARIF="$FIXTURES/check-clean.sarif"
assert_equals "$CODE" "0" "check: the step passes"
assert_equals "$(call_args 1)" "check --fail-on-issues --format json " "check: the JSON run has the gate flag"
assert_equals "$(call_args 2)" "check --fail-on-issues --format sarif --quiet " "check: the SARIF run"
assert_equals "$(call_args 3)" "check --fail-on-issues --format human --quiet " "check: the human run for format human"
assert_contains "$OUT" "stub progress line" "the log shows the stderr of the JSON run"
assert_contains "$OUT" "stub human report" "the log shows the human report"
assert_equals "$(output_of exit-code)" "0" "check: exit-code output"
assert_equals "$(output_of issues)" "0" "check: issues output"
assert_equals "$(output_of results)" "$(cd "$CASE_DIR/artifacts" && pwd -P)/cargo-fallow-results.json" "check: results output is the absolute path"
assert_equals "$(output_of report)" "$(cd "$CASE_DIR/artifacts" && pwd -P)/cargo-fallow-report.txt" "check: report output is the human report"
assert_equals "$(output_of path-prefix)" "" "check: no cargo, so no prefix"

for pair in check:--fail-on-issues deps:--fail-on-issues visibility:--fail-on-issues \
  boundaries:--fail-on-violations size:--fail-on-growth complexity:--fail-on-complexity \
  dupes:--fail-on-clones; do
  run_analyze INPUT_COMMAND="${pair%%:*}" INPUT_FORMAT=json
  assert_equals "$(call_args 1)" "${pair%%:*} ${pair#*:} --format json " "${pair%%:*}: the gate flag is ${pair#*:}"
done

run_analyze INPUT_COMMAND=complexity INPUT_FAIL_ON_ISSUES=false INPUT_FORMAT=sarif
assert_equals "$(call_args 1)" "complexity --format json " "fail-on-issues false: no gate flag"
assert_equals "$(calls_made)" "2" "format sarif: no human run"
assert_equals "$(output_of report)" "$(output_of sarif)" "format sarif: report is the SARIF log"
assert_equals "$(output_of gates-failed)" "health-findings" "gates-failed output"

run_analyze INPUT_COMMAND=complexity INPUT_ARGS="--fail-on-complexity --max-cyclomatic 10"
assert_equals "$(call_args 1)" "complexity --fail-on-complexity --max-cyclomatic 10 --format json " "a gate flag in args is not given twice"

MARKER="$WORK/pwned"
run_analyze INPUT_COMMAND=dupes INPUT_FORMAT=json INPUT_ARGS="--min-tokens 40 \$(touch $MARKER) ;touch $MARKER \`touch $MARKER\` *
--skip-local"
assert_equals "$(call_args 1)" "dupes --fail-on-clones --min-tokens 40 \$(touch $MARKER) ;touch $MARKER \`touch $MARKER\` * --skip-local --format json " "args stay literal, also after a line break"
if [ -e "$MARKER" ]; then fail "args run no command" "the marker file exists"; else pass "args run no command"; fi

for bad in "--format json" "--format=sarif" "--quiet"; do
  run_analyze INPUT_COMMAND=check INPUT_ARGS="$bad"
  assert_equals "$CODE" "1" "args '$bad' stops the step"
  assert_contains "$OUT" "::error::The 'args' input cannot hold" "args '$bad' explains the failure"
  assert_equals "$(calls_made)" "0" "args '$bad' runs nothing"
done

run_analyze INPUT_COMMAND="check; touch $MARKER"
assert_equals "$CODE" "1" "an unknown command stops the step"
assert_contains "$OUT" "Invalid 'command' input" "an unknown command explains the failure"
run_analyze INPUT_COMMAND=check INPUT_FORMAT=xml
assert_contains "$OUT" "Invalid 'format' input" "an unknown format stops the step"
run_analyze INPUT_COMMAND=check INPUT_FAIL_ON_ISSUES=yes
assert_contains "$OUT" "Invalid 'fail-on-issues' input" "a bad boolean stops the step"
run_analyze INPUT_COMMAND=mutants INPUT_JOBS=0
assert_contains "$OUT" "Invalid 'jobs' input" "jobs 0 stops the step"
run_analyze INPUT_COMMAND=mutants INPUT_TIMEOUT="5; id"
assert_contains "$OUT" "Invalid 'timeout' input" "a number with text stops the step"

run_analyze INPUT_COMMAND=mutants STUB_JSON="$FIXTURES/mutants.json" STUB_SARIF=/dev/null \
  INPUT_PACKAGE=my-crate INPUT_MAX_MUTANTS=10 INPUT_TIME_BUDGET=600 INPUT_JOBS=2 INPUT_TIMEOUT=30 \
  INPUT_BASE=auto PR_BASE_SHA=0123abc
assert_equals "$(calls_made)" "1" "mutants: one run"
assert_equals "$(call_args 1)" "mutants --fail-on-survivors --in-diff 0123abc --package my-crate --max-mutants 10 --time-budget 600 --jobs 2 --timeout 30 --format json " "mutants: the inputs become flags"
if diff <(jq -S . "$(output_of sarif)") <(jq -S . "$FIXTURES/mutants.sarif") > /dev/null; then
  pass "mutants: the SARIF log comes from the JSON report"
else
  fail "mutants: the SARIF log comes from the JSON report" "the logs differ"
fi
assert_equals "$(output_of mutation-score)" "66.7" "mutants: mutation-score output"
assert_contains "$OUT" "The mutants run has no second human run" "mutants: format human explains the log"

run_analyze INPUT_COMMAND=mutants INPUT_BASE=auto STUB_JSON="$FIXTURES/mutants.json"
assert_equals "$(call_args 1)" "mutants --fail-on-survivors --format json " "base auto with no pull request gives no flag"
assert_contains "$OUT" "base: auto needs a pull request event" "base auto with no pull request explains it"
run_analyze INPUT_COMMAND=size INPUT_BASE=origin/main INPUT_FORMAT=json
assert_equals "$(call_args 1)" "size --fail-on-growth --base origin/main --format json " "size: base becomes --base"
run_analyze INPUT_COMMAND=complexity INPUT_BASE=origin/main INPUT_PACKAGE=x INPUT_FORMAT=json
assert_equals "$(call_args 1)" "complexity --fail-on-complexity --format json " "complexity: base and mutants inputs give no flags"
assert_contains "$OUT" "The 'base' input applies only to size and mutants" "complexity: base gives a warning"
assert_contains "$OUT" "The mutants-* inputs apply only to command: mutants" "complexity: mutants inputs give a warning"

run_analyze INPUT_COMMAND=check INPUT_MANIFEST_PATH="crates/a b/Cargo.toml" INPUT_FORMAT=json
assert_equals "$(call_args 1)" "check --manifest-path crates/a b/Cargo.toml --fail-on-issues --format json " "a manifest path with a space stays one argument"

run_analyze INPUT_COMMAND=check STUB_JSON="$FIXTURES/error.json" STUB_EXIT=2
assert_equals "$CODE" "0" "exit code 2: the step itself passes"
assert_equals "$(output_of exit-code)" "2" "exit code 2: exit-code output"
assert_equals "$(output_of error-code)" "CARGO_FALLOW_MANIFEST_NOT_FOUND" "exit code 2: error-code output"
assert_equals "$(output_of sarif)" "" "exit code 2: no SARIF log"
assert_equals "$(calls_made)" "1" "exit code 2: no other run"
assert_contains "$OUT" "::error::cargo fallow check: the manifest" "exit code 2: the log shows the message"

printf 'not json' > "$WORK/garbage"
run_analyze INPUT_COMMAND=check STUB_JSON="$WORK/garbage" STUB_EXIT=0
assert_equals "$(output_of exit-code)" "2" "no JSON report: exit code 2"
assert_equals "$(output_of error-code)" "ACTION_NO_REPORT" "no JSON report: error code"

run_analyze INPUT_COMMAND=complexity STUB_EXIT=1 INPUT_FORMAT=json
assert_equals "$(output_of exit-code)" "1" "exit code 1 goes to the output"
assert_equals "$(output_of sarif)" "$(cd "$CASE_DIR/artifacts" && pwd -P)/cargo-fallow-results.sarif" "exit code 1 still gives the SARIF log"

# Two steps in one job share RUNNER_TEMP. Without artifacts-dir, each step
# must get its own folder, so the outputs of the first step stay valid.
SHARED="$WORK/shared-temp"
mkdir -p "$SHARED"
run_analyze INPUT_COMMAND=deps INPUT_FORMAT=json INPUT_ARTIFACTS_DIR="" RUNNER_TEMP="$SHARED" STUB_JSON="$FIXTURES/check-clean.json"
FIRST_RESULTS="$(output_of results)"
run_analyze INPUT_COMMAND=check INPUT_FORMAT=json INPUT_ARTIFACTS_DIR="" RUNNER_TEMP="$SHARED" STUB_JSON="$FIXTURES/error.json" STUB_EXIT=2
if [ "$FIRST_RESULTS" != "$(output_of results)" ] && [ "$(jq -r .kind "$FIRST_RESULTS")" = "check" ]; then
  pass "each step without artifacts-dir gets its own folder"
else
  fail "each step without artifacts-dir gets its own folder" "the second step replaced ${FIRST_RESULTS}"
fi

# On Windows, git bash gives /d/a/... paths, and a node step cannot read them.
# A stub cygpath shows that the outputs go through `cygpath -m`.
CYGPATH_STUB="$WORK/cygpath-stub"
mkdir -p "$CYGPATH_STUB"
printf '#!/usr/bin/env bash\nprintf "W:%%s\\n" "$2"\n' > "$CYGPATH_STUB/cygpath"
chmod +x "$CYGPATH_STUB/cygpath"
run_analyze INPUT_COMMAND=complexity INPUT_FORMAT=sarif PATH="$STUB_DIR:$CYGPATH_STUB:$BASE_PATH"
assert_contains "$(output_of results)" "W:/" "Windows: results is a native path"
assert_contains "$(output_of sarif)" "W:/" "Windows: sarif is a native path"
assert_contains "$(output_of report)" "W:/" "Windows: report is a native path"
assert_equals "$(output_of exit-code)" "0" "Windows: the run still reads its own files"

run_analyze INPUT_COMMAND=check STUB_JSON="$FIXTURES/error.json" STUB_EXIT=0
assert_equals "$(output_of exit-code)" "2" "an error document with exit code 0 gives exit code 2"

# A stub cargo for `cargo locate-project`: the workspace is in a subfolder.
CARGO_STUB="$WORK/cargo-stub"
mkdir -p "$CARGO_STUB"
cat > "$CARGO_STUB/cargo" << 'STUB'
#!/usr/bin/env bash
mkdir -p "$GITHUB_WORKSPACE/rust/ws"
echo "$GITHUB_WORKSPACE/rust/ws/Cargo.toml"
STUB
chmod +x "$CARGO_STUB/cargo"
run_analyze INPUT_COMMAND=complexity INPUT_FORMAT=json PATH="$STUB_DIR:$CARGO_STUB:$BASE_PATH"
assert_equals "$(output_of path-prefix)" "rust/ws/" "a workspace in a subfolder gives a prefix"
assert_equals "$(jq -r '.runs[0].results[0].locations[0].physicalLocation.artifactLocation.uri' "$(output_of sarif)")" "rust/ws/src/deps/manifest.rs" "the SARIF paths get the prefix"
cat > "$CARGO_STUB/cargo" << 'STUB'
#!/usr/bin/env bash
mkdir -p "$GITHUB_WORKSPACE/rust/ws"
printf '%s\\rust\\ws\\Cargo.toml\n' "$GITHUB_WORKSPACE"
STUB
run_analyze INPUT_COMMAND=complexity INPUT_FORMAT=json PATH="$STUB_DIR:$CARGO_STUB:$BASE_PATH"
assert_equals "$(output_of path-prefix)" "rust/ws/" "a manifest path with backslashes gives the same prefix"

# --- install.sh --------------------------------------------------------------

echo "install.sh"
SOURCE="$WORK/source"
mkdir -p "$SOURCE"
printf '[package]\nname = "cargo-fallow"\nversion = "0.0.0"\n' > "$SOURCE/Cargo.toml"
RELEASED="$WORK/released"
mkdir -p "$RELEASED"
printf '[package]\nname = "cargo-fallow"\nversion = "0.3.1"\n' > "$RELEASED/Cargo.toml"

run_install() {
  local case_dir="$WORK/install-$RANDOM$RANDOM"
  mkdir -p "$case_dir"
  : > "$case_dir/outputs"
  : > "$case_dir/path"
  OUT="$(env -i PATH="$BASE_PATH" HOME="$case_dir" RUNNER_TEMP="$case_dir" \
    GITHUB_OUTPUT="$case_dir/outputs" GITHUB_PATH="$case_dir/path" \
    RUNNER_OS=Linux RUNNER_ARCH=X64 ACTION_PATH="$SOURCE" CARGO_FALLOW_DRY_RUN=1 "$@" \
    bash "$SCRIPTS/install.sh" 2>&1)"
  CODE=$?
  OUTPUTS="$(cat "$case_dir/outputs")"
  INSTALL_CASE="$case_dir"
}

run_install INPUT_INSTALL=auto PATH="$STUB_DIR:$BASE_PATH"
assert_equals "$(output_of method)" "path" "auto: a binary on the PATH wins"
assert_equals "$(cat "$INSTALL_CASE/path")" "" "path: the PATH does not change"
run_install INPUT_INSTALL=auto INPUT_VERSION=0.3.1 PATH="$STUB_DIR:$BASE_PATH"
assert_equals "$(output_of method)" "release" "auto: a PATH binary of another version does not win over the version input"
run_install INPUT_INSTALL=auto
assert_equals "$(output_of method)" "source" "auto: an unreleased action ref builds the source"
assert_contains "$OUT" "DRY RUN: cargo install --locked --root $INSTALL_CASE/cargo-fallow-install --path $SOURCE" "source: cargo install --path on the action source"
assert_equals "$(cat "$INSTALL_CASE/path")" "$INSTALL_CASE/cargo-fallow-install/bin" "source: the bin folder goes on the PATH"
run_install INPUT_INSTALL=auto ACTION_PATH="$RELEASED"
assert_equals "$(output_of method)" "release" "auto: a released action ref takes the release binary"
assert_contains "$OUT" "DRY RUN: download https://github.com/fallow-rs/cargo-fallow/releases/download/v0.3.1/cargo-fallow-x86_64-unknown-linux-gnu.tar.xz" "release: the asset of the action version"
run_install INPUT_INSTALL=release ACTION_PATH="$RELEASED" INPUT_VERSION=v0.4.0 RUNNER_OS=Windows
assert_contains "$OUT" "/v0.4.0/cargo-fallow-x86_64-pc-windows-msvc.zip" "release: the version input wins, and Windows takes a zip"
run_install INPUT_INSTALL=release ACTION_PATH="$RELEASED" RUNNER_OS=Linux RUNNER_ARCH=ARM64
assert_contains "$OUT" "cargo-fallow-aarch64-unknown-linux-gnu.tar.xz" "release: Linux ARM64"
run_install INPUT_INSTALL=release
assert_equals "$CODE" "1" "release: an unreleased version fails"
assert_contains "$OUT" "install: release found no binary" "release: the failure names other methods"
run_install INPUT_INSTALL=release ACTION_PATH="$RELEASED" RUNNER_OS=FreeBSD
assert_equals "$CODE" "1" "release: an unknown runner fails"
run_install INPUT_INSTALL=cargo INPUT_VERSION=0.2.0
assert_contains "$OUT" "DRY RUN: cargo install --locked --root $INSTALL_CASE/cargo-fallow-install --version 0.2.0 cargo-fallow" "cargo: install the version from crates.io"
run_install INPUT_INSTALL=cargo
assert_contains "$OUT" "DRY RUN: cargo install --locked --root $INSTALL_CASE/cargo-fallow-install cargo-fallow" "cargo: an unreleased action ref takes the latest version"
run_install INPUT_INSTALL=path
assert_equals "$CODE" "1" "path: no binary on the PATH fails"
run_install INPUT_INSTALL=cargo INPUT_VERSION='1.0; rm -rf ~'
assert_equals "$CODE" "1" "an invalid version fails"
assert_contains "$OUT" "Invalid 'version' input" "an invalid version explains the failure"
run_install INPUT_INSTALL=npm
assert_contains "$OUT" "Invalid 'install' input" "an unknown method explains the failure"

# A real download from a file URL, with a good and with a bad checksum.
sha256() {
  if command -v sha256sum > /dev/null 2>&1; then sha256sum "$1" | awk '{ print $1 }'; else shasum -a 256 "$1" | awk '{ print $1 }'; fi
}
DIST="$WORK/dist/v0.3.1"
mkdir -p "$DIST" "$WORK/pack/cargo-fallow-x86_64-unknown-linux-gnu"
printf '#!/bin/sh\necho "cargo-fallow 0.3.1"\n' > "$WORK/pack/cargo-fallow-x86_64-unknown-linux-gnu/cargo-fallow"
chmod +x "$WORK/pack/cargo-fallow-x86_64-unknown-linux-gnu/cargo-fallow"
(cd "$WORK/pack" && tar -cJf "$DIST/cargo-fallow-x86_64-unknown-linux-gnu.tar.xz" cargo-fallow-x86_64-unknown-linux-gnu)
printf '%s  cargo-fallow-x86_64-unknown-linux-gnu.tar.xz\n' "$(sha256 "$DIST/cargo-fallow-x86_64-unknown-linux-gnu.tar.xz")" > "$DIST/cargo-fallow-x86_64-unknown-linux-gnu.tar.xz.sha256"
if command -v curl > /dev/null 2>&1 && command -v xz > /dev/null 2>&1; then
  run_install INPUT_INSTALL=release ACTION_PATH="$RELEASED" CARGO_FALLOW_DRY_RUN=0 CARGO_FALLOW_RELEASE_URL="file://$WORK/dist"
  assert_equals "$CODE" "0" "release download: a good checksum installs"
  assert_contains "$OUT" "cargo-fallow: cargo-fallow 0.3.1 (install: release)" "release download: the binary runs"
  printf '%s  x\n' "0000000000000000000000000000000000000000000000000000000000000000" > "$DIST/cargo-fallow-x86_64-unknown-linux-gnu.tar.xz.sha256"
  run_install INPUT_INSTALL=auto ACTION_PATH="$RELEASED" CARGO_FALLOW_DRY_RUN=0 CARGO_FALLOW_RELEASE_URL="file://$WORK/dist"
  assert_equals "$CODE" "1" "release download: a bad checksum fails, also for auto"
  assert_contains "$OUT" "The SHA-256 of cargo-fallow-x86_64-unknown-linux-gnu.tar.xz is" "release download: the failure names the checksum"
  assert_not_contains "$OUT" "Building cargo-fallow from the source" "release download: a bad checksum does not fall back"
  run_install INPUT_INSTALL=release ACTION_PATH="$RELEASED" CARGO_FALLOW_DRY_RUN=0 CARGO_FALLOW_RELEASE_URL="http://127.0.0.1:9/dist"
  assert_equals "$CODE" "1" "release download: a plain http URL is refused"
  assert_contains "$OUT" "No release asset at http://127.0.0.1:9/dist" "release download: the refused URL is named"
  assert_contains "$OUT" "Protocol" "release download: curl refuses the http protocol"
else
  echo "  skip release download: curl or xz is missing"
fi

echo "code-scanning.sh"
OUT="$(env -i PATH="$BASE_PATH" GH_REPO=o/r IS_FORK=true GITHUB_OUTPUT=/dev/stdout bash "$SCRIPTS/code-scanning.sh" 2>&1)"
assert_contains "$OUT" "available=false" "a fork pull request does not upload"
assert_contains "$OUT" "pull request from a fork" "a fork pull request explains the skip"

# --- annotate.sh, summary.sh and gate.sh -------------------------------------

echo "annotate.sh, summary.sh and gate.sh"
OUT="$(ACTION_JQ_DIR="$JQ_DIR" SARIF_FILE="$FIXTURES/complexity.sarif" MAX_ANNOTATIONS=2 bash "$SCRIPTS/annotate.sh" 2>&1)"
assert_equals "$(printf '%s\n' "$OUT" | grep -c '^::warning file=')" "2" "annotate: the cap limits the annotations"
assert_contains "$OUT" "::notice::cargo-fallow wrote 2 of 3 findings as annotations, errors first." "annotate: the cap gives a notice"
OUT="$(ACTION_JQ_DIR="$JQ_DIR" SARIF_FILE="" MAX_ANNOTATIONS=2 bash "$SCRIPTS/annotate.sh" 2>&1)"
assert_contains "$OUT" "No SARIF log, so no annotations." "annotate: no SARIF log"
OUT="$(ACTION_JQ_DIR="$JQ_DIR" SARIF_FILE="$FIXTURES/complexity.sarif" MAX_ANNOTATIONS=x bash "$SCRIPTS/annotate.sh" 2>&1)"
assert_contains "$OUT" "Invalid 'max-annotations' input" "annotate: a bad cap fails"

OUT="$(ACTION_JQ_DIR="$JQ_DIR" RESULTS_FILE="$FIXTURES/error.json" SARIF_FILE="" FALLOW_COMMAND=check EXIT_CODE=2 MAX_FINDINGS=50 \
  GITHUB_STEP_SUMMARY=/dev/stdout bash "$SCRIPTS/summary.sh" 2>&1)"
assert_contains "$OUT" "### cargo fallow check" "summary: an error document with no SARIF log"
OUT="$(ACTION_JQ_DIR="$JQ_DIR" RESULTS_FILE="$FIXTURES/dupes-missing.json" FALLOW_COMMAND=dupes EXIT_CODE=2 MAX_FINDINGS=50 \
  bash "$SCRIPTS/summary.sh" 2>&1)"
assert_contains "$OUT" "No JSON report, so no job summary." "summary: no report file"

gate() {
  OUT="$(env -i PATH="$BASE_PATH" FALLOW_COMMAND=dupes "$@" bash "$SCRIPTS/gate.sh" 2>&1)"
  CODE=$?
}
gate EXIT_CODE=0
assert_equals "$CODE" "0" "gate: exit code 0 passes"
gate EXIT_CODE=1 GATES_FAILED=duplication-findings
assert_equals "$CODE" "1" "gate: exit code 1 fails"
assert_contains "$OUT" "an enforced gate failed (duplication-findings)" "gate: the failure names the gates"
gate EXIT_CODE=2 ERROR_CODE=CARGO_FALLOW_MANIFEST_NOT_FOUND
assert_equals "$CODE" "2" "gate: exit code 2 fails with 2"
assert_contains "$OUT" "exit code 2, CARGO_FALLOW_MANIFEST_NOT_FOUND" "gate: the failure names the error code"
gate EXIT_CODE=""
assert_equals "$CODE" "2" "gate: no exit code fails"

# --- action.yml --------------------------------------------------------------

echo "action.yml"
ACTION_YML="$ACTION_DIR/../action.yml"
if grep -nE '^\s+run:.*\$\{\{' "$ACTION_YML" > "$WORK/interp"; then
  fail "no expression in a run script" "$(cat "$WORK/interp")"
else
  pass "no expression in a run script"
fi
if grep -nE '^\s+(- )?uses: [^.][^@]*@' "$ACTION_YML" | grep -vE '@[0-9a-f]{40} # v' > "$WORK/pins"; then
  fail "each action is pinned by SHA" "$(cat "$WORK/pins")"
else
  pass "each action is pinned by SHA"
fi
for script in "$SCRIPTS"/*.sh; do
  name="$(basename "$script")"
  if grep -q "action/scripts/${name}" "$ACTION_YML"; then pass "action.yml runs ${name}"; else fail "action.yml runs ${name}" "no step runs it"; fi
done

echo
echo "${PASSED} passed, ${FAILED} failed"
if [ "$FAILED" -gt 0 ]; then
  printf '  %s\n' "${FAILURES[@]}"
  exit 1
fi
