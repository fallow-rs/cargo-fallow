<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/fallow-rs/cargo-fallow/main/assets/logo-dark.svg">
    <source media="(prefers-color-scheme: light)" srcset="https://raw.githubusercontent.com/fallow-rs/cargo-fallow/main/assets/logo.svg">
    <img src="https://raw.githubusercontent.com/fallow-rs/cargo-fallow/main/assets/logo.svg" alt="cargo fallow" width="360">
  </picture>
</p>

<p align="center">
  <strong>Codebase intelligence for Rust workspaces.</strong><br>
  cargo-fallow finds unused dependencies, <code>pub</code> items that only their own crate uses, duplicated code, complex functions and boundary violations.<br>
  Its mutation tests show the code changes that your tests do not detect.
</p>

<p align="center">
  <a href="https://github.com/fallow-rs/cargo-fallow/actions/workflows/ci.yml"><img src="https://github.com/fallow-rs/cargo-fallow/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/fallow-rs/cargo-fallow/blob/main/Cargo.toml"><img src="https://img.shields.io/badge/rust-1.98%2B-8B7355.svg" alt="MSRV 1.98"></a>
  <a href="https://crates.io/crates/cargo-fallow"><img src="https://img.shields.io/crates/v/cargo-fallow.svg" alt="crates.io"></a>
  <a href="https://github.com/fallow-rs/cargo-fallow/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT License"></a>
</p>

<p align="center">
  <a href="docs/README.md">Docs</a> ·
  <a href="#install">Install</a> ·
  <a href="#in-your-terminal">Terminal</a> ·
  <a href="#in-pull-requests">Pull requests</a> ·
  <a href="#mutation-testing">Mutation testing</a> ·
  <a href="#with-scripts-and-coding-agents">Agents</a>
</p>

---

Run it in the root of any Cargo workspace:

```bash
cargo fallow
```

`cargo fallow` is the Rust version of [fallow](https://github.com/fallow-rs/fallow).
It reads `cargo metadata`, parses your source with `ra_ap_syntax` (the parser
of rust-analyzer), and builds a module graph of the workspace. The static
checks work from this graph and the syntax tree, so you do not have to
compile the workspace first.

> [!IMPORTANT]
> cargo-fallow is at version 0.x. The JSON output can change in a minor
> release. The [CHANGELOG](CHANGELOG.md) lists each change.

## What cargo-fallow finds

| Question | Check | Command |
|---|---|---|
| Which dependencies does no code use? | [Unused, misplaced and feature-only dependencies, and unused features](docs/dependencies.md), with an [auto-fix](docs/fix.md) that keeps the format of `Cargo.toml` | `cargo fallow deps`, `cargo fallow fix` |
| Which `pub` items are too visible? | [Dead public items and items that only their own crate uses](docs/visibility.md), with an optional rust-analyzer pass for method calls and macro output | `cargo fallow visibility` |
| Which `.rs` files does no target compile? | [`.rs` files outside the module tree of each target](docs/unused-files.md) | `cargo fallow files` |
| What is copied? | [Clone groups](docs/structure.md) on normalized token streams, in four modes from `strict` to `semantic` | `cargo fallow dupes` |
| Where is the code hard to change? | [Functions over the cyclomatic or the cognitive limit](docs/structure.md), a [CRAP score from LCOV coverage](docs/structure.md#crap-score), and a [0 to 100 health score](docs/check.md#health-score) | `cargo fallow complexity` |
| Do crates and modules follow your dependency rules? | [Crate, module and source rules](docs/structure.md) in `Cargo.toml` | `cargo fallow boundaries` |
| Do large files grow? | [Line limits for files and functions](docs/structure.md), and a growth gate against a base ref | `cargo fallow size` |
| Do the tests detect a code change? | [Mutation testing](docs/mutation-testing.md) with test selection from a coverage map (opt-in, runs the tests) | `cargo fallow mutants` |

`cargo fallow check` runs all the static checks and writes one report with a
health score. It reads `cargo metadata` one time and parses each file one time
for all the checks. To see what a rule means and how to fix a finding, run
`cargo fallow explain <rule>`.

## Install

Install the crate from crates.io:

```bash
cargo install cargo-fallow --locked
```

Each [GitHub release](https://github.com/fallow-rs/cargo-fallow/releases)
also has prebuilt binaries for Linux, macOS and Windows, with a checksum and a
signature for each archive.

`--locked` uses the dependency versions in `Cargo.lock`, which are the
versions that CI tests. The package installs one binary, `cargo-fallow`, and
you run it as `cargo fallow`.

> [!WARNING]
> Do not run `cargo install fallow`. That command installs
> [fallow for TypeScript and JavaScript](https://github.com/fallow-rs/fallow).
> The cargo-fallow package does not install a binary named `fallow`.

`mutants` and `visibility --precise` need more tools:

- `cargo fallow mutants` needs [`cargo-nextest`](https://nexte.st) and the
  `llvm-tools` component (`rustup component add llvm-tools`).
- `cargo fallow visibility --precise` needs a build with the `precise` feature
  and the `rust-src` component. See [Precise mode](docs/visibility.md#precise-mode).

## In your terminal

```bash
cargo fallow                                         # each static check, one report
cargo fallow check --changed-since origin/main       # only the findings in changed files
cargo fallow check -p my-core --only deps,visibility # one member, two checks
cargo fallow fix --dry-run                           # show the edits, then run `fix --yes`
cargo fallow explain unused-dependency               # the meaning and the fix of a rule
```

The report shows the health score first, then the findings of each check, and
then the gates. Here is part of the report for the cargo-fallow repository:

```text
cargo fallow check: 1 member, 19 targets
Checks: parse, deps, files, visibility, boundaries, size, complexity, dupes
Health (rust): 97.6 (A), formula version 2

== deps ==
cargo fallow deps: 1 member
No issues found

== visibility ==
cargo fallow visibility (fast mode)
  src/deps/report.rs:41:15 unnecessary-public associated_const `deps::report::Rule::ALL`: pub -> pub(crate) (only tests use it)
  src/mutation/coverage.rs:479:8 unnecessary-public function `mutation::coverage::parse_test_map`: pub -> pub(crate) (only tests use it)
5 findings: 0 dead-public, 5 unnecessary-public, 0 unnecessary-restricted-visibility

== complexity ==
cargo fallow complexity: 108 files, 3811 functions, limits cyclomatic 20 and cognitive 15
  src/deps/manifest.rs:70 entries cyclomatic 20 cognitive 35 (high, 60 lines)
  src/visibility/mod.rs:532 decide cyclomatic 28 cognitive 31 (high, 91 lines)

Gates:
  error-severity-findings: pass, not enforced
  visibility-findings: fail (5 files), not enforced
  ...
```

Each report lists the gates. A failed gate changes the exit code only when
you add `--fail-on-issues`. All commands use the same exit codes:

| Exit code | Meaning |
|---|---|
| `0` | No enforced gate failed. |
| `1` | An enforced gate failed. |
| `2` | The input was not valid, or the run could not complete. With `--format json`, a JSON error document goes to stdout. |

In a script that reads the JSON output, treat `0` and `1` as a complete run.
Do not hide exit code `2` with `|| true`.

## In pull requests

This repository is also a GitHub Action. The action runs one `cargo fallow`
command and writes the result to the job summary. It adds an annotation for
each finding, and it can post a comment on the pull request.

```yaml
name: cargo-fallow
on: pull_request

permissions:
  contents: read

jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@<sha>
        with:
          fetch-depth: 0
          persist-credentials: false
      - uses: dtolnay/rust-toolchain@<sha>
        with:
          toolchain: stable
      - uses: fallow-rs/cargo-fallow@<sha>
        with:
          command: check
          auto-changed-since: 'true'
```

With `auto-changed-since`, the report has only the findings in the files
that the pull request changed. Old findings then do not fail the job. Pin each
action to a full commit SHA.

| GitHub surface | Turn it on with | Job permission |
|---|---|---|
| Job summary and annotations | On by default | None |
| Sticky pull request comment | `comment: 'true'` | `pull-requests: write` |
| Code scanning | `sarif: 'true'` | `security-events: write` |

To get a starting workflow, run `cargo fallow init --ci github`. The
[action guide](action/README.md) lists all inputs and outputs. If you do not
use the action, `--format github-annotations` and `--format github-summary`
write the same annotations and summary. For GitLab, the
[CI template](docs/gitlab.md) writes the code quality report of the merge
request widget, and can post one merge request note.

## Adopt cargo-fallow on an existing workspace

You do not have to fix each finding before you turn on the gates.

1. Run `cargo fallow init`. The command prints the configuration tables, with
   each limit set to the current maximum in the workspace. It does not change
   a file.
2. Run `cargo fallow init --write`. The command adds the tables to the root
   `Cargo.toml` and saves the current findings as a baseline. After this,
   `cargo fallow check --fail-on-issues` passes, and only new findings fail it.
3. In pull requests, add `--changed-since <ref>`. The report then has only the
   findings in the changed files, and you do not need a baseline.
4. Lower the limits over time. To remove unused dependencies and narrow the
   visibility of `pub` items, run `cargo fallow fix`.

The configuration is in `Cargo.toml`. You do not need a separate
configuration file:

```toml
[workspace.metadata.fallow]
baseline = "fallow-baseline.json"

[workspace.metadata.fallow.complexity]
max-cyclomatic = 20
max-cognitive = 15

[workspace.metadata.fallow.deps]
ignored = ["getrandom"] # turns on a feature for a workspace build
```

To keep an intentional finding, add a suppression comment:

```rust
// fallow-ignore-next-line visibility -- kept for a plugin
pub fn hook() {}
```

To change the severity of a rule, set it to `error`, `warn` or `off` in
`[workspace.metadata.fallow.rules]`. The [adoption guide](docs/adoption.md)
describes rule severity, baselines, `--package`, `--changed-since` and all
suppression kinds.

## Mutation testing

`cargo fallow mutants` makes small changes (mutants) in the source of one
package and runs the tests against each mutant. The report lists each mutant
that no test detects.

```bash
cargo fallow mutants --package my-crate
cargo fallow mutants --in-diff origin/main --fail-on-survivors
cargo fallow mutants --inspect 3f6c1d2a9b804e57
```

```text
cargo fallow mutants: my-crate 0.1.0
6 mutants, score 50.0%: 2 killed, 2 survived, 2 no coverage

Survived (2)
  src/a.rs
    :8:5 relational-operator `a == 2` -> `!=` [3f6c1d2a9b804e57]
```

- One build contains most of the mutants. At run time, a switch selects the
  active mutant, so these mutants do not need a compile of their own.
- A coverage map tells which tests execute the lines of a mutant. Only those
  tests run, and the fastest test runs first.
- With `--in-diff <ref>`, you test only the mutants on changed lines.
- All work happens in a copy of the workspace, so your checkout does not
  change. cargo-fallow kills a test process above 4096 MiB (`--memory-limit`
  sets another limit). If `cargo fallow` dies, a watchdog kills the tests.
- Each result goes to a journal. When a run stops early, the next run with the
  same source and options reads the journal and tests only the other mutants.

[Mutation testing](docs/mutation-testing.md) describes the tiers, each status,
the test selection and the limits.

## What cargo-fallow guarantees

- If the source does not change, the report does not change. Finding ids do
  not use line numbers, so an edit above a finding keeps its id.
- When cargo-fallow cannot tell from the syntax if code uses an item, it
  counts the item as used. Examples are a method call on an unknown type, a
  name in macro tokens, and an `include!` from `OUT_DIR`.
- A `check` run does not change `Cargo.lock`, also when `deps` resolves the
  dependencies.
- cargo-fallow parses each file on a thread with a stack sized for that file,
  so a deeply nested file cannot crash the run. A file that does not parse
  fails the `parse-error` gate.
- Each JSON document matches the [JSON Schema](docs/output-schema.json). A test
  in CI fails when a report and the schema differ.
- cargo-fallow computes finding ids and the health score the same way as
  fallow for JavaScript. Most gate names are also the same.

## With scripts and coding agents

In a script or a coding agent, add `--format json --quiet`. Each command then
writes one JSON document to stdout:

- `kind` is the name of the command, and `schema_version` is the version of
  the document shape.
- Each finding has `id`, `check`, `rule`, `severity`, `location` and an
  `actions[]` array. When `auto_fixable` is `true`, `cargo fallow fix` can
  apply the action.
- With `--explain`, the document has a `_meta.rules` object. It gives the
  meaning and the fix of each rule that has a finding.
- `cargo fallow schema` prints the JSON Schema of each document.

For grep, use `--format compact`. It writes one `path:line:col: rule: message`
line for each finding. [Output formats](docs/output.md) describes all formats.

## Documentation

| Page | Contents |
|---|---|
| [The check command](docs/check.md) | Flags of `check`, and the health score |
| [Adoption](docs/adoption.md) | Changed files, packages, rule severity, baselines, `init` and suppressions |
| [Unused dependencies](docs/dependencies.md) | Rules of `deps`, and the cases where it counts a dependency as used |
| [Fix](docs/fix.md) | What `cargo fallow fix` changes, and how it checks the result |
| [Unused files](docs/unused-files.md) | How `files` finds the files that no target compiles |
| [Visibility](docs/visibility.md) | Kinds of `visibility`, and precise mode |
| [Structure checks](docs/structure.md) | `boundaries`, `size`, `complexity` and `dupes` |
| [Mutation testing](docs/mutation-testing.md) | `mutants`: tiers, status, test selection and safety |
| [Trace](docs/trace.md) | `--trace` of `visibility`, `deps` and `dupes`, to see why a check reports one item or does not |
| [Output formats](docs/output.md) | Each format, the JSON Schema, `explain` and the exit codes |
| [GitHub Action](action/README.md) | Inputs, outputs and install methods of the action |

## Contributing

To report a false positive or a missing case, [open an issue](https://github.com/fallow-rs/cargo-fallow/issues).
[CONTRIBUTING.md](CONTRIBUTING.md) describes the build, the quality gates and
the DCO sign-off. To report a vulnerability, follow [SECURITY.md](SECURITY.md).

## License

MIT. See [LICENSE](LICENSE).
