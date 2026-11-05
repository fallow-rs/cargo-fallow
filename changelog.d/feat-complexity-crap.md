### Added

- `cargo fallow complexity --coverage <LCOV>` and `cargo fallow check
  --coverage <LCOV>` give each function a CRAP score from its cyclomatic
  complexity and its line coverage. A function at or over
  `[workspace.metadata.fallow.complexity] max-crap` (default 30) is a
  `complexity-crap` finding. `--coverage-root <PATH>` maps the `SF` paths of
  an LCOV file from another machine.
