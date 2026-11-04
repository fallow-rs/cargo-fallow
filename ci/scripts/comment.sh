#!/usr/bin/env bash
set -euo pipefail

# Post or update one merge request note with the markdown report of the run.
# A hidden marker names the note of each key, so a second run updates the
# note and does not add a new one.
#
# Required env: REPORT_FILE, CI_API_V4_URL, CI_PROJECT_ID,
#   CI_MERGE_REQUEST_IID
# Optional env: GITLAB_TOKEN (a project or personal access token with the
#   `api` scope; CI_JOB_TOKEN cannot write notes), COMMENT_KEY (default:
#   check)

: "${COMMENT_KEY:=check}" "${GITLAB_TOKEN:=}"

if ! [[ "$COMMENT_KEY" =~ ^[A-Za-z0-9._-]+$ ]]; then
  echo "ERROR: Invalid comment key: ${COMMENT_KEY}. Use letters, digits, '.', '_' and '-'."
  exit 1
fi
if [ -z "$GITLAB_TOKEN" ]; then
  echo "WARNING: The merge request note needs GITLAB_TOKEN with the api scope. CI_JOB_TOKEN cannot write notes. The job posts no note."
  exit 0
fi
: "${REPORT_FILE:?REPORT_FILE is required}"
: "${CI_API_V4_URL:?CI_API_V4_URL is required}"
: "${CI_PROJECT_ID:?CI_PROJECT_ID is required}"
: "${CI_MERGE_REQUEST_IID:?CI_MERGE_REQUEST_IID is required}"

# The largest note body of GitLab is 1000000 characters.
MAX_BODY=900000
MARKER="<!-- cargo-fallow-comment:${COMMENT_KEY} -->"
NOTES="${CI_API_V4_URL}/projects/${CI_PROJECT_ID}/merge_requests/${CI_MERGE_REQUEST_IID}/notes"

BODY="$(mktemp "${TMPDIR:-/tmp}/cargo-fallow-note.XXXXXX")"
PAYLOAD="$(mktemp "${TMPDIR:-/tmp}/cargo-fallow-payload.XXXXXX")"
trap 'rm -f "$BODY" "$PAYLOAD"' EXIT
printf '%s\n\n' "$MARKER" > "$BODY"
if [ "$(wc -c < "$REPORT_FILE")" -gt "$MAX_BODY" ]; then
  head -c "$MAX_BODY" "$REPORT_FILE" >> "$BODY"
  printf '\n\n_The note is cut. The job artifacts have the full report._\n' >> "$BODY"
else
  cat "$REPORT_FILE" >> "$BODY"
fi
jq -n --rawfile body "$BODY" '{body: $body}' > "$PAYLOAD"

# An API error gives a warning: the gates of the run decide the job, not the
# note.
api_warning() {
  echo "WARNING: The job cannot ${1} the merge request note. Check that GITLAB_TOKEN has the api scope and access to the project."
  exit 0
}

# One API call with a time limit. Only a read retries: a retry of a write that
# timed out can add a second note.
api() {
  curl --silent --show-error --fail --max-time 30 --header "PRIVATE-TOKEN: ${GITLAB_TOKEN}" \
    --header "Content-Type: application/json" "$@"
}

# The user of the token. Only a note of this user with the marker is the note
# of the job: another user can write the marker in a note of their own.
me="$(api --retry 3 "${CI_API_V4_URL}/user" | jq -r '.id // empty')" || api_warning read
[ -n "$me" ] || api_warning read

existing=""
page=1
while [ -z "$existing" ]; do
  notes="$(api --retry 3 "${NOTES}?per_page=100&page=${page}")" || api_warning read
  existing="$(printf '%s' "$notes" | jq -r --arg marker "$MARKER" --argjson me "$me" \
    '[.[] | select((.system | not) and .author.id == $me and ((.body // "") | startswith($marker))) | .id] | first // empty')" || api_warning read
  [ "$(printf '%s' "$notes" | jq 'length')" -lt 100 ] && break
  page=$((page + 1))
done

if [ -n "$existing" ]; then
  api -X PUT --data-binary "@${PAYLOAD}" "${NOTES}/${existing}" > /dev/null || api_warning update
  echo "Updated the merge request note ${existing}."
else
  api -X POST --data-binary "@${PAYLOAD}" "$NOTES" > /dev/null || api_warning post
  echo "Posted the merge request note."
fi
