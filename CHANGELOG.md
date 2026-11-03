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

### Fixed

- A large valid source file no longer counts as too deep. The parse stack is
  sized from the nesting depth of the file, not from its size.
- The JSON `severity` of a finding is now `warn`, not `warning`. The fallow
  JSON output and the shared `GateStatus` use `warn`. SARIF keeps its own
  `warning` level.
