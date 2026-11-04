# Unused files

`cargo fallow files` reports each `.rs` file in a target folder of a package
that no target compiles. The compiler never reads such a file, so its code is
dead and no other check sees it.

```bash
cargo fallow files
cargo fallow files --format json --fail-on-issues
```

A file counts as used when one of these reaches it from a target root:

- a `mod name;` item, also in an inline module and in the tokens of a macro
  call such as `cfg_if!`,
- a `#[path = "..."]` attribute or a `path` value in a `cfg_attr` attribute,
- a literal `include!`, `include_str!` or `include_bytes!` call,
- a Cargo target: a library, a binary, a test, a benchmark, an example or a
  build script.

The check gives credit when the syntax cannot decide:

- It reads only `src/`, `tests/`, `benches/`, `examples/` and the folder of
  each target root that is not the package folder.
- It skips a folder with no reached `.rs` file, for example the inputs of a
  compile test. So a folder of old modules with no `mod` item is not reported:
  only the file that names the folder is.
- It skips each folder with one of these names: `fixture`, `fixtures`,
  `testdata`, `test-data`, `test_data`, `data`, `snapshots`, `ui`,
  `compile-fail`, `compile_fail`, `corpus`, `samples`, `inputs` and
  `resources`.
- It skips a folder with its own `Cargo.toml`, a hidden folder, `target`, and
  a file with a generator header or an `@generated` comment.
- A `.rs` string literal in a macro call counts as an include, because a
  macro can give it to `include_str!`.
- It skips each file of a package when the syntax cannot follow the code: a
  computed `include!` path (an `OUT_DIR` path is not a source file), a file
  that `include!` reads and that declares a module, a `path` attribute on an
  inline module, a module in a function body, a module form in a macro that
  the scan cannot read, or a file that does not parse. A macro that declares a
  module from its input skips each package of the workspace.
  `summary.skipped_packages` names each such package with the reason.

The check cannot see a module that a macro of another crate, for example a
dependency, declares. Add such a file to `exclude`.

Each finding has the rule `unused-file`, the `warning` severity and a
`delete-file` action with `auto_fixable: false`. `--fail-on-issues` enforces
the `unused-files` gate. The health score does not change.

```toml
[workspace.metadata.fallow.files]
exclude = ["crates/app/src/scratch.rs"]
```

`exclude` lists files and folders that the check skips.
