# cargo-fallow agent guide

This file is the router for agents and contributors. Keep it short.

## Start here

1. Read `README.md` for the CLI contract, and `docs/` for each command.
2. Read `CONTRIBUTING.md` for the build, the checks and the DCO.
3. Load a workflow from `.agents/skills/` when one exists.

Before you edit, check the branch and the worktree. Other agents can work in
the same checkout. Read a file again immediately before you edit it.

## Architecture

```text
cargo metadata -> workspace model -> gates -> report -> render
```

- `src/lib.rs` holds the modules and the one public function, `run`.
- `src/main.rs` is a thin binary that calls `run`.
- `src/workspace.rs` reads `cargo metadata --no-deps` and builds the model.
- `src/check/` holds the `check` command: the static checks in one run and
  the combined report.
- `src/gates.rs` holds the name of each gate, and computes the parse gate.
- `src/finding.rs` holds the shared finding shape and the CodeClimate output.
- `src/section.rs` holds the output of one check in each format.
- `src/health.rs` computes the health score of the Rust code.
- `src/report.rs` holds the report versions.
- `src/render/` writes the human and SARIF output of `deps`, `mutants` and the
  parse gate.
- `src/error.rs` holds the CLI errors and their codes.
- `src/parse.rs` holds the bounded parse. Each parse goes through this module.
- `src/modules.rs` holds the module walk from `mod` declarations. The
  `mutants` command and the module graph use it. Do not add a second walk.
- `src/attrs.rs` reads attributes and names from the syntax tree.
- `src/graph/` holds the workspace module graph: crates, modules, items,
  imports and references, and the name resolution in `resolve.rs`.
- `src/deps/` holds the `deps` command: the unused dependency rules on top of
  the module graph and `cargo metadata` with the resolve.
- `src/visibility/` holds the `visibility` command. `precise/` holds
  `--precise` mode: a worker process on the `ra_ap_*` crates, behind the
  `precise` Cargo feature. Keep each `ra_ap_*` crate on one version.
- `src/structure/` holds the `boundaries`, `size`, `complexity` and `dupes`
  commands. `src/structure/mod.rs` lists its parts. Their configuration is
  under `[workspace.metadata.fallow]`. `src/structure/git.rs` spawns `git` for
  the size check, and only reads.
- `src/mutation/` holds the `mutants` command. `src/mutation/mod.rs` lists its
  parts. A mutated binary runs only through `process::run_to_files`, under a time
  limit and the memory guard.

Internal parts are modules, not crates. Shared wire types come from
`fallow-report-protocol`. Do not define a second copy of them here.

## CLI contract

- Use `--format json` for machine output and `--quiet` to hide progress.
- Exit `0` when no enforced gate failed.
- Exit `1` when an enforced gate failed.
- Exit `2` for invalid input or an execution error.
- With `--format json`, an error is a JSON error document on stdout.
- Output paths are relative to the workspace root, with `/` separators.
- The package never installs a binary named `fallow`.

## Validation

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo deny check
```

The Quality gates section in `CONTRIBUTING.md` lists every gate.
`hk run pre-commit --all` and `hk run pre-push --all` run the hook gates.

## Git

- Sign commits with `git commit -S -s`. The `-s` flag adds the DCO sign-off.
- Use conventional commit prefixes: `feat:`, `fix:`, `chore:`, `docs:`,
  `refactor:`, `test:`, `ci:`.
- Do not add AI attribution or `Co-Authored-By` trailers.
- Before you commit, check `git status` and the staged paths.
