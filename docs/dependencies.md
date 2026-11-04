# Unused dependencies

`cargo fallow deps` reports the dependencies in `Cargo.toml` that no code of
the package uses. For each target, the command reads the first segment of
these names:

- paths, `use` items and `extern crate` items,
- macro paths and attribute paths.

The command also reads these names in macro tokens, attributes, `cfg_attr`
and doc tests.

```bash
cargo fallow deps
cargo fallow deps --fail-on-issues --format sarif --quiet
```

| Flag | Meaning |
| --- | --- |
| `--manifest-path <PATH>` | The `Cargo.toml` of the workspace. |
| `--format human\|json\|sarif\|codeclimate\|compact\|markdown\|github-annotations\|github-summary` | The output format. The default is `human`. |
| `--quiet` | Hide the progress lines on stderr. |
| `--fail-on-issues` | Exit 1 when a finding has the `error` severity. |
| `--changed-since <REF>` | Report only the findings in the files that changed since the fork point of `REF`. See [Changed files](adoption.md#changed-files). |
| `--save-baseline <PATH>`, `--baseline <PATH>`, `--fail-on-stale-baseline` | Save the findings, or leave out the saved findings. See [Baselines](adoption.md#baselines). |
| `--package <NAME>`, `-p <NAME>` | Report only the findings of this workspace member. The flag can repeat. See [Packages](adoption.md#packages). |
| `--locked` | Give `--locked` to `cargo metadata`. |
| `--offline` | Give `--offline` to `cargo metadata`. |
| `--frozen` | Give `--frozen` to `cargo metadata`. |

To resolve the dependencies, the command runs `cargo metadata` with all
features and without `--no-deps`. The resolve gives the library name and the
`links` key of each dependency. Cargo can use the network for the resolve. To
prevent network access, use `--offline` or `--frozen`.

After the run, `Cargo.lock` is the same as before the run. If Cargo created a
lock file, the command removes it. If Cargo changed the lock file, the command
writes back the old bytes. Because all features are on, a feature can need a
dependency that Cargo cannot get. In that case the run fails with exit code 2.

| Rule | Severity | Meaning |
| --- | --- | --- |
| `unused-dependency` | error | No code of the package uses the `[dependencies]` entry. |
| `unused-dev-dependency` | error | No test, bench, example, doc test or `cfg(test)` code uses the entry. |
| `unused-build-dependency` | error | The build script does not use the entry. |
| `misplaced-dependency` | error | Only tests, benches, examples or doc tests use the `[dependencies]` entry. `used_by` names them. |
| `unused-workspace-dependency` | error | No workspace member inherits the `[workspace.dependencies]` entry, or each member entry that inherits it is unused. |
| `unused-optional-dependency` | warn | No code uses the optional dependency. A feature enables it. |
| `feature-only-dependency` | warn | No code uses the dependency, and a feature sets its features (`dep/feature`). |
| `redundant-ignore` | warn | An ignore entry hides no finding. |

Each finding has the fields `package`, `manifest_path`, `line`, `section`,
`platform`, `dependency` (the key in `Cargo.toml`), `package_name` (for a
rename) and `actions`. `cargo fallow fix` applies each action with
`auto_fixable: true`. Some `error` findings need a decision from a person (see
[Fix](fix.md#fix)). Their actions have `auto_fixable: false`, and so do the
actions of each warning.

The `error-severity-findings` gate counts the `error` findings only. The gate
fails the run only when you give `--fail-on-issues`.

Code can use a dependency without its name in the source. In these cases the
command counts the dependency as used, or does not report it:

- A dependency with a `links` key can link a native library, so it gets no
  `error` finding. The warning rules still apply to it, for example
  `unused-optional-dependency`.
- `include!("path")` with one string literal adds the code of that file. The
  command reads that code like the other code of the target.
- Another `include!` call, for example
  `include!(concat!(env!("OUT_DIR"), "/gen.rs"))`, can add code that the
  command cannot read. If the package has a build script, the command counts
  these dependencies as used in that target: each `name::` in the text of the
  build script, and the runtime crate of an `X-build` or `X-codegen` build
  dependency. If the package has no build script, the
  command counts each dependency as used. A literal `include!` path that the
  command cannot read or parse gets the same treatment.
- If a file of a package does not parse, the command does not check the
  dependencies of that package. The report lists the package in `skipped`.
- The command does not report a dev-dependency that repeats a normal
  dependency. Such an entry often turns on more features for the tests.
- If `[dev-dependencies]` has `trybuild`, `macrotest`, `ui_test` or
  `compiletest_rs`, the package gets no dev-dependency findings. These
  harnesses build test files that no target declares.
- If code uses a macro crate `X_derive`, `X_derives`, `X_macro`, `X_macros`,
  `X_impl`, `X_internal` or `X_attributes`, the crate `X` is in use too. The
  output of the macro names `X`, as `serde_derive` output names `serde`.
- Two macro crates have a runtime crate with a different name. A use of
  `num-derive` counts as a use of `num-traits`, and a use of `serde_repr`
  counts as a use of `serde`. For a different pair, add the runtime crate to
  the `ignored` list.
- The command reads each `cfg` branch. Thus a `[target.'cfg(..)'.dependencies]`
  entry is in use when `#[cfg(..)]` code uses it.

Some crates are in `Cargo.toml` only to turn on a feature of another
dependency. Such a crate looks unused. Add its name to an ignore list:

```toml
[package.metadata.fallow.deps]
ignored = ["chrono"] # unifies the features with a workspace build

[workspace.metadata.fallow.deps]
ignored = ["getrandom"]
```

The command also reads `ignored` in `[package.metadata.cargo-shear]` and
`[workspace.metadata.cargo-shear]`. Thus a workspace can move from cargo-shear
with no change to its `ignored` lists. The `redundant-ignore` rule checks only the
cargo-fallow lists.
