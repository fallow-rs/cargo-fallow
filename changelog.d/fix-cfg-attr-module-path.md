### Fixed

- The module graph and `cargo fallow mutants` now read each file that a
  `#[cfg_attr(cond, path = "x.rs")] mod name;` declaration can load. Before,
  `deps` reported a dependency that only a platform file uses, and
  `visibility` did not see the uses in such a file.
