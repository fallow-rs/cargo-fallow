# Visibility

`cargo fallow visibility` reports items whose visibility is larger than their
uses need. The command treats the workspace as the only user of its
libraries, with two exceptions:

- A library that a registry can publish: `publish` is not `false` in its
  manifest. Users outside the workspace can use its exported items.
- A library in `--exclude-crate` or `exclude-crates`.

When `--public-crate` or `public-crates` names libraries, only those
libraries and the excluded libraries are an external API. Each other library
is internal, also when a registry can publish it. Use this when a workspace
publishes internal crates only because a published facade crate depends on
them. An empty `public-crates = []` makes each library internal. A name is a
crate name or a package name, and `*` matches any text, for example
`"my-api-*"`. The match is exact, so `my_api` and `my-api` differ: use the
form of the crate name or of the package name. A pattern that matches no
library is an error with the code `CARGO_FALLOW_INVALID_CONFIG`. The flag
replaces the list of the key. `check`, `fix`, `api` and the `unused-feature`
rule of `deps` read the key. fallow for TypeScript has the same option as
`publicPackages`.

The exported items of these libraries are an external API. They are entry
points: they get no `dead-public` or `unnecessary-public` finding, `fix` does
not narrow them, and each item that they use is live. The `pub(crate)`,
`pub(super)` and `pub(in path)` items of a published library still get the
`unnecessary-restricted-visibility` rule. An excluded library gets no
finding. A binary and a library with `publish = false` keep each rule. When a library is not for a registry, set
`publish = false` in its manifest, so that the command checks its public API.

| Kind | Meaning | Change |
| --- | --- | --- |
| `dead-public` | A `pub` item that is not reachable from a root. | Remove the item, or make it `pub(crate)` to let the compiler confirm the dead code. |
| `unnecessary-public` | A `pub` item that only its own crate uses. | `pub` to `pub(crate)`. |
| `unnecessary-restricted-visibility` | A `pub(crate)`, `pub(super)` or `pub(in path)` item that only its own module and its child modules use. A `#[cfg(test)] pub` item that other crates cannot name gets this rule too. | Remove the keyword. |

Each finding has the `error` severity, because it counts for the gate of
the command. See [Rule severity](adoption.md#rule-severity).

The rule ids are kebab-case in each output format. The snake_case form of
hawk, for example `dead_public`, names the same rule in the rules table, in
`kinds`, in a suppression comment and in `cargo fallow explain`. The finding
ids did not change, so a saved baseline still matches.

```bash
cargo fallow visibility
cargo fallow visibility --format json --quiet --kind dead-public
cargo fallow visibility --production my-cli:my-cli --exclude-crate my_api --fail-on-issues
cargo fallow visibility --precise
```

| Flag | Meaning |
| --- | --- |
| `--production <PACKAGE:BIN>` | A production binary. The flag can repeat. Without the flag, each binary is a production binary. Tests, benches, examples and doctests are roots of a different kind. An item that only these roots use has `test_only: true`. |
| `--exclude-crate <CRATE>` | A library whose public items are an external API. The flag can repeat. |
| `--public-crate <CRATE>` | A library whose exported items are an external API. Each other library is internal. The flag can repeat. |
| `--uniform-fields` | When all fields of a struct have the same written visibility, the command changes them as a group. |
| `--kind <KIND>` | Report only this kind: `dead-public`, `unnecessary-public` or `unnecessary-restricted-visibility`. The flag can repeat. |
| `--fail-on-issues` | Exit 1 when the run has a finding. |
| `--locked`, `--offline`, `--frozen` | Give the flag to `cargo metadata`. |
| `--changed-since <REF>` | Report only the findings in the files that changed since the fork point of `REF`. See [Changed files](adoption.md#changed-files). |
| `--save-baseline <PATH>`, `--baseline <PATH>`, `--fail-on-stale-baseline` | Save the findings, or leave out the saved findings. See [Baselines](adoption.md#baselines). |
| `--package <NAME>`, `-p <NAME>` | Report only the findings of this workspace member. The flag can repeat. See [Packages](adoption.md#packages). |
| `--precise` | Decide the undecided candidates with rust-analyzer. See below. |
| `--trace <CRATE::PATH>` | Show each use and each credit that keeps one item, instead of the report. See [Trace](trace.md). |

You can set the same options in the `[workspace.metadata.fallow.visibility]`
table of the root `Cargo.toml`. A flag has priority over the table. A list
flag replaces the list of the table, and `--uniform-fields` turns the option
on. If the table has an unknown key, the run stops with exit code 2 and the
code `CARGO_FALLOW_INVALID_CONFIG`.

```toml
[workspace.metadata.fallow.visibility]
production = ["my-cli:my-cli"]
exclude-crates = ["my_api"]
public-crates = ["my-api", "my-types"]
uniform-fields = true
kinds = ["dead-public", "unnecessary-public"]
```

The command uses the syntax and the module graph of the workspace. It counts
the items below as used and does not report them. For most of them, a use can
exist that cargo-fallow cannot find without type information or macro
expansion:

- each item with the name of a method call or a field access, because the
  type of the receiver is unknown,
- each item with a name in macro tokens, in an attribute or in a
  documentation code block,
- items of trait impls, `#[macro_export]` macros and proc-macro entry points,
- items under an attribute macro, for example the fields of a
  `#[napi(object)]` struct,
- items under `allow(dead_code)` or `expect(dead_code)`, also when the
  attribute is on an enclosing item, module or file,
- items behind a glob re-export.

Two other uses keep an item at its visibility, because the change would add
a warning:

- A type in the signature of an item that stays `pub`. Parameter and return
  types, field types, bounds, aliases and the members of a trait impl count.
  rustc warns with `private_interfaces` when such a type is less visible
  than the item. For example, an attribute macro keeps a `#[napi] pub fn`
  public, so its return type stays `pub` too.
- The target of an intra-doc link: `` [`path`] ``, `[text](path)`,
  `[text][path]` and `[label]: path`. The link is a use from the module of
  the doc comment. When the documented item stays `pub`, the target stays
  `pub` too, because rustdoc warns with `private_intra_doc_links`. A link
  makes no item live, so an item that only a link names can still be
  `dead-public`. `Self::` in a link names the self type of the `impl` block
  or the documented type.
- A type that macro tokens name, when the macro can write a `pub` item: the
  tokens of the `macro_rules!` body or of the call hold `pub` or a `vis`
  fragment. The output of the macro can hold a `pub` item that names the
  type.

The command ignores `cfg` conditions. It reads the code of each branch.

The command reports no finding in a generated file, because a code generator
sets the visibility in that file. A file is generated when a comment in its
first ten lines has one of these texts:

- `@generated`
- `AUTOGENERATED FILE. DO NOT EDIT.`
- `Generated by <x>. Do not edit.` or `Generated from <x>. Do not edit.`

The items of a generated file still count as uses of other items. A file in
`OUT_DIR` is not in the source tree, so the command never reports it.

The command cannot read the file of an `include!` call when the path is not
a literal, for example `include!(concat!(env!("OUT_DIR"), "/gen.rs"))`. The
included code can name the items of its own crate and the `pub` items of the
dependencies. The command puts those items in `undecided`, not in `findings`.
If the package has a build script, the command uses only the dependencies
that the build script names as `name::`. `cargo fallow deps` uses the same
rule.

## Output

The JSON report has `schema_version`, `version`, `kind` (`visibility`),
`language`, `mode` (`fast` or `precise`), `total_issues`, `summary`,
`gate_outcomes`, `precise`, `graph`, `crates`, `findings` and `undecided`.

Each finding has an `actions` array. The first action has the type
`change-visibility`. The action gives the written keyword (`from`), the new
keyword (`to`, an empty text for private) and the `range` of the keyword. An
enum variant has no keyword, so its action has the type `remove-item`. A
`dead-public` finding has a second action of type `remove-item`.

A `change-visibility` action of a finding has `auto_fixable: true`, and
`cargo fallow fix` applies it (see [Fix](fix.md#fix)). These actions have
`auto_fixable: false`:

- a `remove-item` action;
- the action of a `dead-public` finding and of a finding with
  `test_only: true`, because a smaller keyword makes the compiler report dead
  code. Put `#[cfg(test)]` on an item that only tests use;
- the action of each undecided candidate.

`undecided` lists the candidates that are used only by a name match without a
type. These candidates are not findings. With `--format sarif`, the command
writes one result for each finding. The level of a result follows the
severity of its finding: `error` by default, `warning` for a rule at `warn`.
The gate is `visibility-findings`.

## Precise mode

`--precise` checks only the undecided candidates. These are method calls on
a type that the syntax does not show, and names in macro output. A worker
process loads the workspace into rust-analyzer and runs the build scripts and
the proc-macros. Then the worker uses name resolution and type inference to
find the uses of each candidate.

To use precise mode, build cargo-fallow with the `precise` feature. The
feature adds about 150 crates to the build.

```bash
cargo install --path . --locked --features precise
```

The worker uses the proc-macro server in the sysroot of the active toolchain.
It reads the standard library source from the `rust-src` component. The
default rustup profile does not install `rust-src`, so add it:

```bash
rustup component add rust-src
```

If the standard library source is missing, or a proc-macro crate does not
load, rust-analyzer does not find all uses. In that case the run stops with
exit code 2 and the code `CARGO_FALLOW_PRECISE_FAILED`.

The worker runs `cargo check` in the `rust-analyzer` folder of the target
folder. Thus the worker does not wait for the build lock of your own builds.

| Flag | Default | Meaning |
| --- | --- | --- |
| `--precise-timeout <SECONDS>` | 1200 | The time limit of the worker. |
| `--precise-memory-limit <MIB>` | 8192 | The memory guard stops the worker when it uses more memory than this. |
| `--precise-file-limit <KIB>` | 1024 | The largest file that the worker searches with name resolution. When the name of a candidate is in a larger file, the worker does not resolve the name there, and the candidate is not reported. |

rust-analyzer loads the workspace with all features and with tests. It does
not analyze code under a `cfg` that is off in that build, for example a
condition for another target. A candidate with a use under such a `cfg` stays
undecided. rust-analyzer does not search the target folder either. Thus a
candidate stays undecided when code from `OUT_DIR` can name it.

A limit or a failure can stop the worker before it checks all candidates.
The report has the results that the worker found, and the other candidates
stay undecided. In that case:

- `precise.status` is `failed`, `time_limit` or `memory_limit`.
- The command writes a warning to stderr.
- The SARIF log has `executionSuccessful: false` and a notification with the
  id `visibility-precise`.
- With `--fail-on-issues`, the run stops with exit code 2 and the code
  `CARGO_FALLOW_PRECISE_FAILED`, because the worker checked only part of the
  candidates and the gate result would be incomplete.

`precise.elapsed_ms`, `precise.load_ms` and `precise.search_ms` are run times,
so their values change from run to run. The other fields of the JSON report
are the same in each run on the same input.
