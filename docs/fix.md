# Fix

`cargo fallow fix` applies the actions with `auto_fixable: true` from the
`deps` and `visibility` checks.

```bash
cargo fallow fix --dry-run                 # show the edits, write nothing
cargo fallow fix --dry-run --format json   # the edits as a list
cargo fallow fix --yes                     # apply the edits
```

- The command removes each `unused-dependency`, `unused-dev-dependency`,
  `unused-build-dependency` and `unused-workspace-dependency` entry. This
  includes an entry in a `[target.<platform>.*]` table and an entry in the
  `[dependencies.<key>]` form.
- It moves each `misplaced-dependency` entry to the `[dev-dependencies]` table
  of the same platform. When that `[dev-dependencies]` table already has the
  entry, the command only removes the misplaced entry.
- When no dependency table of the package has the key after the edit, the
  command also removes `dep:<key>`, `<key>/<feature>` and `<key>?/<feature>`
  from each feature.
- For each `visibility` finding (fast mode), the command changes the
  visibility keyword at the `range` of its action. `pub` becomes
  `pub(crate)`, and the command removes a restricted keyword. The edit counts
  characters, not bytes, so a wide character before the keyword on the same
  line does not put the edit at the wrong place.
- When a `pub use` has a finding, the analysis does not report the target of
  that `pub use` in the same run. Each change is then valid together with the
  other changes. The next run reports the target.
- With a visibility edit, the command runs `cargo check --workspace
  --all-targets --all-features` before the edits. When that build fails, the
  command runs it again without `--all-features`. When neither build passes,
  the command changes nothing and exits 2. After the edits, the command runs
  the same check again.
- When the check after the edits fails, the command writes back the old bytes
  of each file and of `Cargo.lock`. Then it leaves out the edits of the items
  that the compiler errors name, and tries again. The command does at most
  five rounds of edits and checks. In the report, each left-out edit has
  `applied: false`. When there is no edit left to leave out, the command
  exits 2 with the compiler errors.
- `cargo check` builds for one platform. It does not build code under a `cfg`
  for another platform, and it does not build doc tests. An edit can break
  such code. After `fix`, run the tests on each platform, or use `--dry-run`
  and review the edits.
- When the process stops during the check, the edits stay on disk. To undo
  them, use the version control of the workspace.
- `fix` does not change these findings, and they have `auto_fixable: false`.
  A person must fix them:
  - a warning: `unused-optional-dependency`, `feature-only-dependency` and
    `redundant-ignore`,
  - an entry that turns on features (`features = [...]`), because Cargo
    unifies the features of a package and another dependency can need them,
  - a misplaced entry that a feature of the package names,
  - a workspace entry that one of these member entries inherits.
- The command keeps the comments and the format of each `Cargo.toml`. When the
  command removes or moves an entry, the comment above the entry is removed
  or moved with it.
- After the edits, the command loads the workspace again with
  `cargo metadata`. When the load fails, the command writes back the old bytes
  of each file and exits 2 with the `CARGO_FALLOW_FIX_FAILED` code.
- When stdin is not a terminal, the command needs `--yes`. Without `--yes`,
  the command changes nothing and exits 2. A run with `--dry-run` does not
  need `--yes`. In a terminal, the command applies the edits also without
  `--yes`, the same as the JavaScript CLI.
- The command writes each file to a new file, and then renames the new file
  to the old name. When the process stops, the disk has the old file or the
  new file, and never a partly written file.
- The command does not write `Cargo.lock` itself. The `cargo metadata` and
  `cargo check` calls after the edits can update it. When the command fails,
  it writes back the old `Cargo.lock`.

The JSON report has `dry_run`, `fixes`, `total_fixed` and `skipped` (the
findings that `fix` does not change). A dependency entry of `fixes` has
`type` (`remove-dependency` or `move-to-dev-dependencies`), `rule`,
`package`, `manifest_path`, `section`, `platform`, `dependency`, `line` and
`applied`. A `change-visibility` entry has `type`, `rule` (the finding kind),
`package`, `item`, `path`, `line`, `column`, `from`, `to` and `applied`.
`skipped_packages` names the packages that `deps` did not check.
