### Added

- `cargo fallow deps` has the opt-in `feature-unification-drift` rule. For each
  member that `unify-features` in `[workspace.metadata.fallow.deps]` names, it
  compares the features that `cargo build -p <member>` and
  `cargo build --workspace` turn on for each shared crate. Each finding names
  the crate, the version and the features that only one build turns on.
