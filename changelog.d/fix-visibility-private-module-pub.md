### Fixed

- `cargo fallow visibility` no longer reports `unnecessary-public` for a
  `#[cfg(test)] pub` item in a private module. There, `pub` already gives
  crate visibility, and `clippy::redundant_pub_crate` rejects `pub(crate)`.
  When only its own module uses the item, the check reports
  `unnecessary-restricted-visibility`, so `fix` makes it private.
