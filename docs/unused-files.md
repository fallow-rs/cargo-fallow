# Unused files

`cargo fallow files` reports each `.rs` file in a target folder of a package
that no target compiles. The code in such a file is dead, because the
compiler does not read it. The other checks do not analyze the file either.

```bash
cargo fallow files
cargo fallow files --format json --fail-on-issues
```

The command has the flags of the [structure checks](structure.md), and
`--fail-on-issues`.

The command starts at each target root and follows these links to other
files. A file that the command gets to is a used file:

- a `mod name;` item, also in an inline module and in the tokens of a macro
  call such as `cfg_if!`,
- a `#[path = "..."]` attribute or a `path` value in a `cfg_attr` attribute,
- a literal `include!`, `include_str!` or `include_bytes!` call,
- a Cargo target: a library, a binary, a test, a benchmark, an example or a
  build script.

In some cases, cargo-fallow cannot tell from the syntax if a target uses a
file. The command does not report a file in these cases:

- The command reads only `src/`, `tests/`, `benches/` and `examples/`. It
  also reads the folder of each target root, when that folder is not the
  package folder.
- The command reports a file only when a used `.rs` file is in the same
  folder. Thus it skips the inputs of a compile test. It also skips a folder
  of old modules that no `mod` item names. For such a folder, the command can
  report only the file that names the folder, for example `old.rs` for
  `old/`.
- The command skips each folder with one of these names: `fixture`,
  `fixtures`, `testdata`, `test-data`, `test_data`, `data`, `snapshots`, `ui`,
  `compile-fail`, `compile_fail`, `corpus`, `samples`, `inputs` and
  `resources`.
- The command skips a folder with its own `Cargo.toml`, a hidden folder and
  `target`. It also skips a file with a generator header or an `@generated`
  comment.
- A `.rs` string literal in a macro call counts as an include, because a
  macro can give the literal to `include_str!`.
- The command skips each file of a package when the syntax cannot show which
  files the code uses. These are the cases:
  - a computed `include!` path, except a path in `OUT_DIR`, because that
    path is not a source file,
  - a file that `include!` reads and that declares a module, includes a file
    or does not parse,
  - a `path` attribute on an inline module,
  - a module in a function body or a block,
  - a module form in a macro that the command cannot read,
  - a file that does not parse.
- Some macros can run in each crate of the workspace. If a macro declares a
  module with a name from its input, or gives its input to an `include!`
  call, the command skips each package of the workspace.

`summary.skipped_packages` names each skipped package and gives the reason.

The command cannot find a module that a macro of another crate declares, for
example a macro of a dependency. Add such a file to `exclude`.

Each finding has the rule `unused-file`, the `error` severity and a
`delete-file` action with `auto_fixable: false`. The `unused-files` gate fails
the run only when you give `--fail-on-issues`. The findings do not change the
health score.

```toml
[workspace.metadata.fallow.files]
exclude = ["crates/app/src/scratch.rs"]
```

`exclude` lists the files and folders that the command skips.
