# Hooks: the agent gate and the git pre-commit hook

`cargo fallow hooks` installs a check that runs before a commit. The check
blocks a commit only when `cargo fallow check` finds a new finding in the
changed files. A finding that the base commit already has does not block. An
error of the tool does not block.

```bash
cargo fallow hooks install --target agent   # Claude Code and Codex
cargo fallow hooks install --target git     # the git pre-commit hook
cargo fallow hooks status
cargo fallow hooks uninstall --target agent
```

| Flag | Meaning |
| --- | --- |
| `--target agent\|git` | The surface. `agent` is the gate of Claude Code and Codex. `git` is the pre-commit hook. |
| `--agent claude\|codex` | Only this harness of the agent target. The default is each harness whose folder (`.claude/` or `.codex/`) exists, else Claude Code. |
| `--manifest-path <PATH>` | The `Cargo.toml` of the workspace. |
| `--dry-run` | Show the changes, and write nothing. |
| `--force` | Replace or remove a script that someone edited, or a hook that cargo-fallow did not write. |
| `--format human\|json` | The output format. The JSON documents are `hooks` and `hooks-status`. |

The commands write to the git top level of the workspace. When the workspace
is in a subfolder, the scripts record the path of its `Cargo.toml`.

## The agent gate

The gate is a `PreToolUse` hook on the shell tool of the harness:

| Harness | Configuration | Script |
| --- | --- | --- |
| Claude Code | `.claude/settings.json`, matcher `Bash`, timeout 300 seconds | `.claude/hooks/cargo-fallow-gate.sh` |
| Codex | `.codex/hooks.json`, matcher `^Bash$` | `.codex/hooks/cargo-fallow-gate.sh` |

The install keeps each other key and each other handler in the configuration.
The handler of the fallow gate for JavaScript (`fallow-gate.sh`) stays, so the
two gates can run in one repository.

For each shell command of the agent, the gate does these steps:

1. It skips each command that is not a `git commit` or a `git push`. A
   command without the words `git` and `commit` or `push` starts no process.
2. It finds the workspace of the session folder, and of each folder that the
   command names (`cd`, `git -C`, `--work-tree`, `--git-dir`).
3. It runs `cargo fallow check --fail-on-issues --offline` on the files that
   changed since the base:
   - A commit compares with `HEAD`. Staged, unstaged and untracked changes
     count.
   - A push compares with the upstream of the branch, else with
     `origin/HEAD`, `origin/main`, `origin/master`, `main` or `master`. A push
     also runs the `file-growth` gate of `size` (`--base`).
4. It runs the same check on the base commit, and leaves out each finding
   that the base commit has. The result of the base commit stays in
   `.fallow/` as a cache, so a retry after a block runs one check.
5. When an enforced gate fails, it blocks the command (exit code 2), and
   prints one line for each new finding, the failed gates and the path of the
   report.

```text
cargo-fallow-gate: blocked by cargo-fallow 0.3.0 in /work/app
cargo-fallow-gate: failed gates: error-severity-findings
cargo-fallow-gate: 1 finding(s) in the changed files caused the block:
  deps/unused-dependency app/Cargo.toml:7 `helper` is in [dependencies], but no code of `app` uses it
cargo-fallow-gate: full report: /work/app/.fallow/cargo-gate-report.json
cargo-fallow-gate: fix the findings and try again. `cargo fallow explain <rule>` explains a rule, and `cargo fallow fix --dry-run --format json` lists the fixable actions.
```

The gate keeps the commit open, with one line on stderr, in these cases:

- The binary is not found (`CARGO_FALLOW_GATE_BIN`, `PATH`, then
  `${CARGO_HOME:-~/.cargo}/bin`).
- The payload is not valid JSON.
- `cargo metadata` fails, for example because `--offline` needs a download.
- The repository has no commit, or a push has no base.
- The runtime crashes or stops on a signal.

The gate needs bash. On Windows, use Git Bash or WSL.

### Settings

| Variable | Meaning |
| --- | --- |
| `CARGO_FALLOW_GATE_BIN` | The `cargo-fallow` binary to run. |
| `CARGO_FALLOW_GATE_MIN_VERSION` | The lowest version that the gate accepts. The default is the version that installed the gate. Below the floor, the gate blocks and prints the upgrade command. An empty value turns the floor off. |
| `CARGO_FALLOW_GATE_REPORT` | The path of the JSON report. The default is `.fallow/cargo-gate-report.json` in the workspace root. A relative path is relative to the workspace root. |
| `CARGO_FALLOW_GATE_DEBUG=1` | Also log each command that the gate skips. |

The report is the JSON report of `check`. The gate never writes through a
symbolic link: when the report path or one of its folders is a link, the gate
writes the report to a temporary file and names that file. The gate writes
`.fallow/.gitignore` with `*`, so the report does not make the work tree
dirty.

### Edited scripts

Each generated script holds a checksum. `hooks install` replaces a script
whose checksum matches. A script that someone edited stays: `install` exits 2,
and `uninstall` keeps it. Give `--force` to replace or remove it.

## The git pre-commit hook

`--target git` writes the `pre-commit` hook in the hooks folder that git uses
(`git rev-parse --git-path hooks`), so `core.hooksPath` and worktrees work.
With a `.husky/` folder, the hook is `.husky/pre-commit`. When `hk.pkl`,
`lefthook.yml` or `.pre-commit-config.yaml` exists, the command writes no file
and prints the command to add to the pre-commit step of that tool.

The hook runs the same commit check as the agent gate. It blocks only on new
findings, and an error of the tool does not block. Skip it for one commit with
`git commit --no-verify`.

## Status

`cargo fallow hooks status` gives the state of `claude`, `codex` and `git`:

| State | Meaning |
| --- | --- |
| `installed` | The handler and the generated script are in place. |
| `partial` | The handler or the script is missing. |
| `edited` | The script is not the generated one. |
| `managed` | A hook manager runs the git hooks. |
| `absent` | Nothing is installed. |
