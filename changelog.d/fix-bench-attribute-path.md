### Fixed

- `cargo fallow deps`, `visibility`, `mutants` and the structure checks no
  longer treat a function with an attribute macro named `bench` from a crate,
  for example `#[gpui::bench]`, as a test. Only `#[bench]` and
  `#[test::bench]` are the built-in bench attribute. Before, `deps` could
  report the crate of such a macro as "used only by unit-test" and advise a
  move to `[dev-dependencies]`, which can break the build.
