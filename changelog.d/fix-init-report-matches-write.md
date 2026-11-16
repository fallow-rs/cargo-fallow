### Fixed

- `cargo fallow init` now prints the `[workspace.metadata.fallow]` tables that
  `--write` gives, with the keys that `Cargo.toml` already has, in the order
  of the file. Before, the output left out the existing keys.
- The `baseline_findings` field of `init` is now the number of entries in the
  baseline file. Before, it was the number of findings, and a clone group of
  `dupes` has one entry for all its copies.
- `cargo fallow init` now reports the `baseline` path that `Cargo.toml`
  configures. Before, the output without `--write` always gave
  `fallow-baseline.json`.
- `cargo fallow init` without `--write` now exits `2` for a manifest that
  `--write` cannot edit, or for a `baseline` key that `--write` rejects.
  Before, it printed tables that `--write` could not write.
