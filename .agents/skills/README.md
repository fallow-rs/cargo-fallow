# Skills

This directory holds the maintainer workflows of cargo-fallow. Codex and
other clients read it directly. Each objective goes through four skills:

1. `implement`: test first, write the change, validate it and open a pull
   request.
2. `review`: get an independent review and fix each confirmed finding.
3. `ship`: merge with pinned auto-merge and watch the CI of the merged
   commit.
4. `sweep`: check each result, each follow-up and each worktree.

`.claude/skills/` holds the Claude adapter of each skill. Do not edit it.
Edit the source here, then run `npm run generate:agent-adapters`. CI runs
`npm run check:agent-adapters` and fails on a difference.
