# Adopt cargo-fallow on an existing workspace

If your workspace already has findings, use these flags and comments to turn
on the gates before you fix the old findings.

## Changed files

`--changed-since <REF>` limits the report to the files that a branch changed.
These commands have the flag: `check`, `deps`, `files`, `visibility`,
`boundaries`, `size`, `complexity` and `dupes`. Use it for a pull request gate
that checks only the changed code.

```bash
cargo fallow check --changed-since origin/main --fail-on-issues
```

- cargo-fallow still analyzes the whole workspace. The scope changes only which findings go into the report.
- The base is the fork point of `REF` and `HEAD`. `mutants --in-diff` uses the
  same rule. Committed, staged, unstaged and untracked changes all count.
- The report has a finding only when the file of the finding changed.
- A finding in a `Cargo.toml`, for example a `deps` finding, stays in the
  report when a file of its package changed. A file of the package is the
  manifest, or a file in the package folder that is not in a nested member.
- A clone group of `dupes` stays in the report when one of its copies is in a
  changed file.
- Only the findings in the report count for the gates. The
  `duplication-threshold` gate is a measure of the whole workspace, so a
  scoped run does not have this gate.
- The `parse-error` gate includes each target root file, also outside the
  scope. When a file does not parse, each check is incomplete.
- The JSON report has a `changed_since` field with `ref`, `base` (the fork
  point) and `changed_files`. The human output has a `Scope:` line. For a
  single command, this line is the first line. For `check`, it is under the
  header.
- In the `dupes` summary, the counts of groups and families include only the
  groups in the report. The line, token and file counts are still for the
  whole workspace.
- With `visibility --precise --fail-on-issues`, the run exits 2 when it leaves
  a candidate undecided. This is also true when the candidate is outside the
  scope.
- A `check` run with the flag has no health score. Its `health` object has
  `scope: "changed"` and no penalties.
- The run exits 2 for an unknown ref, a ref that starts with `-`, a shallow
  clone without the fork point, or a run outside a git work tree. The command
  does not report the whole workspace in these cases.

## Packages

`--package <NAME>` (`-p`) limits the report to part of a workspace. You can
repeat the flag. The commands that have `--changed-since` also have this flag.

```bash
cargo fallow check -p my-core -p my-cli --fail-on-issues
```

- cargo-fallow still analyzes the whole workspace. The scope changes only which findings go into the report.
- A finding stays in the report when its file is in a named member. A file is
  in the member with the deepest folder that contains the file.
- A finding in a `Cargo.toml` is in the member of that manifest. A finding in
  the root `Cargo.toml` of a virtual workspace is in no member.
- A clone group of `dupes` stays in the report when one of its copies is in a
  named member. When the run also has `--changed-since`, one copy must be in a
  changed file of a named member.
- Only the findings in the report count for the gates, the same as for
  `--changed-since`. The JSON report has a `packages` field with the sorted
  names, and the human output has a `Scope:` line.
- A `check` run with the flag has no health score. `health.scope` is
  `package`, or `changed` when the run also has `--changed-since`.
- The run does not look for stale baseline entries. You cannot use
  `--save-baseline` with `--package`.
- When a name is not a workspace member, the run exits 2. The help text of the
  error lists the members.

## Rule severity

`[workspace.metadata.fallow.rules]` sets the level of each rule to `error`,
`warn` or `off`. `warning` is an alias of `warn`. A rule that is not in the
table has its default severity and gate.

A finding that counts for a gate has the default severity `error`: the
`error` rules of `deps`, and each rule of `files`, `visibility`,
`boundaries`, `size`, `complexity`, `dupes`, and the surviving mutants of
`mutants`. The rules table cannot change `surviving-mutant`. A finding that
counts for no gate is a warning, for example `unused-optional-dependency` or
`stale-suppression`. `cargo fallow explain <rule>` shows the default of each
rule. A gate fails the run only with its flag, for example
`--fail-on-issues`. In SARIF, the level of a result follows the severity of
its finding: `error` gives `error`, and `warn` gives `warning`. A gate flag
does not change the level.

```toml
[workspace.metadata.fallow.rules]
unused-optional-dependency = "error"
dead-public = "warn"
code-duplication = "off"
```

- `off`: the rule gives no finding, so a baseline has no entry for it either.
- `error`: the finding has the `error` severity, and its SARIF result has the
  level `error`. The finding counts for the gate of its check, for example
  `error-severity-findings` of `deps`.
- `warn`: the finding has the `warn` severity, and its SARIF result has the
  level `warning`. The finding does not count for the gate of its check. The
  `duplication-threshold` gate of `dupes` measures the duplicated lines, not
  the findings, so `code-duplication = "warn"` does not change that gate.
- A rule name is one of the names of `cargo fallow explain`. `_` and `-` are
  equal, so `dead_public` and `dead-public` are one rule.
- For a boundary rule, you can use three names. The first name is the `rule`
  name of its table. The second is the kind (`crate-dependency`,
  `source-pattern` or `module-path`), which sets each rule of that kind. The
  third is `boundaries`, which sets each boundary rule. When the table has
  more than one of these names, the first name in this order wins.
- `code-duplication = "off"` also removes the `duplication-threshold` gate.
- `parse-error`, `surviving-mutant` and `stale-suppression` have a fixed
  level. For an unknown rule, a fixed rule or an unknown level, the run exits
  2 with `CARGO_FALLOW_INVALID_CONFIG`.

## Baselines

With a baseline, you can turn on the gates in a workspace that already has
findings. A run then fails only on new findings. The commands that have
`--changed-since` also have the baseline flags.

```bash
cargo fallow check --save-baseline fallow-baseline.json
cargo fallow check --baseline fallow-baseline.json --fail-on-issues
```

- `--save-baseline <PATH>` writes the `id`, the `check` and the `rule` of
  each finding to a JSON file. The run then exits 0. Commit the file.
- `--baseline <PATH>` removes each finding whose id is in the file. A removed
  finding does not count for a gate. A clone group of `dupes` has one entry,
  which is the `fingerprint` of the group.
- The ids do not use line numbers, so an edit above a finding does not change
  its id.
- The JSON report has a `baseline` field with `path`, `matched` and `stale`.
  `matched` lists the entries that match a finding. `stale` lists the entries
  of the checks that ran that match no finding in the run. The human output
  has a `Baseline:` line.
- The `stale-baseline` gate fails when an entry is stale. To enforce this
  gate, use `--fail-on-stale-baseline`. To remove the stale entries, save the
  baseline again.
- A run with `--changed-since` does not look for stale entries, so the gate
  is `skipped`. The same is true for a run with a flag that limits the
  findings: `visibility --kind`, the `--mode`, `--min-tokens` and
  `--min-lines` flags of `dupes`, the limit flags of `complexity`, and each
  `size` run.
- In a `check` run, only the entries of the checks that ran can be stale.
- You cannot use `--save-baseline` with `--changed-since` or with
  `--baseline`.
- The run exits 2 when the file is missing, when it is not a baseline, or
  when it has another version.
- A `check` run with `--baseline` has no health score. Its `health` object has
  `scope: "baseline"` and no penalties.
- A run with `--save-baseline` does not enforce a gate. Each gate has
  `enforced: false`.

The baseline file has `schema_version` 1, the `kind` of the command that
saved it, `language: "rust"`, the key scheme in `identity` and the sorted
`findings`.

`baseline = "<PATH>"` in `[workspace.metadata.fallow]` sets the baseline for
each run without `--baseline`. The path is relative to the workspace root,
and it must stay inside the root. When the file is missing, the run exits 2.
A run with `--save-baseline` does not read this key. `--fail-on-stale-baseline`
also works with the key. The path of the `--baseline` flag is relative to the
current folder.

## Init

`cargo fallow init` makes a starting configuration for a workspace.

```bash
cargo fallow init            # print the tables
cargo fallow init --write    # add them to Cargo.toml and write the baseline
cargo fallow init --ci github > .github/workflows/cargo-fallow.yml
```

`init` accepts `--locked`, `--offline` and `--frozen` for its `cargo metadata`
calls.

- The command runs the static checks and prints the tables of
  `[workspace.metadata.fallow]`. Each limit is set to the current maximum of
  the workspace: `max-cyclomatic` and `max-cognitive` of `complexity`, and
  `threshold` of `dupes`. A limit is never lower than its default.
  `threshold = 0.0` means that there is no threshold gate.
- `init` does not set `max-file-lines` of `size`. A large file fails only
  when it grows, so the default limit passes on the first run. A larger
  limit only turns off the growth rule.
- Because each limit is at the current maximum, a new function under the
  maximum passes. Lower the limits over time.
- With `--write`, the command saves the findings of a run with the new limits
  as a baseline, and the tables set `baseline = "fallow-baseline.json"`. As a
  result, the first `cargo fallow check --fail-on-issues` passes, and a new
  finding makes it fail. With the new limits, no baseline entry is stale.
- `--write` adds the tables to the root `Cargo.toml`, and keeps the comments
  and the format of the file. When the manifest already has a key, the
  command does not change its value. A second `--write` changes nothing.
- When the baseline file exists, the command does not change it and writes a
  warning. When the root manifest has no `[workspace]` table, the command adds
  one. When a table uses the inline form, for example `metadata = { ... }`,
  the command stops with exit code 2.
- A file that does not parse still counts for the `parse-error` gate after
  `--write`, because this gate includes each target root file.
- `--ci github` prints a GitHub Actions workflow that runs `check` with the
  cargo-fallow action. Before you use the workflow, pin each action to a
  commit.
- With `--format json`, the output has `kind`, `version`, `tables`,
  `baseline`, `baseline_findings` and `written`. `baseline_findings` is the
  number of findings that the run saved, or `null` when the run wrote no
  baseline.

## Suppressions

To hide a finding that you accept, add a comment. All checks use the same
comments:

- `// fallow-ignore-next-line <kind>` hides the findings of the next code
  line. Attribute lines, doc comments and blank lines between the comment and
  the code do not count, so you can put the comment above `#[derive(...)]`.
- `// fallow-ignore-file <kind>` hides the findings of the file. Put it in the
  first ten lines of the file.
- In a `Cargo.toml`, start the comment with `#`, for example
  `# fallow-ignore-next-line unused-dependency` above an entry.
- A block comment also works, for example `/* fallow-ignore-next-line size */`.
- To give more than one kind, separate the kinds with commas or spaces. Text
  after ` -- ` is a reason, for example
  `// fallow-ignore-next-line visibility -- kept for a plugin`.
- A comment with no kind hides all findings of its line or file.

| Check | Kinds | Place of the comment |
| --- | --- | --- |
| `deps` | `deps` or a rule, for example `unused-dependency` | Above the entry in `Cargo.toml`, or a file comment in the manifest |
| `files` | `files` or `unused-file` | A file comment in the unused file |
| `visibility` | `visibility`, `dead-public`, `unnecessary-public` or `unnecessary-restricted-visibility` | Above the item |
| `boundaries` | `boundaries` or the `rule` name of the configuration | Above the line of the violation |
| `size` | `size`, `grown-file` or `new-large-file` | A file comment in the file |
| `complexity` | `complexity` | Above the function |
| `dupes` | `dupes` or `code-duplication` | Above the copy |

A rule name can use `-` or `_`. Each command, and each section of `check`,
has a `suppressed` field with the number of hidden findings.

A comment is stale when its kind names a check that ran and the comment hides
no finding. For each stale comment, the check reports a `stale-suppression`
warning with a `remove-suppression` action. The JSON report lists the stale
comments in `stale_suppressions`. A `stale-suppression` warning does not count
for a gate.

A run does not report a comment as stale in these cases:

- The run has a flag that limits the findings. These are the same flags as
  for stale baseline entries.
- The comment is a `size` comment, and the run has no growth comparison. A
  `check` run has no growth comparison for `size`.
- The comment is in a package that `deps` or `files` skipped, and the comment
  is for that check.
- The run has `--changed-since` or `--package`, and the comment is outside
  the files of the scope.

When a kind names no check and no rule, the run writes a warning to stderr.
