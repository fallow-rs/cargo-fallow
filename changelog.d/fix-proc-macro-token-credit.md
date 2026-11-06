### Fixed

- `cargo fallow visibility` no longer reports `dead-public` for an item that
  only the output of a proc-macro uses. A name in the macro tokens of a
  proc-macro crate now counts in each crate that can call the macro, also
  through a re-export, and in the crates that it can use.
