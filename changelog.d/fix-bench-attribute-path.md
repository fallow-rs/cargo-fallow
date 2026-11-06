### Fixed

- A function with an attribute macro named `bench` from a crate, for example
  `#[gpui::bench]`, is no longer a test. This applies to `cargo fallow deps`,
  `visibility`, `mutants`, `boundaries` and the other structure checks. Only
  `#[bench]` and `#[test::bench]` are the built-in bench attribute. Before,
  `deps` could report the crate of such a macro as "used only by unit-test".
  Its advice to move the crate to `[dev-dependencies]` can break the build.
