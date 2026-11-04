#!/usr/bin/env bash
# Tests for the scripts of the GitLab CI template.
# Run: bash ci/tests/run.sh
#
# A stub binary stands in for cargo-fallow and a stub curl stands in for the
# GitLab API, so the tests need only bash and jq.

set -uo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
SCRIPTS="$(cd "$DIR/../scripts" && pwd)"
PASSED=0
FAILED=0
FAILURES=()

WORK="$(mktemp -d "${TMPDIR:-/tmp}/cargo-fallow-gitlab-test.XXXXXX")"
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

# --- The stubs ---------------------------------------------------------------
#
# The cargo-fallow stub writes each argument list to $STUB_LOG on one line.
# It prints a document for the format of the call and exits with $STUB_EXIT.
# The curl stub writes each call to $CURL_LOG, prints $CURL_NOTES for a GET
# and exits with $CURL_EXIT.

BIN="$WORK/bin"
mkdir -p "$BIN"
cat > "$BIN/cargo-fallow" << 'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$STUB_LOG"
format=human
previous=
for arg in "$@"; do
  [ "$previous" = "--format" ] && format="$arg"
  previous="$arg"
done
case "$format" in
  json) printf '{"kind":"check","total_issues":2}\n' ;;
  codeclimate) printf '[{"check_name":"deps/unused-dependency"}]\n' ;;
  markdown) printf '## cargo fallow check\n\n2 findings\n' ;;
  *) printf 'cargo fallow check\n' ;;
esac
# STUB_EXIT_MARKDOWN overrides STUB_EXIT for the markdown run.
if [ "$format" = "markdown" ] && [ -n "${STUB_EXIT_MARKDOWN:-}" ]; then exit "$STUB_EXIT_MARKDOWN"; fi
exit "${STUB_EXIT:-0}"
EOF
cat > "$BIN/curl" << 'EOF'
#!/usr/bin/env bash
method=GET
data=
previous=
for arg in "$@"; do
  case "$previous" in
    -X | --request) method="$arg" ;;
    --data-binary) data="$arg" ;;
  esac
  previous="$arg"
done
url="${*: -1}"
printf '%s %s\n' "$method" "$url" >> "$CURL_LOG"
if [ -n "$data" ]; then
  if [ "${data#@}" != "$data" ]; then cat "${data#@}" >> "$CURL_LOG"; else printf '%s\n' "$data" >> "$CURL_LOG"; fi
  printf '\n' >> "$CURL_LOG"
fi
[ "${CURL_EXIT:-0}" = "0" ] || exit "$CURL_EXIT"
if [ "$method" = "GET" ]; then
  case "$url" in
    *page=1) printf '%s' "${CURL_NOTES:-[]}" ;;
    *page=2) printf '%s' "${CURL_NOTES_2:-[]}" ;;
    *) printf '[]' ;;
  esac
else
  printf '{}'
fi
EOF
chmod +x "$BIN/cargo-fallow" "$BIN/curl"
BASE_PATH="$BIN:/usr/bin:/bin"
ln -s "$(command -v jq)" "$BIN/jq"

reset() {
  STUB_LOG="$WORK/stub.log"
  CURL_LOG="$WORK/curl.log"
  : > "$STUB_LOG"
  : > "$CURL_LOG"
  OUT="$WORK/out"
  rm -rf "$OUT"
  mkdir -p "$OUT"
  export STUB_LOG CURL_LOG
}

# Runs run.sh in $OUT with a clean environment and the given variables.
run_job() {
  (cd "$OUT" && env -i PATH="$BASE_PATH" HOME="$WORK" STUB_LOG="$STUB_LOG" \
    CURL_LOG="$CURL_LOG" "$@" bash "$SCRIPTS/run.sh") > "$WORK/job.log" 2>&1
}

# Runs comment.sh in $OUT with a clean environment and the given variables.
run_comment() {
  (cd "$OUT" && env -i PATH="$BASE_PATH" HOME="$WORK" CURL_LOG="$CURL_LOG" \
    CI_API_V4_URL="https://gitlab.example/api/v4" CI_PROJECT_ID=7 \
    CI_MERGE_REQUEST_IID=12 REPORT_FILE="$OUT/report.md" "$@" \
    bash "$SCRIPTS/comment.sh") > "$WORK/comment.log" 2>&1
}

echo "run.sh"

reset
run_job STUB_EXIT=0
code=$?
assert_equals "$code" "0" "a clean run exits 0"
assert_contains "$(cat "$STUB_LOG")" "check --format codeclimate --quiet" "the run writes the code quality report"
assert_contains "$(cat "$STUB_LOG")" "check --format json --quiet --fail-on-issues" "the JSON run has the gate flag"
assert_contains "$(cat "$OUT/gl-code-quality-report.json")" "unused-dependency" "the code quality file holds the report"
assert_contains "$(cat "$OUT/cargo-fallow-results.json")" '"total_issues":2' "the results file holds the JSON report"
assert_not_contains "$(cat "$STUB_LOG")" "--changed-since" "a run outside a merge request has no changed-since"
assert_not_contains "$(cat "$STUB_LOG")" "--format markdown" "a run without the comment has no markdown run"

reset
run_job STUB_EXIT=1
assert_equals "$?" "1" "a failed gate fails the job"
assert_contains "$(cat "$WORK/job.log")" "gate" "the log names the failed gate"

reset
run_job STUB_EXIT=1 CARGO_FALLOW_FAIL_ON_ISSUES=false
assert_equals "$?" "0" "without the gate a finding does not fail the job"
assert_not_contains "$(cat "$STUB_LOG")" "--fail-on-issues" "without the gate the run has no gate flag"

reset
run_job STUB_EXIT=2
assert_equals "$?" "2" "an execution error fails the job with exit 2"

reset
run_job CI_MERGE_REQUEST_IID=12 CI_MERGE_REQUEST_DIFF_BASE_SHA=abc123
assert_contains "$(cat "$STUB_LOG")" "--changed-since abc123" "a merge request run scopes the findings to the changed files"

assert_contains "$(grep -- '--format codeclimate' "$STUB_LOG")" "check --format codeclimate --quiet" "the code quality run runs"
assert_not_contains "$(grep -- '--format codeclimate' "$STUB_LOG")" "--changed-since" "the code quality report has the full scope, so the widget does not show findings as fixed"

reset
run_job CARGO_FALLOW_ARGS="--offline
--package app"
assert_contains "$(cat "$STUB_LOG")" "--offline --package app" "arguments on more lines are all passed"

reset
run_job CI_MERGE_REQUEST_IID=12 CI_MERGE_REQUEST_DIFF_BASE_SHA=abc123 CARGO_FALLOW_AUTO_CHANGED_SINCE=false
assert_not_contains "$(cat "$STUB_LOG")" "--changed-since" "auto changed-since can be off"

reset
run_job CARGO_FALLOW_CHANGED_SINCE=origin/main
assert_contains "$(cat "$STUB_LOG")" "--changed-since origin/main" "an explicit changed-since is passed"

reset
run_job CARGO_FALLOW_COMMAND=deps CARGO_FALLOW_ARGS="--offline --package app"
assert_contains "$(cat "$STUB_LOG")" "deps --format json --quiet --fail-on-issues --offline --package app" "the command and the extra arguments are passed"

reset
run_job CARGO_FALLOW_COMMAND=mutants
assert_equals "$?" "1" "an unknown command is an input error"
assert_contains "$(cat "$WORK/job.log")" "CARGO_FALLOW_COMMAND" "the error names the variable"
assert_equals "$(cat "$STUB_LOG")" "" "an input error runs nothing"

reset
run_job CARGO_FALLOW_FAIL_ON_ISSUES=yes
assert_equals "$?" "1" "an invalid boolean is an input error"

reset
run_job CARGO_FALLOW_COMMENT=true CI_MERGE_REQUEST_IID=12 CI_MERGE_REQUEST_DIFF_BASE_SHA=abc123 \
  CI_API_V4_URL="https://gitlab.example/api/v4" CI_PROJECT_ID=7 GITLAB_TOKEN=secret
assert_equals "$?" "0" "a run with the comment exits with the verdict"
assert_contains "$(cat "$STUB_LOG")" "--format markdown" "the comment run writes markdown"
assert_contains "$(cat "$CURL_LOG")" "POST https://gitlab.example/api/v4/projects/7/merge_requests/12/notes" "the run posts the note"

reset
run_job CARGO_FALLOW_COMMENT=true CI_MERGE_REQUEST_IID=12 STUB_EXIT_MARKDOWN=2 \
  CI_API_V4_URL="https://gitlab.example/api/v4" CI_PROJECT_ID=7 GITLAB_TOKEN=secret
assert_equals "$?" "0" "a failed markdown run does not fail the job"
assert_equals "$(cat "$CURL_LOG")" "" "a failed markdown run keeps the old note"
assert_contains "$(cat "$WORK/job.log")" "does not change the merge request note" "the log says that the note stays"

reset
run_job CARGO_FALLOW_COMMENT=true
assert_equals "$?" "0" "the comment outside a merge request does not fail the job"
assert_contains "$(cat "$WORK/job.log")" "merge request" "the log says that the comment needs a merge request"
assert_equals "$(cat "$CURL_LOG")" "" "no API call outside a merge request"

echo "comment.sh"

reset
printf '## cargo fallow check\n' > "$OUT/report.md"
run_comment
assert_equals "$?" "0" "a missing token does not fail the job"
assert_contains "$(cat "$WORK/comment.log")" "GITLAB_TOKEN" "the log names the token"
assert_equals "$(cat "$CURL_LOG")" "" "no API call without a token"

reset
printf '## cargo fallow check\n' > "$OUT/report.md"
run_comment GITLAB_TOKEN=secret
log="$(cat "$CURL_LOG")"
assert_contains "$log" "GET https://gitlab.example/api/v4/projects/7/merge_requests/12/notes?per_page=100&page=1" "the script reads the notes"
assert_contains "$log" "POST https://gitlab.example/api/v4/projects/7/merge_requests/12/notes" "a new note is posted"
assert_contains "$log" '<!-- cargo-fallow-comment:check -->' "the note has the marker"
assert_contains "$log" '## cargo fallow check' "the note has the report"

reset
printf '## cargo fallow check\n' > "$OUT/report.md"
run_comment GITLAB_TOKEN=secret CURL_NOTES='[{"id":3,"body":"other"},{"id":5,"body":"<!-- cargo-fallow-comment:check -->\nold"}]'
log="$(cat "$CURL_LOG")"
assert_contains "$log" "PUT https://gitlab.example/api/v4/projects/7/merge_requests/12/notes/5" "the note with the marker is updated"
assert_not_contains "$log" "POST" "no second note is posted"

reset
printf '## cargo fallow check\n' > "$OUT/report.md"
page_one="[$(for id in $(seq 1 100); do printf '{"id":%s,"body":"other"},' "$id"; done | sed 's/,$//')]"
run_comment GITLAB_TOKEN=secret CURL_NOTES="$page_one" CURL_NOTES_2='[{"id":205,"body":"<!-- cargo-fallow-comment:check -->\nold"}]'
log="$(cat "$CURL_LOG")"
assert_contains "$log" "page=2" "a full page of notes reads the next page"
assert_contains "$log" "PUT https://gitlab.example/api/v4/projects/7/merge_requests/12/notes/205" "the note on the second page is updated"
assert_not_contains "$log" "POST" "no second note when the marker is on the second page"

reset
printf '## cargo fallow deps\n' > "$OUT/report.md"
run_comment GITLAB_TOKEN=secret COMMENT_KEY=deps CURL_NOTES='[{"id":5,"body":"<!-- cargo-fallow-comment:check -->\nold"}]'
log="$(cat "$CURL_LOG")"
assert_contains "$log" "POST" "the note of another key stays"
assert_contains "$log" '<!-- cargo-fallow-comment:deps -->' "the key names the marker"

reset
printf '## cargo fallow check\n' > "$OUT/report.md"
run_comment GITLAB_TOKEN=secret CURL_EXIT=22
assert_equals "$?" "0" "an API error does not fail the job"
assert_contains "$(cat "$WORK/comment.log")" "WARNING" "an API error gives a warning"

reset
printf '## cargo fallow check\n' > "$OUT/report.md"
run_comment GITLAB_TOKEN=secret COMMENT_KEY='bad key'
assert_equals "$?" "1" "an invalid comment key is an input error"

reset
head -c 1100000 /dev/zero | tr '\0' 'x' > "$OUT/report.md"
run_comment GITLAB_TOKEN=secret
log="$(cat "$CURL_LOG")"
assert_contains "$log" "The note is cut" "a large report is cut"
posted="$(sed -n '/^POST /,$p' "$CURL_LOG" | sed 1d)"
if printf '%s' "$posted" | jq -e '.body | length < 1000000' > /dev/null 2>&1; then
  pass "the cut note is valid JSON under the limit of GitLab"
else
  fail "the cut note" "not valid JSON under 1000000 characters"
fi

echo "gitlab-ci.yml"

TEMPLATE="$DIR/../gitlab-ci.yml"
read -r -a TEMPLATE_SCRIPTS <<< "$(sed -n 's/^ *for script in \(.*\); do$/\1/p' "$TEMPLATE")"
for script in "${TEMPLATE_SCRIPTS[@]}"; do
  if [ -f "$SCRIPTS/$script" ]; then pass "the template script $script exists"; else fail "template" "no ci/scripts/$script"; fi
done
assert_contains "$(cat "$TEMPLATE")" "codequality: gl-code-quality-report.json" "the widget reads the file that run.sh writes"
assert_contains "$(cat "$SCRIPTS/run.sh")" "CARGO_FALLOW_CODE_QUALITY_FILE:=gl-code-quality-report.json" "run.sh writes the file of the template"
assert_not_contains "$(cat "$TEMPLATE")" "$(printf '\t')" "the template has no tab"
assert_contains "$(cat "$TEMPLATE")" 'CARGO_FALLOW_REF: ""' "the template downloads no script of an unpinned ref by default"

echo
echo "${PASSED} passed, ${FAILED} failed"
if [ "$FAILED" -gt 0 ]; then
  printf '  %s\n' "${FAILURES[@]}"
  exit 1
fi
