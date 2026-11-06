# Unused dependencies

`cargo fallow deps` reports the dependencies in `Cargo.toml` that no code of
the package uses. For each target, the command reads the first segment of
these names:

- paths, `use` items and `extern crate` items,
- macro paths and attribute paths.

The command also reads these names in macro tokens, attributes, `cfg_attr`,
doc tests and intra-doc links. rustdoc needs the dependency of a link such as
`` [`other_crate::Item`] ``, so the link is a use.

The command reads each file of the module tree. A `mod name;` declaration
with `path` values in `cfg_attr` attributes adds each file that a `path`
names, and the default file of the name. The command does not evaluate the
conditions, so a use in each platform file counts. A file whose condition
holds only in a test build, such as `cfg_attr(test, path = "mock.rs")`, is
test code.

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

To resolve the dependencies, the command runs `cargo metadata` with all
features and without `--no-deps`. The resolve gives the library name and the
`links` key of each dependency. Cargo can use the network for the resolve. To
prevent network access, use `--offline` or `--frozen`. Each command that
reads the workspace accepts the three flags, but only `deps`, `check`, `fix`
and `init` resolve the dependencies.

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
| `unused-feature` | warn | A feature with an empty list in a `publish = false` package, that no other feature, no member, no target and no file names. See [Unused features](#unused-features). |
| `feature-unification-drift` | error | A crate that `cargo build -p <member>` and `cargo build --workspace` compile with different features, for a member in `unify-features`. See [Feature drift](#feature-drift). |

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

The command also reads `ignored` in `[package.metadata.cargo-shear]`,
`[workspace.metadata.cargo-shear]`, `[package.metadata.cargo-machete]` and
`[workspace.metadata.cargo-machete]`. Thus a workspace can move from
cargo-shear or cargo-machete with no change to its `ignored` lists. The
`redundant-ignore` rule checks only the cargo-fallow lists.

## External macros

A macro of a crate outside the workspace can write paths of other crates. For
example, `#[gpui::bench]` writes `criterion::` paths into the bench. The
command cannot read that output. It still reports such an entry when no code
names it, but `cargo fallow fix` does not remove it in these cases:

- The library or a binary uses an attribute macro of an external crate by a
  path, for example `#[krate::name]`, or calls its macro in the place of an
  item, for example `krate::name!(..);` in a module. Then the unused
  `[dependencies]` entries stay for a person.
- Test code, a bench or an example does the same. Then the unused
  `[dev-dependencies]` entries stay for a person. Code under a `cfg` that
  holds only in a test build, for example `all(test, feature = "x")`, is test
  code here.

A macro call in an expression, for example `log::info!(..)`, a derive and a
macro that a `use` item imports do not count. The macros of `async_trait`,
`bitflags`, `ctor`, `lazy_static`, `pin_project`, `serde`, `thiserror`,
`tokio` and `tracing` write only paths of their own crate and of the
standard library, so they do not count either.

## Feature drift

Cargo resolves features for each build. When one member turns on a feature
of a crate that another member also uses, `cargo build -p <member>` and
`cargo build --workspace` compile two variants of that crate and of each crate
that depends on it. A switch between the two commands then compiles the crates
again.

The `feature-unification-drift` rule finds these crates. It is opt-in: name
the members to compare in `unify-features`.

```toml
[workspace.metadata.fallow.deps]
unify-features = ["my-cli"]
```

For each named member, the command runs `cargo tree -p <member>` and once
`cargo tree --workspace`, with the normal and the build edges. Each crate that
both builds compile with different features is a finding on the manifest of
the member. The finding has `version`, `workspace_only_features` and
`member_only_features`. The fix is a feature-only entry in the
`[dependencies]` of the member with the workspace-only features, so `fix`
does not change it. Features that a dev-dependency adds for `cargo test` are
out of scope, and so are the dependencies of another platform: `cargo tree`
reads the host platform. When the member declares the crate in
`[dependencies]` or `[build-dependencies]`, the finding has the line of that
entry, so a `# fallow-ignore-next-line feature-unification-drift` comment
can name one crate. A crate that the member does not declare has no line;
only a `# fallow-ignore-file` comment hides it. A name that is no workspace member exits 2 with
`CARGO_FALLOW_INVALID_CONFIG`. When `cargo tree` fails, for example offline
with a missing crate, the member is in `skipped` with the reason.

## Unused features

The `unused-feature` rule reports a feature in `[features]` with an empty
list, for example `fast = []`, that nothing names. Turning on such a feature
changes nothing. The finding has `section: "features"`, the feature name in
`dependency`, and the line of the key.

The rule judges only the features that no user outside the workspace can
turn on: the features of a package with `publish = false`. A package that a
registry can publish gets no finding, because users can turn on its
features with a dependency entry or with `cargo install --features`. A
library in `exclude-crates` of `[workspace.metadata.fallow.visibility]` gets
no finding either. A feature with a value, for example `["dep:serde"]` or
`["log/std"]`, turns on a dependency or a feature, so the rule does not
report it.

A feature gets credit when one of these names it:

- `default`, or another feature of the package,
- a dependency entry of a member, in `features = [...]`, or a feature of a
  member as `key/feature` or `key?/feature`,
- `required-features` of a target,
- the name as a whole word in any file under the repository root (the
  nearest folder with `.git`), also in a comment or a string. So a `cfg`, a
  build script that writes code, a CI workflow, a script and a manifest
  outside the workspace all count. The `[features]` tables of the members and
  the ignore lists of the manifests do not count,
- `CARGO_FEATURE_<NAME>` in a file. When a file reads `CARGO_FEATURE` with no
  name, no package with a build script gets a finding.

The scan runs only when a candidate exists. It reads ignored and untracked
files too. It skips `.git`, `node_modules`, `Cargo.lock` and each Cargo target
folder (a folder with `CACHEDIR.TAG`). It does not follow a symlink to a
folder outside the repository. When the scan cannot read a file or a folder,
the rule reports nothing.

The rule cannot see a user who builds the package from a git URL or a path,
for example with `cargo install --git`, or a name in documents outside the
repository. Add the name to `ignored` in `[package.metadata.fallow.deps]` to
keep such a feature.
