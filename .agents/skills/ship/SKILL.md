---
name: ship
description: Merge reviewed cargo-fallow work to main with pinned auto-merge, watch the CI of the merged commit, and clean up. Use when asked to ship or merge a pull request.
---

# Ship

1. Make sure that the diff is the reviewed scope. When the pull request
   conflicts with `main`, merge `origin/main` into the branch. Then run the
   gates again and push.
2. Make sure that each commit is signed (`git commit -S -s`) and has a
   conventional prefix.
3. Arm pinned auto-merge before any CI wait. Do it again after each push,
   with the new head:

   ```bash
   gh pr merge <N> --squash --auto --match-head-commit <reviewed SHA> \
     --subject "<type>: <summary> (#<N>)"
   ```

   The pin stops the merge when someone pushes a commit that you did not
   review. The required checks are `CI` and `Action checks passed`.
4. Wait for the checks in the foreground: run `gh pr checks <N>` in a shell
   loop, one call every 2 minutes. Fix each failure, push, and arm the
   merge again. A check that is not required, for example `Commitlint`,
   must also pass. When it fails, remove the auto-merge with
   `gh pr merge <N> --disable-auto` until the fix is pushed.
5. When the pull request is merged, read the merge commit with
   `gh pr view <N> --json mergeCommit`. Wait until each run of that commit
   on `main` passes: `gh run list --commit <SHA>`, then
   `gh run watch <run ID> --exit-status`. A failure there needs a new pull
   request through `review` and `ship`.

## In a T3 Code thread

T3 forbids a poll for a pull request. It also sends no wake when a pull
request merges (pingdotgg/t3code#25160). So steps 3 and 4 change:

1. Do not arm auto-merge. Call `watch_pull_request` and end the turn.
2. At each checks wake, read `gh pr view <N> --json mergeStateStatus`.
   - `CLEAN`: merge by hand with
     `gh pr merge <N> --squash --match-head-commit <reviewed SHA>`.
   - `BLOCKED`: end the turn. Both required checks are jobs with `needs:`,
     so GitHub makes them late. T3 can say that all required checks
     passed before `CI` exists (pingdotgg/t3code#28183). The next wake
     comes when the check appears.
3. For step 5, run `gh run watch <run ID> --exit-status` in the
   background and wait for it with `t3_background_wait`.
6. Remove the worktree with `git worktree remove` and delete the local
   branch.

## Release

Only the owner releases. Do not tag, publish or bump a version without
the owner. `docs/release-procedure.md` gives each step. The release commit
has the exact subject `chore: release vX.Y.Z`, without ` (#N)`.
