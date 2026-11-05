#!/usr/bin/env bash
set -euo pipefail

# Post or update one sticky pull request comment with the job summary of the
# run. A hidden marker names the comment of each command, so a second run
# updates the comment and does not add a new one.
#
# Required env: ACTION_JQ_DIR, RESULTS_FILE, FALLOW_COMMAND, EXIT_CODE,
#   MAX_FINDINGS, GH_REPO (owner/repo)
# Optional env: GH_TOKEN (gh reads it), SARIF_FILE, PR_NUMBER (empty when the
#   event has no pull request), IS_FORK (true for a pull request from a fork),
#   COMMENT_KEY (default: the command), COMMENT_AUTHOR (default: the login of
#   the token, else github-actions[bot]), RUNNER_TEMP

: "${PR_NUMBER:=}" "${IS_FORK:=false}" "${SARIF_FILE:=}" "${COMMENT_KEY:=$FALLOW_COMMAND}"
: "${RUNNER_TEMP:=${TMPDIR:-/tmp}}"

if [ -z "$PR_NUMBER" ]; then
  echo "::notice::The comment input applies to pull requests. The ${FALLOW_COMMAND} run has no pull request, so it posts no comment."
  exit 0
fi
if [ "$IS_FORK" = "true" ]; then
  echo "::notice::The action did not post the comment: a pull request from a fork has a read-only token."
  exit 0
fi
if ! [[ "$PR_NUMBER" =~ ^[0-9]+$ ]]; then
  echo "::error::The pull request number '${PR_NUMBER}' is not a number."
  exit 1
fi

if ! [[ "$COMMENT_KEY" =~ ^[A-Za-z0-9._-]+$ ]]; then
  echo "::error::Invalid 'comment-key' input: ${COMMENT_KEY}. Use letters, digits, '.', '_' and '-'."
  exit 1
fi

# The largest body that the comment API takes is 65536 characters.
MAX_BODY=60000

MARKER="<!-- cargo-fallow-comment:${COMMENT_KEY} -->"
BODY="$(mktemp "${RUNNER_TEMP}/cargo-fallow-comment.XXXXXX")"
SUMMARY="$(mktemp "${RUNNER_TEMP}/cargo-fallow-summary.XXXXXX")"
trap 'rm -f "$BODY" "$SUMMARY"' EXIT
GITHUB_STEP_SUMMARY="$SUMMARY" bash "$(dirname "$0")/summary.sh" > /dev/null
if [ ! -s "$SUMMARY" ]; then
  echo "::notice::The run has no summary, so the action posts no comment."
  exit 0
fi
printf '%s\n\n' "$MARKER" > "$BODY"
if [ "$(wc -c < "$SUMMARY")" -gt "$MAX_BODY" ]; then
  head -c "$MAX_BODY" "$SUMMARY" >> "$BODY"
  printf '\n\n_The comment is cut. The job summary has the full report._\n' >> "$BODY"
else
  cat "$SUMMARY" >> "$BODY"
fi

# The author of the comment: the login of the token. The default token cannot
# read its own user, and its comments have the author github-actions[bot]. A
# comment of another author with the marker is not the comment of the action.
# A failed lookup can print an error body or `null`, so only a value in the
# form of a login counts.
if [ -z "${COMMENT_AUTHOR:-}" ]; then
  COMMENT_AUTHOR="$(gh api user --jq .login 2> /dev/null || true)"
  if ! [[ "$COMMENT_AUTHOR" =~ ^[A-Za-z0-9][A-Za-z0-9-]*(\[bot\])?$ ]] || [ "$COMMENT_AUTHOR" = "null" ]; then
    COMMENT_AUTHOR="github-actions[bot]"
  fi
fi

echo "The comment author is ${COMMENT_AUTHOR}."

# An API error, for example a read-only token, gives a warning: the gates of
# the run decide the job, not the comment.
api_warning() {
  echo "::warning::The action cannot ${1} the pull request comment. The token needs 'pull-requests: write'."
  exit 0
}
comments="$(gh api --paginate "repos/${GH_REPO}/issues/${PR_NUMBER}/comments")" || api_warning read
# One line `<id> <same>` for the comment of the action, or nothing. `same` is
# true when the body of the comment is the new body. The API can give other
# line ends and no last newline, so the compare ignores them. jq on Windows
# ends each line with CRLF, so `tr` removes the CR.
match="$(printf '%s' "$comments" | jq -r --arg marker "$MARKER" --arg author "$COMMENT_AUTHOR" \
  --rawfile body "$BODY" \
  'def norm: gsub("\r\n"; "\n") | sub("\\s+$"; "");
   .[] | select(.user.login == $author and ((.body // "") | startswith($marker)))
   | "\(.id) \((.body | norm) == ($body | norm))"' | tr -d '\r' | head -n 1)"
existing="${match%% *}"

# A clean run has no issue, exit code 0 and no gate that fails or warns. Like
# fallow, a clean run posts no first comment, because that comment only sends
# a notification. A clean run still updates an old comment, so the comment
# does not keep the findings of an earlier run.
clean="$(jq -r --arg exit_code "$EXIT_CODE" \
  '$exit_code == "0" and .error != true and (.total_issues // 0) == 0
   and ([(.gate_outcomes // {})[] | select(.status == "fail" or .status == "warn")] | length) == 0' \
  "$RESULTS_FILE" 2> /dev/null | tr -d '\r' || echo false)"

if [ -z "$existing" ] && [ "$clean" = "true" ]; then
  echo "::notice::The run is clean and the pull request has no comment of the action, so the action posts no comment."
  exit 0
fi
if [ "${match#* }" = "true" ]; then
  echo "The pull request comment ${existing} is unchanged, so the action does not update it."
  exit 0
fi
if [ -n "$existing" ]; then
  gh api -X PATCH "repos/${GH_REPO}/issues/comments/${existing}" -F "body=@${BODY}" > /dev/null \
    || api_warning update
  echo "Updated the pull request comment ${existing}."
else
  gh api -X POST "repos/${GH_REPO}/issues/${PR_NUMBER}/comments" -F "body=@${BODY}" > /dev/null \
    || api_warning post
  echo "Posted the pull request comment."
fi
