### Fixed

- `cargo fallow api --base <REF>` now reads the same libraries that
  `visibility` treats as an external API. When `public-crates` of
  `[workspace.metadata.fallow.visibility]` is set, the command reads only the
  libraries that `public-crates` and `exclude-crates` name. A narrowed item of
  an internal crate is then no `removed` finding.

### Added

- `cargo fallow api --public-crate <CRATE>` replaces `public-crates` for one
  run. `--all-published` reads each published library and ignores the table.
