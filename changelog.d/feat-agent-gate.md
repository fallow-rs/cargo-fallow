### Added

- `cargo fallow hooks install --target agent` installs a gate for Claude Code
  and Codex. The gate blocks a `git commit` or a `git push` of the agent when
  `cargo fallow check` finds a new finding in the changed files, and it lists
  each finding. A finding that the base commit already has does not block. An
  error of the tool does not block.
- `cargo fallow hooks install --target git` installs the same check as a git
  pre-commit hook. `hooks uninstall` removes the files, and `hooks status`
  shows them. `--format json` gives the `hooks` and `hooks-status` documents.
