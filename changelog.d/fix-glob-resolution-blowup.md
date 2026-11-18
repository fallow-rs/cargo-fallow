### Fixed

- The name resolution no longer runs out of memory or stack on a module that
  declares many child modules with glob imports in a cycle, as the generated
  bindings of `web-sys` do. Each glob import now resolves its path one time
  before the first lookup. `cargo fallow deps` and `cargo fallow visibility`
  now finish on the `wasm-bindgen` workspace.
