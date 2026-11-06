### Added

- `cargo fallow deps` reads the `ignored` lists in
  `[package.metadata.cargo-machete]` and `[workspace.metadata.cargo-machete]`,
  as it does for cargo-shear. A workspace can move from cargo-machete with no
  change to its lists.
