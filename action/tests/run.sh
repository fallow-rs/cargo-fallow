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
# install.sh checks the release signature with node.
if command -v node > /dev/null 2>&1; then ln -s "$(command -v node)" "$TOOLS/node"; fi
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
OUT="$(summary "$FIXTURES/api.json" "" 1 50)"
assert_contains "$OUT" "| \`api-removals\` | fail | yes | 1 |" "api: the gate row"
assert_contains "$OUT" "1 added, 1 removed, 1 changed against main." "api: one count line"
assert_contains "$OUT" "| demo | removed | \`demo::gone\` | \`pub fn gone ( )\` -> |" "api: a removed row"
assert_contains "$OUT" "| demo | changed | \`demo::reshaped\` | \`pub fn reshaped ( a : u8 )\` -> \`pub fn reshaped ( a : u16 )\` |" "api: a changed row"
assert_not_contains "$OUT" "demo::fresh" "api: an added item has no row"
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
OUT="$(jq -c '.gate_outcomes["surviving-mutants"].enforced = false' "$FIXTURES/mutants.json" \
  | jq -f "$JQ_DIR/mutants-sarif.jq" | jq -c '.runs[0].results | map(.level) | unique')"
assert_equals "$OUT" '["error"]' "a survivor is an error also when the gate is not enforced"
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

for command in check deps files visibility boundaries size complexity dupes mutants; do
  run_analyze INPUT_COMMAND="$command" INPUT_FORMAT=json
  assert_equals "$(call_args 1)" "$command --fail-on-issues --format json " "$command: the gate flag is --fail-on-issues"
done

run_analyze INPUT_COMMAND=api INPUT_BASE=main INPUT_FORMAT=json
assert_equals "$(call_args 1)" "api --fail-on-issues --base main --format json " "api: the gate flag and the base"
run_analyze INPUT_COMMAND=api INPUT_FORMAT=json
assert_equals "$CODE" "1" "api: no base stops the step"
assert_contains "$OUT" "command: api needs the base input" "api: the error names the base input"

run_analyze INPUT_COMMAND=complexity INPUT_FAIL_ON_ISSUES=false INPUT_FORMAT=sarif
assert_equals "$(call_args 1)" "complexity --format json " "fail-on-issues false: no gate flag"
assert_equals "$(calls_made)" "2" "format sarif: no human run"
assert_equals "$(output_of report)" "$(output_of sarif)" "format sarif: report is the SARIF log"
assert_equals "$(output_of gates-failed)" "health-findings" "gates-failed output"

run_analyze INPUT_COMMAND=complexity INPUT_ARGS="--fail-on-issues --max-cyclomatic 10"
assert_equals "$(call_args 1)" "complexity --fail-on-issues --max-cyclomatic 10 --format json " "a gate flag in args is not given twice"
run_analyze INPUT_COMMAND=complexity INPUT_ARGS="--fail-on-complexity --max-cyclomatic 10"
assert_equals "$(call_args 1)" "complexity --fail-on-complexity --max-cyclomatic 10 --format json " "an older gate flag in args gives no second gate flag"
run_analyze INPUT_COMMAND=size INPUT_ARGS="--fail-on-clones"
assert_equals "$(call_args 1)" "size --fail-on-issues --fail-on-clones --format json " "the older flag of another command does not count"

MARKER="$WORK/pwned"
run_analyze INPUT_COMMAND=dupes INPUT_FORMAT=json INPUT_ARGS="--min-tokens 40 \$(touch $MARKER) ;touch $MARKER \`touch $MARKER\` *
--skip-local"
assert_equals "$(call_args 1)" "dupes --fail-on-issues --min-tokens 40 \$(touch $MARKER) ;touch $MARKER \`touch $MARKER\` * --skip-local --format json " "args stay literal, also after a line break"
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
  INPUT_BASE=auto PR_BASE_SHA=0123abc00123abc00123abc00123abc00123abc0
assert_equals "$(calls_made)" "1" "mutants: one run"
assert_equals "$(call_args 1)" "mutants --fail-on-issues --in-diff 0123abc00123abc00123abc00123abc00123abc0 --package my-crate --max-mutants 10 --time-budget 600 --jobs 2 --timeout 30 --format json " "mutants: the inputs become flags"
if diff <(jq -S . "$(output_of sarif)") <(jq -S . "$FIXTURES/mutants.sarif") > /dev/null; then
  pass "mutants: the SARIF log comes from the JSON report"
else
  fail "mutants: the SARIF log comes from the JSON report" "the logs differ"
fi
assert_equals "$(output_of mutation-score)" "66.7" "mutants: mutation-score output"
assert_contains "$OUT" "The mutants run has no second human run" "mutants: format human explains the log"

SHA_A=4567def04567def04567def04567def04567def0
SHA_B=0123abc00123abc00123abc00123abc00123abc0
run_analyze INPUT_COMMAND=mutants INPUT_BASE=auto STUB_JSON="$FIXTURES/mutants.json"
assert_equals "$(call_args 1)" "mutants --fail-on-issues --format json " "base auto with no pull request and no push gives no flag"
assert_contains "$OUT" "found no pull request base and no push" "base auto with no base explains it"
run_analyze INPUT_COMMAND=size INPUT_BASE=auto INPUT_FORMAT=json PUSH_BEFORE_SHA="$SHA_A"
assert_equals "$(call_args 1)" "size --fail-on-issues --base $SHA_A --format json " "base auto on a push takes the commit before the push"
run_analyze INPUT_COMMAND=mutants INPUT_BASE=auto STUB_JSON="$FIXTURES/mutants.json" PUSH_BEFORE_SHA="$SHA_A"
assert_equals "$(call_args 1)" "mutants --fail-on-issues --in-diff $SHA_A --format json " "base auto on a push gives mutants the commit before the push"
run_analyze INPUT_COMMAND=size INPUT_BASE=auto INPUT_FORMAT=json PR_BASE_SHA="$SHA_B" PUSH_BEFORE_SHA="$SHA_A"
assert_equals "$(call_args 1)" "size --fail-on-issues --base $SHA_B --format json " "base auto prefers the pull request base"
run_analyze INPUT_COMMAND=size INPUT_BASE=auto INPUT_FORMAT=json PUSH_BEFORE_SHA=0000000000000000000000000000000000000000
assert_equals "$(call_args 1)" "size --fail-on-issues --format json " "base auto on the push of a new branch gives no base"
assert_contains "$OUT" "a new branch or tag" "base auto on a new branch explains it"
run_analyze INPUT_COMMAND=size INPUT_BASE=auto INPUT_FORMAT=json PUSH_BEFORE_SHA="--upload-pack=x"
assert_equals "$CODE" "1" "base auto with a value that is not a commit SHA fails the size step"
assert_equals "$(calls_made)" "0" "base auto with a value that is not a commit SHA runs nothing"
assert_contains "$OUT" "::error::base: auto got '--upload-pack=x'" "base auto names the value that is not a commit SHA"
run_analyze INPUT_COMMAND=complexity INPUT_BASE=auto INPUT_FORMAT=json PUSH_BEFORE_SHA="--upload-pack=x"
assert_equals "$CODE" "0" "base auto with a bad value stays a notice for a command without a base gate"
assert_contains "$OUT" "::notice::base: auto got" "base auto with a bad value gives a notice for a command without a base gate"
run_analyze INPUT_COMMAND=check INPUT_BASE=auto INPUT_FORMAT=json PUSH_BEFORE_SHA="$SHA_A"
assert_equals "$(call_args 1)" "check --fail-on-issues --base $SHA_A --format json " "check: base auto becomes --base"
# A repository with a remote: the commit that the checkout lacks comes from
# the remote, a commit that no remote has gives no base.
GIT_ID=(-c user.name=Test -c user.email=test@example.com -c commit.gpgsign=false)
REMOTE="$WORK/remote-repo"
git init -q "$REMOTE"
git -C "$REMOTE" "${GIT_ID[@]}" commit -q --allow-empty -m one
REMOTE_SHA="$(git -C "$REMOTE" rev-parse HEAD)"
git -C "$REMOTE" config uploadpack.allowReachableSHA1InWant true
SHALLOW="$WORK/shallow-repo"
git init -q "$SHALLOW"
git -C "$SHALLOW" remote add origin "$REMOTE"
run_analyze INPUT_COMMAND=size INPUT_BASE=auto INPUT_FORMAT=json INPUT_WORKING_DIRECTORY="$SHALLOW" \
  PUSH_BEFORE_SHA="$REMOTE_SHA"
assert_equals "$(call_args 1)" "size --fail-on-issues --base $REMOTE_SHA --format json " "base auto fetches a commit that the checkout lacks"
run_analyze INPUT_COMMAND=size INPUT_BASE=auto INPUT_FORMAT=json INPUT_WORKING_DIRECTORY="$SHALLOW" \
  PUSH_BEFORE_SHA="$REMOTE_SHA"
assert_equals "$(call_args 1)" "size --fail-on-issues --base $REMOTE_SHA --format json " "base auto uses a commit that the checkout has"
run_analyze INPUT_COMMAND=size INPUT_BASE=auto INPUT_FORMAT=json INPUT_WORKING_DIRECTORY="$SHALLOW" \
  PUSH_BEFORE_SHA=89abcdef0123456789abcdef0123456789abcdef
assert_equals "$CODE" "1" "base auto with a commit that no remote has fails the size step"
assert_equals "$(calls_made)" "0" "base auto with a missing commit runs nothing"
assert_contains "$OUT" "::error::base: auto cannot read the commit" "base auto with a missing commit is an error"
assert_contains "$OUT" "fetch-depth: 0" "base auto with a missing commit names the checkout fix"
run_analyze INPUT_COMMAND=mutants INPUT_BASE=auto STUB_JSON="$FIXTURES/mutants.json" INPUT_WORKING_DIRECTORY="$SHALLOW" \
  PUSH_BEFORE_SHA=89abcdef0123456789abcdef0123456789abcdef
assert_equals "$CODE" "1" "base auto with a missing commit fails the mutants step"
assert_equals "$(calls_made)" "0" "base auto with a missing commit runs no mutant"
run_analyze INPUT_COMMAND=check INPUT_BASE=auto INPUT_FORMAT=json INPUT_WORKING_DIRECTORY="$SHALLOW" \
  INPUT_FAIL_ON_ISSUES=false PR_BASE_SHA=89abcdef0123456789abcdef0123456789abcdef
assert_equals "$CODE" "1" "base auto with a missing commit fails the check step, also with fail-on-issues false"
assert_contains "$OUT" "set base to a ref" "base auto with a missing commit names the force push fix"
run_analyze INPUT_COMMAND=check INPUT_BASE=auto INPUT_FORMAT=json
assert_equals "$CODE" "0" "check: base auto with no pull request and no push stays a notice"
assert_equals "$(call_args 1)" "check --fail-on-issues --format json " "check: base auto with no base gives no flag"
run_analyze INPUT_COMMAND=check INPUT_BASE=auto INPUT_FORMAT=json PUSH_BEFORE_SHA=0000000000000000000000000000000000000000
assert_equals "$CODE" "0" "check: base auto on a new branch stays a notice"
run_analyze INPUT_COMMAND=size INPUT_FORMAT=json INPUT_WORKING_DIRECTORY="$SHALLOW" \
  INPUT_AUTO_CHANGED_SINCE=true PR_BASE_SHA=89abcdef0123456789abcdef0123456789abcdef
assert_equals "$CODE" "0" "auto-changed-since with a missing commit stays a notice"
assert_contains "$OUT" "::notice::base: auto cannot read the commit" "auto-changed-since with a missing commit gives a notice"
run_analyze INPUT_COMMAND=size INPUT_BASE=auto INPUT_FORMAT=json INPUT_WORKING_DIRECTORY="$SHALLOW" \
  PR_BASE_SHA=89abcdef0123456789abcdef0123456789abcdef
assert_equals "$CODE" "1" "base auto checks the pull request base too"
run_analyze INPUT_COMMAND=size INPUT_BASE=origin/main INPUT_FORMAT=json
assert_equals "$(call_args 1)" "size --fail-on-issues --base origin/main --format json " "size: base becomes --base"
run_analyze INPUT_COMMAND=complexity INPUT_BASE=origin/main INPUT_PACKAGE=x INPUT_FORMAT=json
assert_equals "$(call_args 1)" "complexity --fail-on-issues --format json " "complexity: base and mutants inputs give no flags"
assert_contains "$OUT" "The 'base' input applies only to check, size, api and mutants" "complexity: base gives a warning"
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
    bash "${INSTALL_SH:-$SCRIPTS/install.sh}" 2>&1)"
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
assert_contains "$OUT" "DRY RUN: download https://github.com/fallow-rs/cargo-fallow/releases/download/v0.3.1/cargo-fallow-0.3.1-x86_64-unknown-linux-musl.tar.gz" "release: the asset of the action version"
assert_contains "$OUT" "check https://github.com/fallow-rs/cargo-fallow/releases/download/v0.3.1/cargo-fallow-0.3.1-x86_64-unknown-linux-musl.tar.gz.sig and" "release: the signature of the asset"
run_install INPUT_INSTALL=release ACTION_PATH="$RELEASED" INPUT_VERSION=v0.4.0 RUNNER_OS=Windows
assert_contains "$OUT" "/v0.4.0/cargo-fallow-0.4.0-x86_64-pc-windows-msvc.zip" "release: the version input wins, and Windows takes a zip"
run_install INPUT_INSTALL=release ACTION_PATH="$RELEASED" RUNNER_OS=Linux RUNNER_ARCH=ARM64
assert_contains "$OUT" "cargo-fallow-0.3.1-aarch64-unknown-linux-musl.tar.gz" "release: Linux ARM64"
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

# A real download from a file URL, with good and bad signatures and checksums.
# The names are those of .github/workflows/release.yml. A copy of the action
# scripts holds the public key of a test key pair instead of the release key.
sha256() {
  if command -v sha256sum > /dev/null 2>&1; then sha256sum "$1" | awk '{ print $1 }'; else shasum -a 256 "$1" | awk '{ print $1 }'; fi
}
NAME="cargo-fallow-0.3.1-x86_64-unknown-linux-musl"
ASSET="$NAME.tar.gz"
DIST="$WORK/dist/v0.3.1"
mkdir -p "$DIST" "$WORK/pack/$NAME"
pack() {
  printf '#!/bin/sh\necho "cargo-fallow %s"\n' "$1" > "$WORK/pack/$NAME/cargo-fallow"
  chmod +x "$WORK/pack/$NAME/cargo-fallow"
  (cd "$WORK/pack" && tar -czf "$DIST/$ASSET" "$NAME")
}
pack 0.3.1
write_checksum() {
  printf '%s  %s\n' "$(sha256 "$DIST/$ASSET")" "$ASSET" > "$DIST/$ASSET.sha256"
}
write_checksum
SIGN="$ACTION_DIR/../.github/scripts/sign-binary.mjs"
VERIFY="$SCRIPTS/verify-signature.mjs"
TEST_SEED="$(printf 'cargo-fallow action test key 32b' | base64)"
OTHER_SEED="$(printf 'cargo-fallow action other key 32' | base64)"
public_key_of() {
  node -e 'const c = require("node:crypto");
const seed = Buffer.from(process.argv[1], "base64");
const header = Buffer.from("302e020100300506032b657004220420", "hex");
const key = c.createPrivateKey({ key: Buffer.concat([header, seed]), format: "der", type: "pkcs8" });
const spki = c.createPublicKey(key).export({ format: "der", type: "spki" });
process.stdout.write(spki.subarray(-32).toString("base64"));' "$1"
}
sign_with() {
  ED25519_BINARY_SIGNING_PRIVATE_KEY="$1" node "$SIGN" "$DIST/$ASSET" "$DIST/$ASSET.sig" > /dev/null
}
RELEASE_KEY="AkUBBjL/jJEZYBAJvHfjqlo0O3PNUMorszc5+LOzziM="
download() {
  INSTALL_SH="$WORK/test-action/scripts/install.sh" run_install INPUT_INSTALL="$1" \
    ACTION_PATH="$RELEASED" CARGO_FALLOW_DRY_RUN=0 CARGO_FALLOW_RELEASE_URL="file://$WORK/dist"
}
if command -v curl > /dev/null 2>&1 && command -v node > /dev/null 2>&1; then
  TEST_PUBLIC_KEY="$(public_key_of "$TEST_SEED")"
  mkdir -p "$WORK/test-action"
  cp -R "$SCRIPTS" "$WORK/test-action/scripts"
  sed "s|$RELEASE_KEY|$TEST_PUBLIC_KEY|" "$VERIFY" > "$WORK/test-action/scripts/verify-signature.mjs"
  assert_not_contains "$(cat "$WORK/test-action/scripts/verify-signature.mjs")" "$RELEASE_KEY" "test copy: the test key replaces the release key"
  sign_with "$TEST_SEED"
  node "$VERIFY" "$DIST/$ASSET" "$DIST/$ASSET.sig" > /dev/null 2>&1
  assert_equals "$?" "1" "verify-signature: the release key rejects a signature of another key"
  CARGO_FALLOW_SIGNING_PUBLIC_KEY="$TEST_PUBLIC_KEY" node "$VERIFY" "$DIST/$ASSET" "$DIST/$ASSET.sig" > /dev/null 2>&1
  assert_equals "$?" "1" "verify-signature: no variable replaces the release key"
  node "$WORK/test-action/scripts/verify-signature.mjs" "$DIST/$ASSET" "$DIST/$ASSET.sig"
  assert_equals "$?" "0" "verify-signature: the signer and the verifier agree"
  download release
  assert_equals "$CODE" "0" "release download: a good signature and checksum install"
  assert_contains "$OUT" "cargo-fallow: cargo-fallow 0.3.1 (install: release)" "release download: the binary runs"

  sign_with "$OTHER_SEED"
  download auto
  assert_equals "$CODE" "1" "release download: a signature of another key fails, also for auto"
  assert_contains "$OUT" "The Ed25519 signature of $ASSET is not valid." "release download: the failure names the signature"
  assert_not_contains "$OUT" "Building cargo-fallow from the source" "release download: a bad signature does not fall back"

  sign_with "$TEST_SEED"
  printf 'changed' >> "$DIST/$ASSET"
  write_checksum
  download release
  assert_equals "$CODE" "1" "release download: a changed archive with a new checksum fails the signature"
  assert_contains "$OUT" "signature of $ASSET is not valid" "release download: the signature finds a replaced archive"
  pack 0.3.1
  write_checksum

  pack 0.2.0
  write_checksum
  sign_with "$TEST_SEED"
  download auto
  assert_equals "$CODE" "1" "release download: a signed archive of another version fails, also for auto"
  assert_contains "$OUT" "The binary in $ASSET has version 0.2.0, not 0.3.1." "release download: the failure names both versions"
  assert_not_contains "$OUT" "Building cargo-fallow from the source" "release download: another version does not fall back"
  pack 0.3.1
  write_checksum

  rm -f "$DIST/$ASSET.sig"
  download auto
  assert_equals "$CODE" "1" "release download: a missing signature fails, also for auto"
  assert_contains "$OUT" "The release has no signature $ASSET.sig" "release download: the failure names the missing signature"

  sign_with "$TEST_SEED"
  printf '%s  x\n' "0000000000000000000000000000000000000000000000000000000000000000" > "$DIST/$ASSET.sha256"
  download auto
  assert_equals "$CODE" "1" "release download: a bad checksum fails, also for auto"
  assert_contains "$OUT" "The SHA-256 of $ASSET is" "release download: the failure names the checksum"
  assert_not_contains "$OUT" "Building cargo-fallow from the source" "release download: a bad checksum does not fall back"
  write_checksum

  run_install INPUT_INSTALL=release ACTION_PATH="$RELEASED" CARGO_FALLOW_DRY_RUN=0 CARGO_FALLOW_RELEASE_URL="http://127.0.0.1:9/dist"
  assert_equals "$CODE" "1" "release download: a plain http URL is refused"
  assert_contains "$OUT" "No release asset at http://127.0.0.1:9/dist" "release download: the refused URL is named"
  assert_contains "$OUT" "Protocol" "release download: curl refuses the http protocol"
else
  echo "  skip release download: curl or node is missing"
fi
if ! PATH=/usr/bin:/bin command -v node > /dev/null 2>&1; then
  OUT="$(env -i PATH=/usr/bin:/bin HOME="$WORK" RUNNER_TEMP="$WORK" GITHUB_OUTPUT=/dev/null \
    GITHUB_PATH=/dev/null RUNNER_OS=Linux RUNNER_ARCH=X64 ACTION_PATH="$RELEASED" \
    INPUT_INSTALL=release CARGO_FALLOW_RELEASE_URL="file://$WORK/dist" \
    bash "$SCRIPTS/install.sh" 2>&1)"
  assert_equals "$?" "1" "release download: a runner without node fails closed"
  assert_contains "$OUT" "cannot check the signature" "release download: the failure names node"
else
  echo "  skip release download without node: /usr/bin has node"
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
# --- comment.sh ---------------------------------------------------------------

echo "comment.sh"
GH_DIR="$WORK/gh-stub"
mkdir -p "$GH_DIR"
# The gh stub logs each call and answers the list call with $GH_COMMENTS.
cat > "$GH_DIR/gh" << 'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$GH_LOG"
case "$*" in
  "api user --jq .login") printf '%s\n' "${GH_USER_OUTPUT:-}"; exit "${GH_USER_EXIT:-1}" ;;
  *"--paginate"*) cat "$GH_COMMENTS" ;;
  *"body=@"*)
    for arg in "$@"; do
      case "$arg" in body=@*) cp "${arg#body=@}" "$GH_BODY" ;; esac
    done
    echo '{}'
    ;;
esac
STUB
chmod +x "$GH_DIR/gh"
run_comment() {
  GH_LOG="$WORK/gh-log-$RANDOM$RANDOM"
  GH_BODY="$WORK/gh-body-$RANDOM$RANDOM"
  : > "$GH_LOG"
  OUT="$(env -i PATH="$GH_DIR:$BASE_PATH" GH_LOG="$GH_LOG" GH_BODY="$GH_BODY" \
    GH_USER_OUTPUT="${GH_USER_OUTPUT:-}" GH_USER_EXIT="${GH_USER_EXIT:-1}" \
    GH_COMMENTS="${GH_COMMENTS:-/dev/null}" RUNNER_TEMP="$WORK" \
    ACTION_JQ_DIR="$JQ_DIR" RESULTS_FILE="$FIXTURES/complexity.json" SARIF_FILE="" \
    FALLOW_COMMAND=complexity EXIT_CODE=0 MAX_FINDINGS=50 GH_REPO=o/r "$@" \
    bash "$SCRIPTS/comment.sh" 2>&1)"
  CODE=$?
}
printf '[]' > "$WORK/comments-none.json"
GH_COMMENTS="$WORK/comments-none.json" run_comment PR_NUMBER=7
assert_equals "$CODE" "0" "comment: a first run exits 0"
assert_contains "$(cat "$GH_LOG")" "-X POST repos/o/r/issues/7/comments" "comment: a first run posts a comment"
assert_contains "$(head -n 1 "$GH_BODY")" "<!-- cargo-fallow-comment:complexity -->" "comment: the body starts with the marker"
assert_contains "$(cat "$GH_BODY")" "complexity" "comment: the body holds the summary"
printf '[{"id": 11, "body": "other", "user": {"login": "someone"}}, {"id": 13, "body": "<!-- cargo-fallow-comment:complexity -->", "user": {"login": "someone"}}]\n[{"id": 42, "body": "<!-- cargo-fallow-comment:complexity -->\\nold", "user": {"login": "github-actions[bot]"}}]\n' > "$WORK/comments-one.json"
GH_COMMENTS="$WORK/comments-one.json" run_comment PR_NUMBER=7
assert_contains "$(cat "$GH_LOG")" "-X PATCH repos/o/r/issues/comments/42" "comment: a second run updates the comment"
assert_equals "$(grep -c -- '-X POST' "$GH_LOG")" "0" "comment: a second run posts no new comment"
GH_USER_OUTPUT='{"message":"Resource not accessible by integration"}' GH_COMMENTS="$WORK/comments-one.json" run_comment PR_NUMBER=7
assert_contains "$(cat "$GH_LOG")" "-X PATCH repos/o/r/issues/comments/42" "comment: a failed user lookup with an error body still finds the comment"
GH_USER_OUTPUT=null GH_USER_EXIT=0 GH_COMMENTS="$WORK/comments-one.json" run_comment PR_NUMBER=7
assert_contains "$(cat "$GH_LOG")" "-X PATCH repos/o/r/issues/comments/42" "comment: a null login still finds the comment"
GH_USER_OUTPUT=someone GH_USER_EXIT=0 GH_COMMENTS="$WORK/comments-one.json" run_comment PR_NUMBER=7
assert_contains "$(cat "$GH_LOG")" "-X PATCH repos/o/r/issues/comments/13" "comment: a personal token finds the comment of its user"
GH_COMMENTS="$WORK/comments-one.json" run_comment PR_NUMBER=7 COMMENT_KEY=other-key
assert_contains "$(cat "$GH_LOG")" "-X POST" "comment: another key posts its own comment"
run_comment PR_NUMBER=7 IS_FORK=true
assert_equals "$CODE" "0" "comment: a fork pull request passes"
assert_contains "$OUT" "pull request from a fork" "comment: a fork pull request explains the skip"
assert_equals "$(cat "$GH_LOG")" "" "comment: a fork pull request calls no API"
run_comment PR_NUMBER=""
assert_contains "$OUT" "has no pull request" "comment: a push posts no comment"
run_comment PR_NUMBER="7; id"
assert_equals "$CODE" "1" "comment: a bad number fails"
run_comment PR_NUMBER=7 COMMENT_KEY='x --> y'
assert_equals "$CODE" "1" "comment: a key that can close the marker fails"
run_comment PR_NUMBER=7 RESULTS_FILE=/nonexistent
assert_contains "$OUT" "posts no comment" "comment: a run with no summary posts no comment"
assert_equals "$(cat "$GH_LOG")" "" "comment: a run with no summary calls no API"
printf '#!/usr/bin/env bash\nexit 1\n' > "$WORK/gh-fail"
mkdir -p "$WORK/gh-fail-dir" && cp "$WORK/gh-fail" "$WORK/gh-fail-dir/gh" && chmod +x "$WORK/gh-fail-dir/gh"
OUT="$(env -i PATH="$WORK/gh-fail-dir:$BASE_PATH" RUNNER_TEMP="$WORK" ACTION_JQ_DIR="$JQ_DIR" \
  RESULTS_FILE="$FIXTURES/complexity.json" FALLOW_COMMAND=complexity EXIT_CODE=0 MAX_FINDINGS=50 \
  GH_REPO=o/r PR_NUMBER=7 bash "$SCRIPTS/comment.sh" 2>&1)"
assert_equals "$?" "0" "comment: an API error does not fail the step"
assert_contains "$OUT" "pull-requests: write" "comment: an API error names the permission"

# --- the scope inputs of analyze.sh -------------------------------------------

echo "analyze.sh scope inputs"
run_analyze INPUT_COMMAND=check INPUT_FORMAT=json INPUT_CHANGED_SINCE=origin/main INPUT_BASELINE=b.json
assert_equals "$(call_args 1)" "check --fail-on-issues --changed-since origin/main --baseline b.json --format json " "scope: changed-since and baseline become flags"
run_analyze INPUT_COMMAND=deps INPUT_FORMAT=json INPUT_SAVE_BASELINE=b.json
assert_equals "$(call_args 1)" "deps --fail-on-issues --save-baseline b.json --format json " "scope: save-baseline becomes a flag"
run_analyze INPUT_COMMAND=check INPUT_FORMAT=json INPUT_AUTO_CHANGED_SINCE=true PR_BASE_SHA=0123abc00123abc00123abc00123abc00123abc0
assert_equals "$(call_args 1)" "check --fail-on-issues --changed-since 0123abc00123abc00123abc00123abc00123abc0 --format json " "scope: auto-changed-since takes the pull request base"
run_analyze INPUT_COMMAND=check INPUT_FORMAT=json INPUT_AUTO_CHANGED_SINCE=true
assert_equals "$(call_args 1)" "check --fail-on-issues --format json " "scope: auto-changed-since without a pull request gives no flag"
assert_contains "$OUT" "auto-changed-since applies to pull requests" "scope: auto-changed-since without a pull request explains it"
run_analyze INPUT_COMMAND=check INPUT_FORMAT=json INPUT_AUTO_CHANGED_SINCE=yes
assert_contains "$OUT" "Invalid 'auto-changed-since' input" "scope: a bad boolean stops the step"
run_analyze INPUT_COMMAND=mutants STUB_JSON="$FIXTURES/mutants.json" INPUT_CHANGED_SINCE=origin/main
assert_equals "$(call_args 1)" "mutants --fail-on-issues --format json " "scope: mutants ignores changed-since"
assert_contains "$OUT" "The mutants run ignores them" "scope: mutants explains the ignored input"

echo "${PASSED} passed, ${FAILED} failed"
if [ "$FAILED" -gt 0 ]; then
  printf '  %s\n' "${FAILURES[@]}"
  exit 1
fi
