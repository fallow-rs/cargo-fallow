### Fixed

- `cargo fallow fix` no longer stops the whole run for one edit that adds a
  compiler warning or error. It leaves out that edit, applies the others and
  exits 0. Each left-out edit has a `skip_reason` (`compiler_error` or
  `compiler_warning`) and the compiler message in `diagnostic`. A message that
  names no item leaves out the edit at a line of its spans, or else the edits
  of its file.
- The `fix` guard now counts a new `dead_code` warning, so an edit that makes
  the compiler report dead code is left out.

### Changed

- A `dead-public` finding and a finding with `test_only: true` are no longer
  `auto_fixable`. A smaller keyword on such an item makes the compiler report
  dead code. A `dead-public` finding has a second action, `remove-item`.
