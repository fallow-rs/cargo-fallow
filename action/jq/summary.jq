# The job summary in Markdown for one cargo-fallow run.
#
# Input: the JSON report or the JSON error document. Arguments: $command (the
# command that ran), $exit_code, $max (the most findings to list), $prefix
# (see annotations.jq) and $sarif (the SARIF log in an array, or an empty
# array).

def cell: tostring | gsub("[\r\n]+"; " ") | gsub("\\|"; "\\|");
def number: if type == "number" and . == floor then floor else . end;
def yes_no: if . then "yes" else "no" end;
def plural($n; $word): "\($n) \($word)\(if $n == 1 then "" else "s" end)";
def percent: . * 10 | round / 10 | tostring + "%";

def gate_rows:
  (.gate_outcomes // {}) | to_entries | sort_by(.key)[]
  | "| `\(.key)` | \(.value.status) | \(.value.enforced | yes_no) | \(.value.observed // "" | number | cell) |";

def mutants_line:
  .summary as $s
  | "Mutation score \($s.score | percent): \($s.killed) killed, \($s.survived) survived, \($s.no_coverage) no coverage, \($s.timeout) timeout, \($s.pending) pending.";

def findings($results):
  ($results | length) as $total
  | ($results[0:($max | tonumber)]) as $shown
  | if $total == 0 then empty
    else
      "",
      "#### Findings\(if $total > ($shown | length) then " (\($shown | length) of \($total))" else "" end)",
      "",
      "| Location | Rule | Message |",
      "| --- | --- | --- |",
      ($shown[]
        | (.locations[0].physicalLocation // {}) as $where
        | "| `\("\($prefix)\($where.artifactLocation.uri // "")\(if $where.region.startLine then ":\($where.region.startLine)" else "" end)" | cell)` | `\(.ruleId // "" | cell)` | \(.message.text // "" | cell) |")
    end;

if .error == true then
  "### cargo fallow \($command)",
  "",
  "The run failed with exit code \($exit_code): \(.message | cell)",
  "",
  "- Code: `\(.code // "" | cell)`",
  (if .help then "- Help: \(.help | cell)" else empty end)
else
  "### cargo fallow \(.kind // $command)",
  "",
  "\(plural(.total_issues // 0; "issue")), exit code \($exit_code).",
  (if .kind == "mutants" and .summary.score != null then "", mutants_line else empty end),
  "",
  "| Gate | Status | Enforced | Observed |",
  "| --- | --- | --- | --- |",
  gate_rows,
  findings([$sarif[0].runs[]?.results[]?])
end
