### Added

- `cargo fallow boundaries` has item rules
  (`[[workspace.metadata.fallow.boundaries.items]]`) and glob re-export rules
  (`[[workspace.metadata.fallow.boundaries.reexports]]`). An item rule keeps
  the code of some packages from a workspace item, with the path resolved
  through `use`, aliases and re-exports. `allow-via` keeps a use through a
  named module allowed. A glob re-export rule finds a `pub use <path>::*`
  that reads a denied crate or module. The findings have the kinds
  `item-path` and `glob-reexport`, and the summary counts the uses that do
  not resolve in `unresolved`.
