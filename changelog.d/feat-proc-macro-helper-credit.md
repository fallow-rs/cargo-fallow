### Added

- `cargo fallow visibility` now counts a name in the `quote!`,
  `quote_spanned!`, `parse_quote!` and `parse_quote_spanned!` tokens of a
  helper crate of a proc-macro crate. A helper crate is a workspace library
  in the normal dependencies of the proc-macro crate or of another helper
  crate.
  The name counts in the crates that can call the macro, as for the
  proc-macro crate itself. A dev-dependency, a build-dependency and the test
  code of a helper crate give no credit. A name in a string that
  `syn::parse_str` or `TokenStream::from_str` parses still gives no credit.
