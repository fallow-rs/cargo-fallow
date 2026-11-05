### Added

- `cargo fallow agent install` sets up a repository for coding agents in one
  step: a guide block in `AGENTS.md`, the `cargo-fallow` skill for Claude Code
  and Codex, and the commit gate. `agent status` shows the parts, and `agent
  uninstall` removes them. `cargo fallow init --agents` runs `agent install`.
