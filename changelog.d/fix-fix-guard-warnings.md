### Fixed

- `cargo fallow fix` leaves out an edit that adds a compiler warning, the same
  as an edit that adds an error. Before, a workspace that builds with
  `-D warnings` could break after `fix` exited 0. A new `dead_code` warning
  does not count, because a `dead-public` edit shows the dead code on
  purpose.

### Added

- `cargo fallow fix` formats each changed file with `rustfmt` when `rustfmt`
  formatted it before the edits, so `cargo fmt --check` still passes.
