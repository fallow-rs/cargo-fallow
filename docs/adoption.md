# Adopt cargo-fallow on an existing workspace

These flags and comments let a workspace with old findings turn on the gates now.

## Changed files

`--changed-since <REF>` scopes a run to the files that a branch changed. The
flag is on `check`, `deps`, `files`, `visibility`, `boundaries`, `size`,
`complexity` and `dupes`. Use it for a pull request gate that judges only the changed code.

```bash
cargo fallow check --changed-since origin/main --fail-on-issues
```

- The analysis still reads the whole workspace. Only the report changes.
- The base is the fork point of `REF` and `HEAD`, the same rule as
  `mutants --in-diff`. A committed, a staged, an unstaged and an untracked
  change count.
- The report keeps a finding when its file changed.
- A finding in a `Cargo.toml` (for example a `deps` finding) stays when a file
  of its package changed: the manifest, or a file in the package folder that
  no nested member owns.
- A clone group of `dupes` stays when one of its copies is in a changed file.
- The gates count only the kept findings. The `duplication-threshold` gate
  measures the whole workspace, so a scoped run does not give it.
- The `parse-error` gate always judges each target root file. A file that does
  not parse makes each check incomplete.
- The JSON report has a `changed_since` member with `ref`, `base` (the fork
  point) and `changed_files`. The human output has a `Scope:` line: first for
  a single command, and under the header for `check`.
- The `dupes` summary counts only the kept groups and their families. Its
  line, token and file counts stay those of the whole workspace.
- With `visibility --precise --fail-on-issues`, a run that leaves a candidate
  undecided exits 2 also when the candidate is outside the scope.
- A `check` run with the flag has no health score: its `health` object has
  `scope: "changed"` and no penalties.
- An unknown ref, a ref that starts with `-`, a shallow clone without the fork
  point, or a run outside a git work tree exits 2. The run never falls back to
  the whole workspace.

## Packages

`--package <NAME>` (`-p`) scopes a run to part of a workspace. The flag can
repeat, and it is on the same commands as `--changed-since`.

```bash
cargo fallow check -p my-core -p my-cli --fail-on-issues
```

- The analysis still reads the whole workspace. Only the report changes.
- A finding stays when a named member owns its file: the member with the
  deepest folder that holds the file. A finding in a `Cargo.toml` follows the
  member of that manifest. A finding of the root `Cargo.toml` of a virtual
  workspace belongs to no member.
- A clone group of `dupes` stays when one of its copies is in a named member.
  With `--changed-since` too, one copy must be in a changed file of a named
  member.
- The gates count only the kept findings, the same as for `--changed-since`.
  The report has a `packages` member with the sorted names, and the human
  output has a `Scope:` line.
- A `check` run with the flag has no health score: `health.scope` is
  `package`, or `changed` when the run also has `--changed-since`. The run
  does not judge stale baseline entries.
  `--save-baseline` cannot go with `--package`.
- A name that is not a workspace member exits 2, and the help names the
  members.

## Rule severity

`[workspace.metadata.fallow.rules]` sets each rule to `error`, `warn` or
`off`. `warning` is an alias of `warn`. A rule that the table does not name
keeps its own severity and gate.

```toml
[workspace.metadata.fallow.rules]
unused-optional-dependency = "error"
dead_public = "warn"
code-duplication = "off"
```

- `off`: the rule gives no finding, so a baseline does not hold it either.
- `error`: the finding has the `error` severity, its SARIF result has the
  level `error`, and it counts for the gate of its check, for example
  `error-severity-findings` of `deps`.
- `warn`: the finding has the `warn` severity, its SARIF result has the level
  `warning`, and it does not count for the gate of its check.
- A rule name is a name of `cargo fallow explain`. `_` and `-` are the same,
  so `dead_public` and `dead-public` are one rule. A boundary rule uses the
  `rule` name of its table, the kind (`crate-dependency`, `source-pattern`,
  `module-path`) for each rule of the kind, or `boundaries` for each boundary
  rule. The first of these names that the table has wins.
- `code-duplication = "off"` also removes the `duplication-threshold` gate.
- `parse-error`, `surviving-mutant` and `stale-suppression` have a fixed
  level. An unknown rule, a fixed rule or an unknown level exits 2 with
  `CARGO_FALLOW_INVALID_CONFIG`.

## Baselines

A baseline lets a workspace with old findings turn on the gates now. The run
then fails only on new findings. The flags are on the same commands as
`--changed-since`.

```bash
cargo fallow check --save-baseline fallow-baseline.json
cargo fallow check --baseline fallow-baseline.json --fail-on-issues
```

- `--save-baseline <PATH>` writes the `id`, the `check` and the `rule` of each
  finding to a JSON file, and the run exits 0. Commit the file.
- `--baseline <PATH>` leaves out each finding whose id is in the file, before
  the gates count. A clone group of `dupes` has one entry: the `fingerprint` of
  the group.
- The ids do not use line numbers, so an edit above a finding keeps its id.
- The JSON report has a `baseline` member with `path`, `matched` (the entries
  that a finding matched) and `stale` (the entries of the checks that ran with
  no finding in the run). The human output has a `Baseline:` line.
- The `stale-baseline` gate fails when an entry is stale.
  `--fail-on-stale-baseline` enforces it. Save the baseline again to remove
  the stale entries.
- A run with `--changed-since` does not judge stale entries, so the gate is
  `skipped`. A flag that narrows the findings has the same effect:
  `visibility --kind`, the `--mode`, `--min-tokens` and `--min-lines` flags of
  `dupes`, the limit flags of `complexity`, and each `size` run. A `check` run
  judges only the entries of the checks that ran. `--save-baseline` cannot go with `--changed-since` or with
  `--baseline`.
- A missing file, a file that is not a baseline, or a file of another version
  exits 2.
- A `check` run with `--baseline` has no health score: its `health` object has
  `scope: "baseline"` and no penalties.
- A run with `--save-baseline` enforces no gate: each gate has
  `enforced: false`.

The file has `schema_version` 1, the `kind` of the command that saved it,
`language: "rust"`, the key scheme in `identity` and the sorted `findings`.

`baseline = "<PATH>"` in `[workspace.metadata.fallow]` is the baseline of each
run without `--baseline`. The path is relative to the workspace root, and it
cannot leave the root. A missing file exits 2. A run with `--save-baseline`
does not read it. `--fail-on-stale-baseline` works with the key too. The
`--baseline` flag takes a path from the current folder.

## Init

`cargo fallow init` gives a starting configuration for a workspace.

```bash
cargo fallow init            # print the tables
cargo fallow init --write    # add them to Cargo.toml and write the baseline
cargo fallow init --ci github > .github/workflows/cargo-fallow.yml
```

- The command runs the static checks. It prints the tables of
  `[workspace.metadata.fallow]` with the limits at the current maximum of the
  workspace: `max-cyclomatic` and `max-cognitive` of `complexity`,
  `max-file-lines` of `size` and `threshold` of `dupes`. A limit is never
  under its default, and `threshold = 0.0` means no threshold gate.
- The limits are a ratchet. A new function under the current maximum passes.
  Lower the limits over time.
- With `--write`, the command saves the findings of a run with the new limits
  as a baseline, and the tables set `baseline = "fallow-baseline.json"`. So
  the first `cargo fallow check --fail-on-issues` passes, and a new finding
  fails it. The baseline has no entry that the new limits make stale.
- `--write` adds the tables to the root `Cargo.toml` with the comments and the
  format kept. A key that the manifest has keeps its value. A second `--write`
  changes nothing. A baseline file that exists stays, and the command warns.
  A root manifest with no `[workspace]` table gets one. A table written in
  the inline form, for example `metadata = { ... }`, stops the command with
  exit code 2.
- The baseline does not hide a file that does not parse: the `parse-error`
  gate still judges each target root file.
- `--ci github` prints a GitHub Actions workflow that runs `check` with the
  action. Pin each action to a commit before you use it.
- `--format json` gives `kind`, `version`, `tables`, `baseline`,
  `baseline_findings` and `written`.

## Suppressions

A comment hides the findings that a person accepts. Each check reads the same
comments:

- `// fallow-ignore-next-line <kind>` hides the findings of the next code
  line. Attribute lines, doc comments and blank lines between the comment and
  the code do not count, so the comment can stand above `#[derive(...)]`.
- `// fallow-ignore-file <kind>` in the first ten lines hides the findings of
  the file.
- In a `Cargo.toml`, the comment starts with `#`:
  `# fallow-ignore-next-line unused-dependency` above an entry.
- A block comment works too: `/* fallow-ignore-next-line size */`.
- A list takes commas or spaces. Text after ` -- ` is a reason:
  `// fallow-ignore-next-line visibility -- kept for a plugin`.
- A comment with no kind hides each finding.

| Check | Kinds | Place of the comment |
| --- | --- | --- |
| `deps` | `deps` or a rule, for example `unused-dependency` | Above the entry in `Cargo.toml`, or a file comment in the manifest |
| `files` | `files` or `unused-file` | A file comment in the unused file |
| `visibility` | `visibility`, `dead_public`, `unnecessary_public` or `unnecessary_restricted_visibility` | Above the item |
| `boundaries` | `boundaries` or the `rule` name of the configuration | Above the line of the violation |
| `size` | `size`, `grown-file` or `new-large-file` | A file comment in the file |
| `complexity` | `complexity` | Above the function |
| `dupes` | `dupes` or `code-duplication` | Above the copy |

A rule name can use `-` or `_`. Each command and each section of `check` has
`suppressed`: the number of hidden findings. A comment with a kind of a check
that ran, and that hides no finding, is stale: the check reports a
`stale-suppression` warning with a `remove-suppression` action, and the JSON
report lists it in `stale_suppressions`. The gates do not count it. A
`--changed-since` run judges only the comments in the changed files. A kind
that names no check or rule gives a warning on stderr.
