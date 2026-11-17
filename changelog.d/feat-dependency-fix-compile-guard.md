### Changed

- `cargo fallow fix` keeps an unused entry for a person only when the crate
  of an external macro and the entry depend on each other in the resolve.
  Before, any external macro in a target of the table kept the entry. For an
  entry that is now auto-fixable for this reason, the edit is guarded:
  `fix` runs `cargo check` before and after the edits, and leaves out the
  edit when a compiler error names the crate of the entry. Such an edit has
  `guarded: true`, and a left-out edit has a `skip_reason` and a
  `diagnostic`. When the workspace does not build before the edits, `fix`
  leaves out only the guarded edits, with the `skip_reason`
  `build_fails_before`. When it builds only without `--all-features`, the
  `skip_reason` is `all_features_fail_before`. An entry in a
  `[target.<platform>.*]` table is never guarded and stays for a person.

### Fixed

- The `cargo check` of `cargo fallow fix` now runs with `--all-features`
  first, as the docs say. Before, cargo rejected the flag, so the check
  always used the default features.
