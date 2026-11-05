### Added

- `cargo fallow visibility` has the `--public-crate` flag and the
  `public-crates` key of `[workspace.metadata.fallow.visibility]`. When they
  name libraries, only those libraries are an external API. Each other
  library gets each rule, also when a registry can publish it. A name can
  have `*` as a glob. fallow for TypeScript names the same option
  `publicPackages`.
