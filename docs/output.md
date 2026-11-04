# Output formats

This page describes `check`. [Output](mutation-testing.md#output) describes `mutants`.

- `human`: the score, then the text of each check, then one gate block, on
  stdout. Progress lines go to stderr.
- `json`: the report on stdout. The report has `schema_version` (1),
  `version`, `kind`, `language`, `workspace`, `checks`, `total_issues`,
  `health`, `findings`, `gate_outcomes` and `sections`. `health` has no
  `score` and no `grade` when `health.missing_checks` names a check. `sections` holds the
  JSON report of each check that ran, keyed by check name. Each section is the
  same as the output of the command of the same name. `findings` holds the
  findings of each check in one shape: `id`, `check`, `rule`, `severity`,
  `message`, `location`, `related_locations` and `actions`.
- `sarif`: a SARIF 2.1.0 log on stdout, with one run. The results, the rules
  and the gate notifications are those of the SARIF log of each check.
- `codeclimate`: a JSON array with one CodeClimate issue for each finding, for
  the code quality report of GitLab. Each command has this format.
- `compact`: one line for each finding, `path:line:col: rule: message`, for
  grep and for agents. A finding with no column has `path:line: rule: message`,
  and a finding with no line has `path: rule: message`. A line break in a
  message becomes a space. A run with no finding writes nothing. The tool
  does not escape a path, so a path with `:` makes the place ambiguous.
- `markdown`: a title with the issue count, the scope and health lines, a
  table of the gates, and one table for each check that ran, for a pull
  request body or a job summary. A table shows at most 50 findings, and a line
  under it gives the count of the rest. The tables stop before the document
  is 60000 bytes, under the size limit of a GitHub comment. Outside a code
  span, the tool escapes each character that markdown or HTML reads. The parse gate has a table only when a
  file does not parse.

- `github-annotations`: GitHub workflow commands, one annotation for each
  SARIF result, at most 50, the errors first. A notice after the list gives
  the count when the run has more results. Each path starts at the
  repository root (`git rev-parse --show-prefix` of the workspace root), so
  GitHub finds the file. An error of the run is an `::error` command on
  stdout.
- `github-summary`: a GitHub job summary in markdown: the issue count and the
  exit code, a gate table with the observed values, and a table of at most 50
  findings. Append it to `$GITHUB_STEP_SUMMARY`.

- `badge`: a flat SVG badge with the health grade, for example
  `fallow rust: A (92)`, on stdout. The number is the whole part of the score.
  Only `check` gives it. A run with no health score, for example
  `check --only deps` or a scoped run, gives a grey `no score` badge. The run
  exits 0 also when a gate fails, so a job can upload the badge. Another
  command with `--format badge` exits 2 before it runs. The badge has the
  shape and the colors of the fallow badge for JavaScript.

```bash
cargo fallow check --format github-annotations --quiet
cargo fallow check --format github-summary --quiet >> "$GITHUB_STEP_SUMMARY"
```

```bash
cargo fallow check --format badge --quiet > fallow-badge.svg
```

Each format is one run. For `mutants`, which takes minutes, pick one format
for the run.

The annotations are the same text as the jq filter of the GitHub Action, so
a workflow without the action gets the same annotations. The summary escapes
more characters than the jq filter, so its text can differ, but GitHub shows
it the same. An error of the run is an `::error` command in
`github-annotations`, and a short summary in `github-summary`.

Each command has the `compact` and `markdown` formats, `mutants` too. A
`mutants --list` run has no finding, so these formats show no mutant. The
GitHub formats are on the same commands. `fix` and `mutants --inspect` have
none of these four formats, and exit 2.

A finding `id` and a CodeClimate `fingerprint` are the same value: 16 hex
digits of an FNV-1a hash of the check, the rule and the identity of the
finding. The identity uses names where a name is unique, for example the
dependency key, or the crate and the item path, so an edit above a finding does
not change its id. The hash is the same as in the JavaScript CLI.

The gates of `check` are the gates of the checks that ran: `parse-error`,
`error-severity-findings` (deps), `unused-files` (files),
`visibility-findings`, `boundary-violations`, `file-growth` (size),
`health-findings` (complexity), `duplication-findings` and
`duplication-threshold` (dupes). Each gate except `unused-files` has the same
name as a gate of the JavaScript CLI. A gate fails the run when its `status`
is `fail` and `enforced` is `true`.

The tool parses each target root file on a thread with a stack that is sized
from the file length, so deep nesting cannot crash the run. A file that needs
more stack than the tool can reserve counts for the `parse-error` gate with
`reason: "nesting-too-deep"`.

A target root file that cannot be read as UTF-8 text is not a parse error.
The tool writes a warning to stderr, and the `parse-error` gate does not
judge the file. `--quiet` does not hide the warning.

With `--format json`, an error goes to stdout as an error document:
`{ "error": true, "message": "...", "exit_code": 2, "code": "..." }`. In the
other formats, the error goes to stderr.

## JSON Schema

[`docs/output-schema.json`](output-schema.json) is the JSON Schema
(draft 2020-12) of each JSON document of the tool. `cargo fallow schema`
prints the same schema.

- The `document-<name>` definitions describe one document each: the report
  of each command (`check`, `deps`, `fix`, `visibility`, `files`,
  `boundaries`, `size`, `complexity`, `dupes` and `mutants`), the inspect report of
  `mutants --inspect` (`mutants-inspect`), the baseline file (`baseline`) and
  the error document (`error`).
- Each report has `schema_version` 1. A change that removes a member or
  changes the type of a member increments the version of its report.
- A test generates the schema and compares it with the file, so a report
  change without a schema change fails CI. Run the tests with
  `CARGO_FALLOW_UPDATE_SCHEMA=1` to write the file again.

## Explain a rule

`cargo fallow explain <rule>` gives the meaning of a rule, an example and the
fix. An agent can read a rule from the tool, with no documentation site.

```bash
cargo fallow explain unused-dependency
cargo fallow explain dead_public --format json
cargo fallow check --format json --explain
```

- `--format json` gives `rule`, `check`, `severity`, `meaning`, `example`,
  `fix` and `docs`. A `_` and a `-` in the rule name are the same.
- An unknown rule exits 2, and the help of the error lists each rule.
- `--explain` on `check` and on each static command adds `_meta` to the JSON
  report. `_meta.rules` holds the explanation of each rule with a finding,
  keyed by the rule of the finding. A boundaries finding has the name of its
  rule from the configuration, and gets the `boundaries` entry. `_meta.docs`
  holds the documentation address. Only the JSON format has `_meta`, and
  `mutants` has no `--explain`.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | No enforced gate failed. |
| `1` | An enforced gate failed. For `mutants`, a survivor with `--fail-on-issues` or `--fail-on-survivors`. For `visibility`, a finding with `--fail-on-issues`. |
| `2` | The input was not valid, or the run could not complete. |

Each command uses these codes and the same error document.
