# Changelog fragments

A pull request does not edit `CHANGELOG.md`. It adds its entries in a new file
in this directory. Two pull requests then never change the same lines, so the
merge of one does not make the other conflict.

## Write a fragment

1. Make one file for each pull request, for example
   `changelog.d/<branch-name>.md`, with `-` for each `/`. The check reads
   only the top level of this directory. Use a name that no other open pull request
   uses.
2. Start each group of entries with a `###` heading. Use only these headings:
   `Breaking`, `Added`, `Changed`, `Deprecated`, `Removed`, `Fixed`,
   `Performance`, `Security`. Use each heading one time in a file.
3. Write each entry in the format of `CHANGELOG.md`: one list item that tells
   the user what changed. Do not use an em dash.

```markdown
### Fixed

- `cargo fallow deps` no longer reports a dependency that only a build
  script uses.
```

To change an entry later, edit the fragment file. Do not edit the
`[Unreleased]` section of `CHANGELOG.md`. The `Changelog fragments` CI step
fails a pull request that changes it. A change that a user cannot see, for
example a change to CI only, needs no fragment.

## Check and release

- `node scripts/changelog-fragments.mjs check` checks each fragment.
- `node scripts/changelog-fragments.mjs check --base origin/main` also checks
  that the branch does not edit `[Unreleased]`. It also fails on a new
  `## [X.Y.Z]` heading, except in a branch with the commit
  `chore: release vX.Y.Z`. The pre-commit hook runs it with
  `--pending-release`, which also accepts the heading of the `Cargo.toml`
  version.
- `node scripts/changelog-fragments.mjs collect --dry-run` prints the
  `[Unreleased]` section that the fragments make.
- The release commit runs `node scripts/changelog-fragments.mjs collect`. The
  command moves each fragment into `[Unreleased]` in merge order, merges each
  heading into one, and deletes the fragment files.
