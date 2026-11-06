### Fixed

- The module graph and `cargo fallow mutants` now read each file that a
  `#[cfg_attr(cond, path = "x.rs")] mod name;` declaration can load. Before,
  `deps` reported a dependency that only a platform file uses, and
  `visibility` did not see the uses in such a file.
- A file from `cfg_attr(test, path = "x.rs")` is test code. `deps` counts its
  uses as test uses, and `mutants` makes no mutants in it.
- `size`, `complexity`, `dupes` and `boundaries` also read these platform
  files. Their file counts can go up, and they can report new findings in
  these files.
