### Fixed

- `cargo fallow visibility` no longer reports a type that the signature of an
  item that stays `pub` names, for example the return type of a `#[napi] pub
  fn`. Before, the `pub(crate)` change made rustc warn with
  `private_interfaces`.
- `cargo fallow visibility` gives credit to an intra-doc link. The link is a
  use from the module of the doc comment, so `fix` no longer breaks
  `cargo doc`. The trace shows the use as `doc-link`.
- `cargo fallow deps` counts an intra-doc link to a dependency as a use.
