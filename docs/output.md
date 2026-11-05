# Output formats

This page describes the output of `check`. [Output](mutation-testing.md#output)
describes the output of `mutants`.

- `human`: the score, then the text of each check, then one gate block, on
  stdout. The progress lines go to stderr. Only the `parse-error` gate lists
  its files, because the findings name the files of the other gates. When a
  gate fails, the last lines name it. A `Failed:` line lists each enforced
  gate that fails, and a second line counts the gates that fail but are not
  enforced. A run with findings ends with a `Next:` line that names
  `cargo fallow explain` and, when an edit is automatic,
  `cargo fallow fix --dry-run`.
- `json`: the report on stdout. The report has `schema_version` (1),
  `version`, `kind`, `language`, `workspace`, `checks`, `total_issues`,
  `health`, `findings`, `gate_outcomes` and `sections`. When
  `health.missing_checks` names a check, `health` has no `score` and no
  `grade`. `sections` has the JSON report of each check that ran, keyed by
  check name. Each section is the same as the output of the command with the
  same name. `findings` has the findings of all checks in one shape: `id`,
  `check`, `rule`, `severity`, `message`, `location`, `related_locations` and
  `actions`.
- `sarif`: a SARIF 2.1.0 log on stdout, with one run. The results, the rules
  and the gate notifications come from the SARIF log of each check.
- `codeclimate`: a JSON array with one CodeClimate issue for each finding, for
  the code quality report of GitLab. Each command has this format. The
  [GitLab CI template](gitlab.md) writes it for the merge request widget.
- `compact`: one line for each finding, `path:line:col: rule: message`, for
  grep and for agents. A finding with no column has `path:line: rule: message`,
  and a finding with no line has `path: rule: message`. A line break in a
  message becomes a space. A run with no finding writes nothing. The format
  does not escape paths. When a path has a `:`, the place of the finding is
  ambiguous.
- `markdown`: a document for a pull request body or a job summary. It has a
  title with the issue count, the scope and health lines, a table of the
  gates, and one table for each check that ran. A table shows at most 50
  findings, and a line under the table gives the count of the other findings.
  The output adds no more table rows when the next row makes the document
  larger than 60000 bytes. This limit is under the size limit of a GitHub
  comment. Outside a code span, the output
  escapes each character that has a meaning in markdown or HTML. The parse
  gate has a table only when a file does not parse.

- `github-annotations`: GitHub workflow commands, with one annotation for each
  SARIF result. The output has at most 50 annotations, with the errors first.
  When the run has more results, a notice after the list gives the count.
  Each path is relative to the repository root
  (`git rev-parse --show-prefix` of the workspace root), so GitHub can find the
  file. An error of the run is an `::error` command on stdout.
- `github-summary`: a GitHub job summary in markdown. It has the issue count
  and the exit code, a gate table with the observed values, and a table of at
  most 50 findings. Append it to `$GITHUB_STEP_SUMMARY`.

- `badge`: a flat SVG badge with the health grade on stdout, for example
  `fallow rust: A (92)`. The number is the whole part of the score. Only
  `check` has this format. A run with no health score gives a grey `no score`
  badge, for example `check --only deps` or a scoped run. The run exits 0 also
  when a gate fails, so a job can upload the badge. When another command gets
  `--format badge`, it exits 2 before it runs. The badge has the shape and the
  colors of the fallow badge for JavaScript.

```bash
cargo fallow check --format github-annotations --quiet
cargo fallow check --format github-summary --quiet >> "$GITHUB_STEP_SUMMARY"
```

```bash
cargo fallow check --format badge --quiet > fallow-badge.svg
```

Each run writes one format. A `mutants` run takes minutes, so choose its
format before you start it.

The annotations have the same text as the output of the jq filter in the
GitHub Action, so a workflow without the action gets the same annotations.
`github-summary` escapes more characters than the jq filter. Its text can be
different, but GitHub shows the same result. When the run has an error,
`github-annotations` writes an `::error` command, and `github-summary` writes
a short summary.

All commands have the `compact`, `markdown`, `github-annotations` and
`github-summary` formats, with two exceptions: `fix` and `mutants --inspect`.
These two exit 2 with any of the four formats. A `mutants --list` run has no
findings, so these formats show no mutant.

A finding `id` and a CodeClimate `fingerprint` have the same value. The value
is 16 hex digits of an FNV-1a hash of the check, the rule and the identity of
the finding. Where a name is unique, the identity uses the name, for example
the dependency key, or the crate and the item path. Because of this, an edit
above a finding does not change its id. The JavaScript CLI uses the same hash.

The gates of `check` are the gates of the checks that ran: `parse-error`,
`error-severity-findings` (deps), `unused-files` (files),
`visibility-findings`, `boundary-violations`, `file-growth` (size),
`health-findings` (complexity), `duplication-findings` and
`duplication-threshold` (dupes). Each gate except `unused-files` has the same
name as a gate of the JavaScript CLI. The run fails when the `status` of a
gate is `fail` and its `enforced` value is `true`.

cargo-fallow parses each target root file on a separate thread. The stack
size of that thread comes from the file length, so deep nesting does not
crash the run. When a file needs more stack than cargo-fallow can reserve, the
file counts for the `parse-error` gate with `reason: "nesting-too-deep"`.

A target root file that is not valid UTF-8 text is not a parse error.
cargo-fallow writes a warning to stderr, and the `parse-error` gate does not
include the file. `--quiet` does not hide the warning.

With `--format json`, cargo-fallow writes an error to stdout as an error
document: `{ "error": true, "message": "...", "exit_code": 2, "code": "..." }`.
In the other formats, it writes the error to stderr.

## JSON Schema

[`docs/output-schema.json`](output-schema.json) is the JSON Schema
(draft 2020-12) of each JSON document that cargo-fallow writes.
`cargo fallow schema` prints the same schema.

- Each `document-<name>` definition describes one document. There is one for
  the report of each command (`check`, `deps`, `fix`, `visibility`, `files`,
  `boundaries`, `size`, `complexity`, `dupes` and `mutants`). The others are
  the inspect report of `mutants --inspect` (`mutants-inspect`), the baseline
  file (`baseline`), the reports of `hooks install` and `hooks uninstall`
  (`hooks`) and `hooks status` (`hooks-status`), and the error document
  (`error`).
- Each report has `schema_version` 1. When a change removes a field or changes
  the type of a field, the maintainers increment the version of that report.
- A test generates the schema and compares it with the file. When a report
  changes and the schema file does not, CI fails. To write the file again, run
  the tests with `CARGO_FALLOW_UPDATE_SCHEMA=1`.

## Explain a rule

`cargo fallow explain <rule>` prints the meaning of a rule, an example and the
fix. An agent can use it to read about a rule without a documentation site.

```bash
cargo fallow explain unused-dependency
cargo fallow explain dead-public --format json
cargo fallow check --format json --explain
```

- With `--format json`, the output has `rule`, `check`, `severity`,
  `meaning`, `example`, `fix` and `docs`. A `_` and a `-` in the rule name are
  equal.
- For an unknown rule, the command exits 2, and the help text of the error
  lists all rules.
- `--explain` on `check` and on each static command adds `_meta` to the JSON
  report. `_meta.rules` has the explanation of each rule that has a finding,
  keyed by the rule of the finding. The rule of a `boundaries` finding is the
  rule name from the configuration, and its value in `_meta.rules` is the
  `boundaries` explanation. `_meta.docs` has the documentation address.
- Only the JSON format has `_meta`. `mutants` has no `--explain` flag.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | No enforced gate failed. |
| `1` | An enforced gate failed. For `mutants`, a survivor with `--fail-on-issues` or `--fail-on-survivors`. For `visibility`, a finding with `--fail-on-issues`. |
| `2` | The input was not valid, or the run could not complete. |

All commands use these exit codes and the same error document.
