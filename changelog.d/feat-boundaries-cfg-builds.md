### Added

- `[[workspace.metadata.fallow.boundaries.cfg]]` rules list `cfg` builds,
  for example `builds = [["test"], ["test", "miri"]]`. For each build, the
  rule reports a path from code that the build compiles to an item that the
  build does not compile. The kind is `cfg-path`. An option that no build
  names is unknown, and then the rule gives credit.
