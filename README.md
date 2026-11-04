<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/fallow-rs/cargo-fallow/main/assets/logo-dark.svg">
    <source media="(prefers-color-scheme: light)" srcset="https://raw.githubusercontent.com/fallow-rs/cargo-fallow/main/assets/logo.svg">
    <img src="https://raw.githubusercontent.com/fallow-rs/cargo-fallow/main/assets/logo.svg" alt="cargo fallow" width="360">
  </picture>
</p>

<p align="center">
  <strong>Codebase intelligence for Rust workspaces.</strong><br>
  Unused dependencies, dead public API, duplication, complexity, boundaries and mutation testing, from one module graph of your workspace.<br>
  One Cargo subcommand. It runs on stable Rust and the static checks build nothing.
</p>

<p align="center">
  <a href="https://github.com/fallow-rs/cargo-fallow/actions/workflows/ci.yml"><img src="https://github.com/fallow-rs/cargo-fallow/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/fallow-rs/cargo-fallow/blob/main/Cargo.toml"><img src="https://img.shields.io/badge/rust-1.98%2B-8B7355.svg" alt="MSRV 1.98"></a>
  <a href="https://github.com/fallow-rs/cargo-fallow/blob/main/CHANGELOG.md"><img src="https://img.shields.io/badge/status-pre--release-orange.svg" alt="Status: pre-release"></a>
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

`cargo fallow` is the Rust part of [Fallow](https://github.com/fallow-rs/fallow).
It reads `cargo metadata`, parses the source with `ra_ap_syntax` (the parser of
rust-analyzer), and builds one module graph of the workspace. Each check reads
that graph. The static checks compile nothing and need no nightly toolchain.

> [!IMPORTANT]
> cargo-fallow is pre-release. The crate is not on crates.io yet, and the JSON
> shapes can change before the first release. The
> [CHANGELOG](CHANGELOG.md) records each change.

## What cargo-fallow finds

| Question | Check | Command |
|---|---|---|
| Which dependencies does no code use? | [Unused, misplaced and feature-only dependencies](docs/dependencies.md), with an [auto-fix](docs/fix.md) that keeps the format of `Cargo.toml` | `cargo fallow deps`, `cargo fallow fix` |
| Which `pub` items are too visible? | [Dead public items and items that only their own crate uses](docs/visibility.md), with an optional rust-analyzer pass for the cases that syntax cannot decide | `cargo fallow visibility` |
| Which `.rs` files does no target compile? | [Files that no `mod` item, `#[path]` or `include!` reaches](docs/unused-files.md) | `cargo fallow files` |
| What is copied? | [Clone groups](docs/structure.md) on normalized token streams, in four modes from `strict` to `semantic` | `cargo fallow dupes` |
| Where is the code hard to change? | [Functions over the cyclomatic or the cognitive limit](docs/structure.md), and a [0 to 100 health score](docs/check.md#health-score) | `cargo fallow complexity` |
| Does the architecture hold? | [Crate, module and source rules](docs/structure.md) in `Cargo.toml` | `cargo fallow boundaries` |
| Do large files grow? | [Line limits for files and functions](docs/structure.md), and a growth gate against a base ref | `cargo fallow size` |
| Do the tests detect a code change? | [Mutation testing](docs/mutation-testing.md) with test selection from a coverage map (opt-in, runs the tests) | `cargo fallow mutants` |

`cargo fallow check` runs each static check in one pass: it reads
`cargo metadata` once, builds the graph once and parses each file once. It
writes one report with a health score. `cargo fallow explain <rule>` gives the
meaning, an example and the fix of each rule.

## Install

The crate is not on crates.io yet. Install it from source:

```bash
cargo install --git https://github.com/fallow-rs/cargo-fallow --locked
```

`--locked` builds with the versions in `Cargo.lock`, the versions that CI
tests. The package installs one binary, `cargo-fallow`. Cargo finds it as the
`cargo fallow` subcommand.

> [!WARNING]
> Do not run `cargo install fallow`. That crate is
> [fallow for TypeScript and JavaScript](https://github.com/fallow-rs/fallow).
> cargo-fallow never installs a binary named `fallow`.

Two parts need more setup:

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

The report starts with the health score, then gives each check, then one gate
block. This is an extract of cargo-fallow on its own repository:

```text
cargo fallow check: 1 member, 19 targets
Checks: parse, deps, files, visibility, boundaries, size, complexity, dupes
Health (rust): 97.6 (A), formula version 2

== deps ==
cargo fallow deps: 1 member
No issues found

== visibility ==
cargo fallow visibility (fast mode)
  src/deps/report.rs:41:15 unnecessary_public associated_const `deps::report::Rule::ALL`: pub -> pub(crate) (only tests use it)
  src/mutation/coverage.rs:479:8 unnecessary_public function `mutation::coverage::parse_test_map`: pub -> pub(crate) (only tests use it)
5 findings: 0 dead_public, 5 unnecessary_public, 0 unnecessary_restricted_visibility

== complexity ==
cargo fallow complexity: 108 files, 3811 functions, limits cyclomatic 20 and cognitive 15
  src/deps/manifest.rs:70 entries cyclomatic 20 cognitive 35 (high, 60 lines)
  src/visibility/mod.rs:532 decide cyclomatic 28 cognitive 31 (high, 91 lines)

Gates:
  error-severity-findings: pass, not enforced
  visibility-findings: fail (5 files), not enforced
  ...
```

<sub>Abridged. The full report has each check and each gate.</sub>

A gate is in each report, but it fails the run only with `--fail-on-issues`.
The exit codes are the same for each command:

| Exit code | Meaning |
|---|---|
| `0` | No enforced gate failed. |
| `1` | An enforced gate failed. |
| `2` | The input was not valid, or the run could not complete. With `--format json`, a JSON error document goes to stdout. |

A script that reads the JSON output can treat `0` and `1` as a complete run.
It must not hide exit code `2` with `|| true`.

## In pull requests

This repository is also a GitHub Action. It runs one command, writes the job
summary, shows each finding as an annotation, and can post one sticky comment
on the pull request.

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

`auto-changed-since` keeps only the findings in the files that the pull
request changed, so old findings do not fail the job. Pin each action to a
full commit SHA.

| GitHub surface | Turn it on with | Job permission |
|---|---|---|
| Job summary and annotations | On by default | None |
| Sticky pull request comment | `comment: 'true'` | `pull-requests: write` |
| Code scanning | `sarif: 'true'` | `security-events: write` |

`cargo fallow init --ci github` writes a starting workflow. The
[action guide](action/README.md) lists each input and each output. Without the
action, `--format github-annotations` and `--format github-summary` give the
same annotations and summary. For GitLab, the [CI template](docs/gitlab.md)
writes the code quality report of the merge request widget, and can post one
merge request note.

## Adopt cargo-fallow on an existing workspace

You do not have to fix each finding before you turn on the gates.

1. Run `cargo fallow init`. It prints the configuration tables with each limit
   at the current maximum of the workspace. It changes no file.
2. Run `cargo fallow init --write`. It adds the tables to the root
   `Cargo.toml` and saves a baseline of the current findings. The first
   `cargo fallow check --fail-on-issues` then passes, and a new finding fails.
3. In pull requests, use `--changed-since <ref>`. The run then judges only the
   changed files, and needs no baseline.
4. Lower the limits over time. `cargo fallow fix` removes the unused
   dependencies and narrows the visibility of each `pub` item that it can.

The configuration lives in `Cargo.toml`, so Cargo parses it and no extra file
is necessary:

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

`[workspace.metadata.fallow.rules]` sets a rule to `error`, `warn` or `off`.
[Adoption](docs/adoption.md) covers rule severity, baselines, `--package`,
`--changed-since` and each suppression kind.

## Mutation testing

`cargo fallow mutants` makes small changes (mutants) in the source of one
package, runs the tests against each mutant, and reports the mutants that no
test detects.

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

- Most mutants go into one build. A runtime switch selects the active mutant,
  so a mutant costs no compile.
- A coverage map selects the tests of each mutant, and the fastest test runs
  first.
- `--in-diff <ref>` runs only the mutants on changed lines.
- The run works in a copy, so the checkout does not change. A memory guard
  stops a mutant that uses too much memory, and a watchdog stops the tests
  when `cargo fallow` stops.
- A journal keeps each result. A run that stops continues where it stopped.

[Mutation testing](docs/mutation-testing.md) describes the tiers, each status,
the test selection and the limits.

## Why you can depend on cargo-fallow

- The same input gives the same output. Each finding has a stable id that does
  not use line numbers, so an edit above a finding keeps its id.
- The static checks read syntax and `cargo metadata`. They compile nothing and
  run no build script, so a full `check` takes seconds.
- When syntax cannot decide, a check gives credit and does not report. A
  method call on an unknown type, a name in macro tokens and an `OUT_DIR`
  include keep an item in use.
- `Cargo.lock` stays as it was before each run, also when `deps` resolves the
  dependencies.
- Each parse runs on a bounded stack, so a deeply nested file cannot crash the
  run. A file that does not parse fails the `parse-error` gate and never
  passes in silence.
- Each command has a typed JSON output, documented exit codes, and a published
  [JSON Schema](docs/output-schema.json). A test fails CI when a report and the
  schema disagree.
- The finding ids, the health score model and most gate names are the same as
  in fallow for JavaScript.

## With scripts and coding agents

Scripts and agents add `--format json --quiet`. Each command then writes one
JSON document to stdout:

- `kind` names the command, and `schema_version` names the shape.
- Each finding has `id`, `check`, `rule`, `severity`, `location` and an
  `actions[]` array. `auto_fixable` tells if `cargo fallow fix` applies the
  action.
- `--explain` adds a `_meta.rules` object with the meaning and the fix of each
  rule with a finding.
- `cargo fallow schema` prints the JSON Schema of each document.

`--format compact` gives one `path:line:col: rule: message` line for each
finding, for grep and for agents. [Output formats](docs/output.md) describes
each format.

## Documentation

| Page | Contents |
|---|---|
| [The check command](docs/check.md) | Flags of `check`, and the health score |
| [Adoption](docs/adoption.md) | Changed files, packages, rule severity, baselines, `init` and suppressions |
| [Unused dependencies](docs/dependencies.md) | Rules of `deps`, and how it gives credit |
| [Fix](docs/fix.md) | What `cargo fallow fix` changes, and how it checks the result |
| [Unused files](docs/unused-files.md) | How `files` finds the files that no target compiles |
| [Visibility](docs/visibility.md) | Kinds of `visibility`, and precise mode |
| [Structure checks](docs/structure.md) | `boundaries`, `size`, `complexity` and `dupes` |
| [Mutation testing](docs/mutation-testing.md) | `mutants`: tiers, status, test selection and safety |
| [Output formats](docs/output.md) | Each format, the JSON Schema, `explain` and the exit codes |
| [GitHub Action](action/README.md) | Inputs, outputs and install methods of the action |

## Contributing

To report a false positive or a missing case, [open an issue](https://github.com/fallow-rs/cargo-fallow/issues).
[CONTRIBUTING.md](CONTRIBUTING.md) covers the build, the quality gates and the
DCO sign-off. [SECURITY.md](SECURITY.md) tells you how to report a
vulnerability.

## License

MIT. See [LICENSE](LICENSE).
