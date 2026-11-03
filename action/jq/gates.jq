# Step outputs from a cargo-fallow JSON report or a JSON error document.
#
# Output: one `name=value` line for each output. Each value is one line, so
# the lines are safe for $GITHUB_OUTPUT.

def names($status):
  [(.gate_outcomes // {}) | to_entries[] | select(.value.status == $status) | .key]
  | sort
  | join(",");

def one_line: tostring | gsub("[\r\n]"; " ");

if .error == true then
  "error-code=\(.code // "" | one_line)",
  "issues=",
  "gates-failed=",
  "gates-warned=",
  "gates-passed=",
  "gates-skipped=",
  "mutation-score="
else
  "error-code=",
  "issues=\(.total_issues // "" | one_line)",
  "gates-failed=\(names("fail"))",
  "gates-warned=\(names("warn"))",
  "gates-passed=\(names("pass"))",
  "gates-skipped=\(names("skipped"))",
  "mutation-score=\(if .kind == "mutants" and .summary.score != null then (.summary.score * 10 | round / 10 | tostring) else "" end)"
end
