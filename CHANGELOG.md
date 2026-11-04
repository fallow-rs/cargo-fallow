# Changelog

All notable changes to `cargo-fallow` are in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The release workflow copies the section of the released version into the
GitHub release. Each release needs a `## [X.Y.Z] - YYYY-MM-DD` section.

## [Unreleased]

### Added

- The GitHub Action has the inputs `changed-since`, `auto-changed-since`,
  `baseline` and `save-baseline`, and `comment` posts or updates one sticky
  pull request comment with the gates and the findings.
- `cargo fallow init` prints the configuration tables with the limits at the
  current maximum, and saves the current findings as a baseline. `--write`
  adds them to the root `Cargo.toml`. `--ci github` prints a workflow. The
  `baseline` key of `[workspace.metadata.fallow]` names the baseline of each
  run.
- `cargo fallow explain <rule>` gives the meaning, an example and the fix of a
  rule, also as JSON. `--explain` adds `_meta` with the rules of the findings
  to the JSON report of `check` and of each static command.
- One suppression model for each check: `// fallow-ignore-next-line <kind>`
  now also works for `visibility`, `size`, `boundaries` and `files`,
  `// fallow-ignore-file <kind>` hides the findings of a file, and
  `# fallow-ignore-next-line <rule>` in `Cargo.toml` hides a `deps` finding.
  A comment that hides no finding gives a `stale-suppression` warning, and
  each report has a `suppressed` count.
- `cargo fallow check` reads the workspace and reports the parse gates.
- `cargo fallow mutants` runs mutation tests with schemata and coverage
  selection.
- `cargo fallow deps`, `visibility`, `boundaries`, `size`, `complexity` and
  `dupes` report native checks of the workspace.
- JSON, SARIF and human output, with exit codes 0, 1 and 2.
- Each release archive has an Ed25519 signature (`.sig`). The GitHub Action
  installer checks it before the SHA-256 and fails closed. The installer now
  downloads the archive names that the release workflow writes.
- `cargo fallow fix` removes each unused dependency entry and moves each
  misplaced entry to `[dev-dependencies]`. `--dry-run` shows the edits. The
  edit keeps comments, and a failed load after the edit writes back the old
  bytes. The `deps` actions that `fix` applies now have `auto_fixable: true`.
- `cargo fallow fix` also narrows the visibility keyword of each `visibility`
  finding. It runs `cargo check --workspace --all-targets` before and after
  the edits, and writes back each file when the check after the edits fails.
  The `change-visibility` actions now have `auto_fixable: true`.
- `cargo fallow files` reports each `.rs` file in a target folder that no
  target compiles, with the rule `unused-file` and the gate `unused-files`.
  `check` runs it by default.
- `boundaries`, `size`, `complexity`, `dupes` and `mutants` accept
  `--fail-on-issues` as their gate flag. The older gate flags stay. The GitHub
  Action now gives `--fail-on-issues` to each command.
- `check`, `deps`, `visibility`, `boundaries`, `size`, `complexity` and
  `dupes` accept `--changed-since <REF>`. The report keeps only the findings
  in the files that changed since the fork point of the ref. The JSON report
  has a `changed_since` member.
- `--save-baseline <PATH>` and `--baseline <PATH>` on the same commands. A run
  with a baseline leaves out the saved findings, and the
  `stale-baseline` gate (`--fail-on-stale-baseline`) fails on an entry with no
  finding.
- `cargo fallow schema` prints the JSON Schema of each JSON document, and
  `docs/output-schema.json` holds a copy. A test fails when the two differ.
  The `check` report now has `schema_version` 1, the same as each other
  report.
- `--package <NAME>` (`-p`) on `check` and the static commands keeps only the
  findings of the named workspace members. The analysis still reads the whole
  workspace. An unknown name exits 2.
- `--format compact` gives one line for each finding,
  `path:line:col: rule: message`, and `--format markdown` gives a gate table
  and one table for each check. Each command has both formats, except `fix`
  and `mutants --inspect`.
- `--format github-annotations` writes GitHub workflow commands, at most 50,
  the errors first, with paths from the repository root.
  `--format github-summary` writes a GitHub job summary. The annotations are
  the same text as the jq filter of the GitHub Action, and the summary shows
  the same in GitHub.

### Fixed

- The action input `base: auto` now takes the commit before a push on a push
  event. A new branch and a commit that the checkout cannot fetch give no base
  and a notice. The size self-test runs on push again.
- The `fingerprint` of a `dupes` clone group now depends only on the tokens of
  the group. Before, a new token in another file changed it.
- A `#[path]` attribute in an inline module now resolves from the module
  folder plus the inline module names, the same as rustc.
- A large valid source file no longer counts as too deep. The parse stack is
  sized from the nesting depth of the file, not from its size.
- The JSON `severity` of a finding is now `warn`, not `warning`. The fallow
  JSON output and the shared `GateStatus` use `warn`. SARIF keeps its own
  `warning` level.
- A `threshold` in `[workspace.metadata.fallow.dupes]` no longer fails the run
  on its own. The `duplication-threshold` gate fails the run only with its
  flag, the same as each other gate.
- `cargo fallow visibility` reports no finding in a generated file.
- The `unit_size` penalty of the health score reached its cap on six of
  seven corpus workspaces. Its weight is now 0.2, not 0.5, and
  `formula_version` is 2. `docs/health-score.md` has the data.
- A `check` run without one of the six checks of the health score, for
  example with `--only deps,visibility`, no longer reports a high score. The
  report has no score and no grade, and `health.missing_checks` names the
  checks that the run did not include.
