### Fixed

- `cargo fallow visibility` no longer reports `dead-public` for an item that
  only the output of a proc-macro uses. A name in the `quote!` tokens of a
  proc-macro crate now counts in each crate that depends on the proc-macro
  crate. The name also counts in each crate that depends on a crate that
  re-exports the macro. These crates can call the macro. The name also counts
  in the workspace crates that these crates depend on. A name match of this
  kind puts the item in `undecided`. Some candidates with a common name thus
  move from `findings` to `undecided`.
- A name that the `quote!` output only defines, for example the name after
  `fn` or a field declaration, is no use. The tokens of a `quote!` in the
  test code of a proc-macro crate also count only in that crate.
- The command does not read names in a string for `syn::parse_str` or
  `TokenStream::from_str`. It also does not read the `quote!` tokens of a
  helper library crate. `docs/visibility.md` lists these limits.
