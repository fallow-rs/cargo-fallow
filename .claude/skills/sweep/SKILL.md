---
name: sweep
description: Audit a cargo-fallow objective for missed work, weak evidence, open follow-ups and dirty worktrees before you report it as complete.
---
<!-- Generated from .agents/skills. Do not edit. -->

# Sweep

Run this at the end of each objective, without a prompt from the owner.

1. Read the full request, the diff and the state of each pull request
   again.
2. Map each acceptance criterion to its evidence: a test, a command
   output or a merged commit.
3. Look for missed consumers: `README.md`, `docs/`,
   `docs/output-schema.json`, `action/`, `ci/`, the changelog fragment and
   the skills.
4. Run each weak or old check again.
5. Check each pull request with `gh pr view <URL> --json state,mergeCommit`.
   Each one must be merged, with green CI on its merge commit (step 5 of
   `ship`). Armed auto-merge does not pass this check. Report a blocked pull
   request with the blocker and the next action.
6. Fix each omission in scope now, in a new pull request through `review`
   and `ship`. Open an issue in `fallow-rs/cargo-fallow` for each other item
   of needed work. Write the question and a recommended answer in the issue
   when it needs a decision of the owner. A note in an ignored file, in
   `/tmp` or in the chat does not track a follow-up.
7. Make sure that each worktree, branch and temporary file of the task is
   clean or removed.
8. Keep a lesson only when it changes a future decision and no skill or doc
   has it. Store it in the strongest form: a test or a CI check, then a
   script, then text in a skill or a doc.

When you find no bug, the work is not yet done. Prove each requested result.
