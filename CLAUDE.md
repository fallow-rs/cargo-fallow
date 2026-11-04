# cargo-fallow repository adapter for Claude

@AGENTS.md

`AGENTS.md` is the repository contract for Claude, Codex and contributors.

## Knowledge layers

- `AGENTS.md` is the compact router. Keep it short.
- `.agents/skills/` holds the client-neutral maintainer workflows.
- `.claude/skills/` is generated from `.agents/skills/`. Do not edit it.
- `README.md` describes the tool for users.
- `CONTRIBUTING.md` describes the build, the checks and the DCO.
- `.plans/` is ignored task state. Do not commit it.

## Workflow

Use the lifecycle skills for each objective: `implement`, `review`, `ship`
and `sweep`. Do not stop at an open pull request.
