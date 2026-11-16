### Fixed

- `cargo fallow init` now prints the `[workspace.metadata.fallow]` tables that
  `--write` gives, with the keys that `Cargo.toml` already has, in the order
  of the file. Before, the output left out the existing keys.
- The `baseline_findings` field of `init` is now the number of entries in the
  baseline file. Before, it was the number of findings, and a clone group of
  `dupes` has one entry for all its copies.
