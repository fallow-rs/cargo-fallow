# Contributing to cargo-fallow

Thank you for your help. This guide covers the build, the checks and the
sign-off rule.

## Build and test

```bash
cargo build
cargo test
```

The tests copy each fixture under `tests/fixtures/` to a temporary directory.
A test never writes into the repository.

## Lints and format

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo deny check
```

`Cargo.toml` enables the Clippy `pedantic`, `nursery` and `cargo` groups and a
set of restriction lints. Do not use `#[allow]` without a `reason`. Use
`writeln!` on a locked stdout or stderr, not `println!`. Do not call
`std::process::exit`. Only the workspace loader in `src/workspace.rs`,
`src/mutation/process.rs` and the git reader in `src/structure/git.rs` can spawn
a process. `.clippy.toml` holds that rule.

## Mutants command

These rules keep the `mutants` command safe and its output stable:

- A mutated binary runs only through `process::run_to_files`, under a time limit and
  the memory guard. A test does not pass `--no-memory-guard`, except for a check of
  that flag.
- `tests/mutation.rs` and `tests/mutation_schema.rs` test the engine. `tests/mutants_cli.rs`
  tests the product surface: each exit code and each format, on a fixture with a known
  killed mutant and a known surviving mutant.
- These tests use `git`, `cargo-nextest` and the `llvm-tools` component. They run one at
  a time: `.config/nextest.toml` puts them in one test group, and a lock in each test file
  does the same under `cargo test`.
- A new member of the report goes into `src/mutation/report.rs`. A change that removes a
  member or changes its type increments `SCHEMA_VERSION`. The human and SARIF forms are
  in `src/render/mutants.rs`.
- The order of `mutants` must not depend on the order of the workers. The engine writes
  each row at a fixed index and sorts the rows at the end.
- An action is a read-only command. The tool does not propose a code change.

## Quality gates

The gates are the same as in the fallow repository, where they apply.
`hk.pkl` runs the local gates. Run `hk install` once per clone to enable
the hooks. Run `npm ci` once to install commitlint, oxlint and oxfmt for the
hooks.

| Gate | Command | Hook | CI |
| --- | --- | --- | --- |
| Format | `cargo fmt --all -- --check` | pre-commit, pre-push | `ci.yml` |
| Clippy | `cargo clippy --all-targets --locked -- -D warnings` | pre-commit, pre-push | `ci.yml`, Linux and Windows |
| Tests | `cargo nextest run --locked` or `cargo test --locked` | none | `ci.yml`, Linux, macOS and Windows |
| Documentation | `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items` | pre-push | `ci.yml` |
| Typos | `typos` | pre-commit | `ci.yml` |
| Hidden unicode | `python3 scripts/scan-hidden-unicode.py --mode committed` | pre-commit | `ci.yml` |
| Conflict markers | `sh scripts/check-conflict-markers.sh` | pre-commit | `ci.yml` |
| Narrator comments | `node scripts/check-comment-quality.mjs --all` | pre-commit | `ci.yml` |
| File size | `node scripts/check-file-size.mjs --staged` | pre-commit | `ci.yml`, with `--base origin/main` |
| JS lint | `npm run lint:js` | pre-commit | `ci.yml` |
| JS format | `npm run fmt:js:check` | pre-commit | `ci.yml` |
| Script tests | `npm run test:scripts` | none | `ci.yml` |
| Dependency policy | `cargo deny check` | none | `ci.yml` |
| Security advisories | `cargo audit` | none | `ci.yml` |
| Unused dependencies | `cargo shear` | pre-push | `ci.yml` |
| Workflow syntax | `actionlint` | none | none |
| Workflow security | `uvx zizmor@1.26.1 --min-confidence medium --format plain .github/` | none | `ci.yml` |
| MSRV | `cargo +1.98.0 check --locked` | none | `ci.yml` |
| Commit messages | `commitlint` | commit-msg | `commitlint.yml` |
| DCO sign-off | `node .github/scripts/check-dco.mjs` | commit-msg | `commitlint.yml` |
| Line coverage | `cargo llvm-cov` | none | `coverage.yml`, floor 95.0% |
| Fuzz smoke | `cargo +nightly-2026-07-20 fuzz run fuzz_parse -- -max_total_time=30` | none | `fuzz-smoke.yml`, main and weekly |
| Visibility audit | `scripts/check-hawk.sh` | none | `hawk.yml`, weekly |

A file over 2000 lines must not grow. Move new code to a new module.

fallow runs `actionlint` by hand only, not in a hook or in CI. Do the same:
run `actionlint` after you change a workflow. `.github/actionlint.yaml` holds
its configuration.

`mise.toml` pins the version of each local tool. Run `mise install` to get
them, or install each tool by hand at the version in that file. The tools
are `typos`, `cargo-shear`, `cargo-deny`, `cargo-audit`,
`cargo-nextest`, the `llvm-tools` component, `cargo-llvm-cov`, `actionlint`, `zizmor`, Node.js 22 and
Python 3. The Hawk audit needs `cargo hawk` 0.1.17 on toolchain 1.99.0.

### Gates that this repository does not have yet

These fallow gates do not apply now:

- Miri: the crate has no unsafe code, and `unsafe_code` is `forbid`.
- The napi, VS Code, Zed, Docker and npm jobs: the crate has no such
  surface.
- CodeQL and OpenSSF Scorecard: both need the public GitHub repository.
- Benchmarks: the crate has no hot path to measure yet.
- Mutation testing on this crate: `cargo fallow mutants` exists, and M1 adds the
  CI job. `--fail-on-survivors` is the gate of that job.

## Releases

`docs/release-procedure.md` describes the release workflow, the package
checks and the one-time steps before the first release. Only the owner
releases.

## Developer Certificate of Origin

cargo-fallow uses the
[Developer Certificate of Origin](https://developercertificate.org/) (DCO).
With a sign-off, you confirm that you wrote the change or that you have the
right to submit it under the project license.

Add a `Signed-off-by` line to every commit with the `-s` flag:

```bash
git commit -s -m "fix: describe the change"
```

Git writes the line from your `user.name` and `user.email`:

```text
Signed-off-by: Your Name <you@example.com>
```

The email must match the commit author. The `-S` flag adds a cryptographic
signature. Use both: `git commit -S -s`.

## Commit messages

Use conventional commit prefixes, for example `feat:`, `fix:` and `chore:`.

## Prose rule

Write all prose in ASD-STE100 Simplified Technical English. This includes
comments, docs, commit messages and pull request text.

- Write short sentences: 20 words for an instruction, 25 for a description.
- Use the active voice and simple tenses.
- Use one word for one meaning.
- Do not use idioms or contractions.
