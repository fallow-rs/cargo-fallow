# Fix

`cargo fallow fix` applies the actions with `auto_fixable: true` of the
`deps` and the `visibility` checks.

```bash
cargo fallow fix --dry-run                 # show the edits, write nothing
cargo fallow fix --dry-run --format json   # the edits as a list
cargo fallow fix --yes                     # apply the edits
```

- It removes each `unused-dependency`, `unused-dev-dependency`,
  `unused-build-dependency` and `unused-workspace-dependency` entry, also in a
  `[target.<platform>.*]` table and in the `[dependencies.<key>]` form.
- It moves each `misplaced-dependency` entry to the `[dev-dependencies]` table
  of the same platform. When `[dev-dependencies]` has the entry already, it
  removes the entry.
- When no dependency table of the package has the key after the edit, it
  removes `dep:<key>`, `<key>/<feature>` and `<key>?/<feature>` from each
  feature.
- It changes the visibility keyword of each `visibility` finding (fast
  mode) at the `range` of its action: `pub` becomes `pub(crate)`, and a
  restricted keyword goes. The edit counts characters, so a wide character
  before the keyword on the same line does not move it. The analysis holds the
  target of a narrowed `pub use`, so each change stays valid with the others.
- With a visibility edit, the command runs `cargo check --workspace
  --all-targets --all-features` before the edits, and without
  `--all-features` when that build fails. When neither build passes, the
  command changes nothing and exits 2. After the edits, it runs the same check
  again.
- When the check after the edits fails, the command writes back each file and
  `Cargo.lock`. It then leaves out the edits of the items that the compiler
  errors name, and tries again, at most five times. The report gives each
  left-out edit `applied: false`. When no edit is left to leave out, the
  command exits 2 with the compiler errors.
- The check builds one platform. Code under a `cfg` for another platform, and
  doc tests, are not part of it. An edit can break such code. Run the tests of
  each platform after `fix`, or use `--dry-run` and review the edits.
- A stop of the process during the check leaves the edits on disk. Use the
  version control of the workspace to undo them.
- These findings stay for a person, with `auto_fixable: false`:
  - a warning: `unused-optional-dependency`, `feature-only-dependency` and
    `redundant-ignore`,
  - an entry that turns on features (`features = [...]`): Cargo unifies the
    features of a package, so another dependency can need them,
  - a misplaced entry that a feature of the package names,
  - a workspace entry that such a kept member entry inherits.
- The edit keeps the comments and the format of each `Cargo.toml`. A comment
  above a removed or moved entry goes with the entry.
- After the edits, the command loads the workspace with `cargo metadata`
  again. When the load fails, it writes back the old bytes of each file and
  exits 2 with the `CARGO_FALLOW_FIX_FAILED` code.
- A process without a terminal on stdin needs `--yes`. Without it, the
  command changes nothing and exits 2. In a terminal, the command applies the
  edits also without `--yes`, the same as the JavaScript CLI.
- The command writes each file through a new file and a rename, so a stop
  leaves the old file or the new file, not a part.
- `Cargo.lock` stays as it was. The next `cargo` build updates it.

The JSON report has `dry_run`, `fixes`, `total_fixed` and `skipped` (the
findings that `fix` does not change). Each entry of `fixes` has `type`
(`remove-dependency` or `move-to-dev-dependencies`), `rule`, `package`,
`manifest_path`, `section`, `platform`, `dependency`, `line` and `applied`.
A `change-visibility` entry has `type`, `rule` (the finding kind),
`package`, `item`, `path`, `line`, `column`, `from`, `to` and `applied`.
`skipped_packages` names the packages that `deps` did not judge.
