# Unused dependencies

`cargo fallow deps` reports the dependencies in `Cargo.toml` that no code of
the package uses. It reads the first segment of each path, `use`, `extern
crate`, macro path and attribute path in each target, also in macro tokens,
attributes, `cfg_attr` and doc tests.

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
| `--trace <DEPENDENCY>` | Show each declaration and each use site of one dependency, instead of the report. See [Trace](trace.md). |

The command resolves the dependencies: it runs `cargo metadata` with all
features and without `--no-deps`, so it gets the library name and the `links`
key of each dependency. Cargo can use the network for this. Use `--offline` or
`--frozen` to prevent it. The command keeps `Cargo.lock` as it was before the
run: it removes a lock file that Cargo created and writes back the old bytes
of a lock file that Cargo changed. All features are on, so a feature that
needs a dependency that Cargo cannot get fails the run with exit code 2.

| Rule | Severity | Meaning |
| --- | --- | --- |
| `unused-dependency` | error | No code of the package uses the `[dependencies]` entry. |
| `unused-dev-dependency` | error | No test, bench, example, doc test or `cfg(test)` code uses the entry. |
| `unused-build-dependency` | error | The build script does not use the entry. |
| `misplaced-dependency` | error | Only tests, benches, examples or doc tests use the `[dependencies]` entry. `used_by` names them. |
| `unused-workspace-dependency` | error | No member inherits the `[workspace.dependencies]` entry, or each member entry that inherits it is unused. |
| `unused-optional-dependency` | warn | No code uses the optional dependency. A feature enables it. |
| `feature-only-dependency` | warn | No code uses the dependency, and a feature sets its features (`dep/feature`). |
| `redundant-ignore` | warn | An ignore entry hides no finding. |

Each finding has `package`, `manifest_path`, `line`, `section`, `platform`,
`dependency` (the key in `Cargo.toml`), `package_name` (for a rename) and an
`actions` array. An action with `auto_fixable: true` is one that
`cargo fallow fix` applies. A warning, and an `error` finding that a person
must decide (see [Fix](fix.md#fix)), has `auto_fixable: false`. Only
`error` findings count for the `error-severity-findings`
gate, and only `--fail-on-issues` enforces it.

The check stays silent where the code can use a dependency without a name:

- A dependency with a `links` key gets no error, because it can link a native
  library. An optional one gets the warning.
- `include!("path")` with one string literal adds the code of that file, and
  the check reads it like the other code. Another `include!`, for example
  `include!(concat!(env!("OUT_DIR"), "/gen.rs"))`, can add code that the check
  cannot read. When the package has a build script, that code can use each
  `name::` in the text of the build script and the runtime crate of an
  `X-build` or `X-codegen` build dependency. Without a build script, it can
  use each dependency. A literal path that the check cannot read or parse
  gets the same credit.
- A package with a file that does not parse is not judged. The report lists
  it in `skipped`.
- A dev-dependency that repeats a normal dependency is not reported. It often
  turns on more features for the tests.
- A package with `trybuild`, `macrotest`, `ui_test` or `compiletest_rs` in
  `[dev-dependencies]` gets no dev-dependency findings. These harnesses build
  test files that no target declares.
- When code uses a macro crate `X_derive`, `X_derives`, `X_macro`,
  `X_macros`, `X_impl`, `X_internal` or `X_attributes`, the crate `X` is in
  use too, because the macro output names it (`serde_derive` and `serde`).
  Two pairs with other names are known: `num-derive` gives credit to
  `num-traits`, and `serde_repr` gives credit to `serde`. For another pair,
  add the runtime crate to the `ignored` list.
- Every `cfg` branch counts, so a `[target.'cfg(..)'.dependencies]` entry that
  `#[cfg(..)]` code uses is in use.

A crate that is in `Cargo.toml` only to turn on a feature of another
dependency looks unused. Add its name to an ignore list:

```toml
[package.metadata.fallow.deps]
ignored = ["chrono"] # unifies the features with a workspace build

[workspace.metadata.fallow.deps]
ignored = ["getrandom"]
```

The command also reads `ignored` in `[package.metadata.cargo-shear]` and
`[workspace.metadata.cargo-shear]`, so a workspace can move from cargo-shear
without a change. Only the cargo-fallow lists get `redundant-ignore` findings.
