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
#   COMMENT_KEY (default: the command), RUNNER_TEMP

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

MARKER="<!-- cargo-fallow-comment:${COMMENT_KEY} -->"
BODY="$(mktemp "${RUNNER_TEMP}/cargo-fallow-comment.XXXXXX")"
trap 'rm -f "$BODY"' EXIT
printf '%s\n\n' "$MARKER" > "$BODY"
GITHUB_STEP_SUMMARY="$BODY" bash "$(dirname "$0")/summary.sh" > /dev/null

existing="$(gh api --paginate "repos/${GH_REPO}/issues/${PR_NUMBER}/comments" \
  | jq -r --arg marker "$MARKER" '.[] | select(.body | startswith($marker)) | .id' | head -n 1)"
if [ -n "$existing" ]; then
  gh api -X PATCH "repos/${GH_REPO}/issues/comments/${existing}" -F "body=@${BODY}" > /dev/null
  echo "Updated the pull request comment ${existing}."
else
  gh api -X POST "repos/${GH_REPO}/issues/${PR_NUMBER}/comments" -F "body=@${BODY}" > /dev/null
  echo "Posted the pull request comment."
fi
