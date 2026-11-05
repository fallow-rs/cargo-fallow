### Changed

- `cargo fallow init --ci github` writes `fallow-rs/cargo-fallow@v0` in the
  workflow. Before, it wrote `@main`, which runs the action of the newest
  commit and not of a release. `v0` is the newest 0.x release.
