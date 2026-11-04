# Changelog

All notable changes to `cargo-fallow` are in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The release workflow copies the section of the released version into the
GitHub release. Each release needs a `## [X.Y.Z] - YYYY-MM-DD` section.

## [Unreleased]

### Added

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
  bytes. The `error` findings of `deps` now have `auto_fixable: true`.
- `boundaries`, `size`, `complexity`, `dupes` and `mutants` accept
  `--fail-on-issues` as their gate flag. The older gate flags stay. The GitHub
  Action now gives `--fail-on-issues` to each command.

### Fixed

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
