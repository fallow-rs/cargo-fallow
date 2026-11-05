### Added

- `consumers` of `[workspace.metadata.fallow.visibility]` lists the manifests
  of separate workspaces whose code uses the items of the workspace, for
  example `fuzz/Cargo.toml`. A use in their code counts, so `visibility` does
  not report an item that a fuzz target calls, and `fix` does not narrow it.
  Without the key, `fuzz/Cargo.toml` is a consumer when it exists. The `fix`
  guard also checks each consumer workspace.
