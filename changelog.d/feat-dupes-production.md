### Added

- `[workspace.metadata.fallow.dupes] production = true` and
  `cargo fallow dupes --production` leave test code out of the clone search.
  Test code is a test target, a test module and an item with a test
  attribute, for example `#[cfg(test)]` or `#[test]`. The line counts and the
  `threshold` then count only production code. The default stays `false`.
  `--no-production` keeps test code.
