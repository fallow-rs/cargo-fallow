---
name: implement
description: Research, test, implement, document and validate a cargo-fallow feature, fix or repository change. Use when asked to build or change cargo-fallow.
---
<!-- Generated from .agents/skills. Do not edit. -->

# Implement

Deliver the change through the lifecycle of this repository:
`implement`, then `review`, then `ship`, then `sweep`.

1. Read `AGENTS.md`, `CONTRIBUTING.md` and the page in `docs/` of each
   command that the change touches.
2. Check the branch, the worktree, the open pull requests and `origin/main`.
   Other workers can use the same checkout. Make a worktree from the newest
   `origin/main` with a branch `<type>/<slug>`. The type is a conventional
   commit type.
3. Look at how fallow does the same thing for TypeScript. Keep its names,
   flags, JSON fields and exit codes where they make sense for Rust.
4. Write a failing test first. For a bug, get a command that fails before
   you change the code. A Rust test goes under `tests/`. A script test is
   `scripts/<name>.test.mjs`.
5. Write the code. Keep the module layout of `AGENTS.md`. A file over 2000
   lines must not grow, so move new code to a new module.
6. Validate a change to the analysis on real workspaces: the corpus in
   `tests/corpus/README.md` and cargo-fallow itself. A false positive is
   not acceptable. When the syntax cannot decide, give credit and do not
   report.
7. Update `README.md`, `docs/`, `docs/output-schema.json`,
   `action/README.md` and `docs/gitlab.md` where the behavior shows.
8. Add a changelog fragment `changelog.d/<slug>.md` for a change that a
   user can see. The check reads only the top level of `changelog.d/`, so
   the name has no `/`. Do not edit `CHANGELOG.md`. `changelog.d/README.md` gives
   the format.
9. Run the gates:

   ```bash
   cargo fmt --check
   cargo clippy --all-targets --locked -- -D warnings
   cargo nextest run --locked
   npm run lint:js && npm run fmt:js:check && npm run test:scripts
   npm run check:agent-adapters
   node scripts/changelog-fragments.mjs check --base origin/main
   node scripts/check-file-size.mjs --base origin/main
   ```

   The Quality gates table in `CONTRIBUTING.md` lists every other gate.
   Run each gate of that table that the change touches.
10. Commit signed with `git commit -S -s` and a conventional prefix. Push
    and open a pull request. Write each open item in the body.
11. Continue with `review`, `ship` and `sweep` in the same session. Do not
    stop at an open pull request. Stop only for a decision of the owner.

## Definition of done

- Each pull request of the objective is merged to `main`, and CI on the
  merged commit is green.
- Pinned auto-merge was armed before each CI wait. In a T3 Code thread,
  each merge was by hand at a wake (see `ship`).
- Each follow-up is fixed in a pull request or filed as an issue in
  `fallow-rs/cargo-fallow`. A note in an ignored file, in `/tmp` or in the
  chat does not track a follow-up.
