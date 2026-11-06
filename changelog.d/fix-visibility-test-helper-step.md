### Fixed

- `cargo fallow visibility` gives the keyword change as the step of an
  `unnecessary-restricted-visibility` finding on an item that exists only in
  test builds, for example a `#[cfg(test)] pub` helper or an item in a
  `#[cfg(test)]` module. `cargo fallow fix` applies it. The step no longer
  tells you to put `#[cfg(test)]` on an item that already has it. Such a
  finding has `test_only: false`, and the human output no longer adds
  "(only tests use it)".
