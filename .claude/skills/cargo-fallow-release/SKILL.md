---
name: cargo-fallow-release
description: Release cargo-fallow, bump its version, publish the crate, tag a version, or publish the GitHub Action to the Marketplace. Use when asked to release or to cut a version.
---
<!-- Generated from .agents/skills. Do not edit. -->

# cargo-fallow release

This skill runs `docs/release-procedure.md` from the first check to the
Marketplace listing. The procedure is canonical. When this skill and the
procedure disagree, follow the procedure and update this skill.

release-contract: fc131be9882bfaf8f42592ccab40e0511f42c5ff11d822313a30a0113fc6b962

`.github/scripts/release-skill.test.mjs` fails when a contract file changes
and this pin stays the same. Read the change, update the steps below, then
set the pin to the hash that the test prints.

## Rules

- The invocation is the approval for the release and the version bump. Ask
  the owner only for a `Breaking` entry, and for the three approvals of the
  `release` environment.
- Take the version from `$VERSION` and `$TAG`, never from `HEAD`.
- Sign each commit with `git commit -S -s` and each tag with `git tag -s`.
- Merge each pull request by hand with `--match-head-commit`. In a T3 Code
  thread, arm no auto-merge.

## Steps

### 1. Preflight

```bash
git fetch -q --force origin --tags
git status --porcelain
node scripts/verify-release-ci.mjs --sha "$(git rev-parse origin/main)" --dispatch-missing
ls changelog.d/*.md | grep -v README.md
```

The status must be empty, and the CI check must pass. The check starts a
`Corpus` run on `main`, because a weekly run does not count. Step 8 of the
procedure gives the fix for each cause of a failed `Corpus` run. The last command
lists the fragments of the release. At least one fragment must exist, except
in a resume: `Cargo.toml` already has the new version, `CHANGELOG.md` has its
section, and the tag is absent. Then skip step 2, and in step 4 use the
recovery for a commit that landed on top of the release commit.

Done when: each check passes.

### 2. Version

Read the `###` headings of the fragments. Before 1.0.0:

- `Breaking` or `Removed` gives a minor bump. Ask the owner first.
- `Added`, `Changed` or `Deprecated` gives a minor bump.
- Only `Fixed`, `Performance` or `Security` gives a patch bump.

Set `VERSION` and `TAG="v$VERSION"`. Make sure that
`git ls-remote --tags origin "refs/tags/$TAG"` prints nothing.

Write `.plans/release-$TAG-in-flight.md` with the version, and add each
later ID to it: the pull request, the release commit and the run ID. A
resumed session reads this file first.

Done when: the in-flight file exists and names the version.

### 3. Freeze `main`

From the release merge until the tag push, `main` must not change. The
`plan` job stops when `origin/main` is not the release commit.

```bash
gh pr list --state open -L 200 --json number,title,autoMergeRequest \
  -q '.[] | select(.autoMergeRequest != null) | "\(.number) \(.title)"'
```

Disable auto-merge on each listed pull request with
`gh pr merge <N> --disable-auto`, and record the numbers in the in-flight
file. Tell the owner to stop each other session that can merge to `main`.

Done when: no open pull request has auto-merge, and the owner confirmed the
freeze.

### 4. Release commit

Follow "Prepare the release commit" in the procedure, on the branch
`chore/release-$TAG`. Merge with the exact subject `chore: release $TAG`.
When another commit lands on top of the release commit, follow the recovery
in step 7 of that section.

Done when: `origin/main` is the release commit and its CI passed with
`node scripts/verify-release-ci.mjs --sha <SHA> --dispatch-missing`.

### 5. Release notes

Follow "Write the release notes" in the procedure. Group the entries by what
the user can do now. Run the `unslop` pass. Check each flag and key name in
the source.

Done when: the notes file has no em dash and ends with the compare URL.

### 6. Release run

Follow "Run the release" in the procedure: the dry run, then the real run.
Give the owner the run URL. The owner approves the `release` environment
for `attest`, `sign` and `publish-crate`.

Done when: `Release ready for signed tag` is `success` on a run whose
`headSha` is the release commit.

### 7. Tag and GitHub release

Run `scripts/publish-github-release.mjs` from the procedure, first with
`--check-only`, then without it. Make sure that the `release-published.yml`
run passes.

Done when: `gh release view $TAG` shows the release with all assets.

### 8. Rolling tag and Marketplace

1. Follow "Move the rolling tag" in the procedure.
2. Give the owner the edit URL
   `https://github.com/fallow-rs/cargo-fallow/releases/edit/$TAG` and the
   steps of "Publish the action to the GitHub Marketplace".
3. After the owner reports the publish, open the Marketplace link on the
   release page, and make sure that the listing shows `$TAG`.

Done when: `v0` is the release commit and the Marketplace page exists.

### 9. Close out

1. Lift the freeze. Tell the owner which pull requests in the in-flight
   file had auto-merge. Outside a T3 Code thread, arm it again.
2. Make sure that `cargo search cargo-fallow --limit 1` shows `$VERSION`.
3. Close each issue that the release resolves. Show each comment draft to
   the owner before you post it.
4. Mark the in-flight file complete, and delete the release branch.

Done when: the crate, the release, the tags and the Marketplace page are
live, and the report names each step that the owner did.
