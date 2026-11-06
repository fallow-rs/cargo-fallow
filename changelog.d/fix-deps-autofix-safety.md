### Fixed

- `cargo fallow fix` no longer removes an unused dependency entry with an
  exact version (`=`) or an upper bound (`<`, `<=`, `~`) when another package
  in the resolve uses the same crate. The entry can pin the version for that
  package. When another entry that `fix` keeps has the same requirement,
  `fix` still removes the entry.
- `cargo fallow fix` no longer removes an unused dependency entry when a
  target of that table uses an attribute macro, or calls a macro in the place
  of an item, from a crate outside the workspace by a path. For example,
  `#[gpui::bench]` writes `criterion::` paths that the command cannot see.
  `deps` still reports the entry, and the action tells why `fix` keeps it.
