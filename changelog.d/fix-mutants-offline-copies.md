### Fixed

- `cargo fallow mutants` now gives `--locked`, `--offline` and `--frozen` to
  each cargo call in the copies: the builds, the test lists, the doctests and
  the `cargo metadata` calls. Before, only the first `cargo metadata` call got
  the flag, so a build in a copy could use the network. With `--offline` or
  `--frozen`, these calls and the test runs also get `CARGO_NET_OFFLINE=true`.
