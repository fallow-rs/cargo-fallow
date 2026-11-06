### Fixed

- `cargo fallow visibility` no longer reports `dead-public` for an item that
  only the output of a proc-macro uses. A name in the `quote!` tokens of a
  proc-macro crate now counts in each crate that can call the macro, also
  through a re-export, and in the crates that it can use. A name match of
  this kind puts the item in `undecided`, so some candidates with a common
  name move from `findings` to `undecided`.
