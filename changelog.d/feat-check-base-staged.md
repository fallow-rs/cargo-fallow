### Added

- `cargo fallow check` accepts `--base <REF>` and `--staged`, and gives them to
  the size check. The `file-growth` gate then runs, and `--fail-on-issues`
  enforces it, so one `check` run covers each gate.
