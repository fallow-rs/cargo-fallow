### Fixed

- `cargo fallow api --base <REF>` now reads `public-crates` of
  `[workspace.metadata.fallow.visibility]`. When the key is set, the command
  reads only the libraries that `public-crates` and `exclude-crates` name, the
  same libraries that `visibility` treats as an external API. A narrowed item
  of an internal crate is then no `removed` finding.
