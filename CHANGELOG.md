# Changelog

All notable changes to `cargo-fallow` are in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

A pull request does not edit this file. It adds a fragment to `changelog.d/`,
and the release commit collects the fragments. Each release needs a
`## [X.Y.Z] - YYYY-MM-DD` section, and the owner writes the release notes from
it.

## [Unreleased]

## [0.2.0] - 2026-11-05

### Added

- `cargo fallow fix` formats each changed file with `rustfmt` when `rustfmt`
  formatted it before the edits, so `cargo fmt --check` still passes.

- `cargo fallow fix --only <CHECK>` and `--skip <CHECK>` limit the edits to
  some checks. The values are `deps` and `visibility`. Each flag takes a list
  with commas and can repeat. A `--skip` of both checks exits 2.

- `[workspace.metadata.fallow.complexity] cyclomatic-policy = "flat-match"`
  counts a flat `match` as one decision for the `max-cyclomatic` limit. A
  flat `match` has no guard and no decision in an arm, for example a lookup
  table. A finding then has `standard_cyclomatic` with the standard count.
  The default stays `standard`, and the health score does not change.

- `cargo fallow visibility` has the `--public-crate` flag and the
  `public-crates` key of `[workspace.metadata.fallow.visibility]`. When they
  name libraries, only those libraries are an external API. Each other
  library gets each rule, also when a registry can publish it. A name can
  have `*` as a glob. fallow for TypeScript names the same option
  `publicPackages`.

- `cargo fallow complexity --coverage <LCOV>` and `cargo fallow check
  --coverage <LCOV>` give each function a CRAP score from its cyclomatic
  complexity and its line coverage. A function at or over
  `[workspace.metadata.fallow.complexity] max-crap` (default 30) is a
  `complexity-crap` finding. `--coverage-root <PATH>` maps the `SF` paths of
  an LCOV file from another machine.

- `[workspace.metadata.fallow.dupes] production = true` and
  `cargo fallow dupes --production` leave test code out of the clone search.
  Test code is a test target, a test module and an item with a test
  attribute, for example `#[cfg(test)]` or `#[test]`. The line counts and the
  `threshold` then count only production code. The default stays `false`.
  `--no-production` keeps test code.

- `cargo fallow check` accepts `--base <REF>` and `--staged`, and gives them to
  the size check. The `file-growth` gate then runs, and `--fail-on-issues`
  enforces it, so one `check` run covers each gate.

- A `boundaries.sources` rule takes `scope = "production"`. The rule then
  skips test targets, test modules and items with a test attribute, for
  example `#[cfg(test)]` or `#[test]`.
- A `boundaries.sources` rule takes `allow = [{ path, max }]`. A listed file
  passes up to `max` matches of the rule. Each match over `max` is a finding.

- The GitHub Action passes the `base` input to `check` as `--base`, so the
  `file-growth` gate runs in a `check` step.

- `cargo fallow boundaries` has item rules
  (`[[workspace.metadata.fallow.boundaries.items]]`) and glob re-export rules
  (`[[workspace.metadata.fallow.boundaries.reexports]]`). An item rule keeps
  the code of some packages from a workspace item, with the path resolved
  through `use`, aliases and re-exports. `allow-via` keeps a use through a
  named module allowed. A glob re-export rule finds a `pub use <path>::*`
  that reads a denied crate or module. The findings have the kinds
  `item-path` and `glob-reexport`, and the summary counts the uses that do
  not resolve in `unresolved`.

- `cargo fallow api --base <REF>` reports the public API changes of each
  published library between the merge base of the ref and HEAD: `added`,
  `removed` and `changed` items, and `crate-added`, `crate-removed` and
  `feature-removed`. The `api-removals` gate counts a removal when the
  breaking part of the version did not change. The GitHub Action runs it with
  `command: api` and the `base` input.

- `[[workspace.metadata.fallow.boundaries.cfg]]` rules list `cfg` builds,
  for example `builds = [["test"], ["test", "miri"]]`. For each build, the
  rule reports a path from code that the build compiles to an item that the
  build does not compile. The kind is `cfg-path`. An option that no build
  names is unknown, and then the rule gives credit.

- `cargo fallow deps` has the opt-in `feature-unification-drift` rule. For each
  member that `unify-features` in `[workspace.metadata.fallow.deps]` names, it
  compares the features that `cargo build -p <member>` and
  `cargo build --workspace` turn on for each shared crate. Each finding names
  the crate, the version and the features that only one build turns on.

### Changed

- The SARIF level of a finding now follows its severity: `error` gives
  `error`, and `warn` gives `warning`. Before, an `error` finding was a
  SARIF `warning` when its gate was not enforced. A gate flag no longer
  changes the level. This is the same rule that fallow uses.
- A `deps` warning, for example `unused-optional-dependency`, is now a SARIF
  `warning`. Before, it was a SARIF `note`.
- The `github-annotations` and `github-summary` formats and the GitHub Action
  use the same level. A surviving mutant is now always an `error`.

- With `code-duplication = "off"`, the `duplication-threshold` gate still
  judges the percentage, and the output has no clone group. A workspace can
  now gate the percentage without a warning for each clone group. Before, the
  rule at `off` removed the gate too. A workspace that used `off` to drop the
  gate must now remove `threshold` from `[workspace.metadata.fallow.dupes]`.

### Fixed

- `cargo fallow mutants` now gives `--locked`, `--offline` and `--frozen` to
  each cargo call in the copies: the builds, the test lists, the doctests and
  the `cargo metadata` calls. Before, only the first `cargo metadata` call got
  the flag, so a build in a copy could use the network. With `--offline` or
  `--frozen`, these calls and the test runs also get `CARGO_NET_OFFLINE=true`.

- `cargo fallow visibility` no longer reports a type that the signature of an
  item that stays `pub` names, for example the return type of a `#[napi] pub
  fn`. Before, the `pub(crate)` change made rustc warn with
  `private_interfaces`.
- `cargo fallow visibility` gives credit to an intra-doc link in a doc
  comment, a `#[doc]` attribute or an included documentation file. The link
  is a use from the module of the documentation, so `fix` no longer breaks
  `cargo doc`. The trace shows the use as `doc-link`.
- `cargo fallow deps` counts an intra-doc link to a dependency as a use.

- `cargo fallow visibility` no longer reports `unnecessary-public` for a
  `#[cfg(test)] pub` item in a private module. There, `pub` already gives
  crate visibility, and `clippy::redundant_pub_crate` rejects `pub(crate)`.
  When only its own module uses the item, the check reports
  `unnecessary-restricted-visibility`, so `fix` makes it private.
- `cargo fallow visibility` no longer reports an item that a `use` with a
  written visibility re-exports, for example `pub(crate) use self::item as
  alias;`. rustc rejects the re-export of a private item.

- `cargo fallow visibility` reads more forms of intra-doc links: a `Self::`
  path (also in the documentation of a field or a variant), a turbofish such
  as `Gen::<u8>::new`, an indented paragraph, and an included file with a
  path from `env!("CARGO_MANIFEST_DIR")` or from more than one `concat!`
  literal.
- `cargo fallow visibility` no longer reports a type that macro tokens name
  when the macro can write a `pub` item: the tokens hold `pub` or a `vis`
  fragment.

- `cargo fallow fix` leaves out an edit that adds a compiler warning, the same
  as an edit that adds an error. Before, a workspace that builds with
  `-D warnings` could break after `fix` exited 0. A new `dead_code` warning
  does not count, because a `dead-public` edit shows the dead code on
  purpose.

- The GitHub Action fails the step when `base: auto` cannot read the base
  commit and the command needs the base for a gate (`check`, `size` and
  `mutants`). Before, the action wrote a notice and ran with no base, so the
  growth gate passed without a check. A run with no pull request base and no
  push stays a notice.

- The description of the GitHub Action now has fewer than 125 characters, so
  the GitHub Marketplace accepts the action.

- The GitHub Action and the GitLab template no longer post a first pull
  request comment or merge request note for a clean run. A clean run still
  updates an old comment, and an unchanged comment gets no update.

### Performance

- `cargo fallow check` parses each file one time for the module graph and the
  `files` check. Before, the `files` check read and parsed each file a second
  time. On a large workspace, the run uses about 14% less CPU time. The output
  does not change.

- The bracket scan of intra-doc links is linear in the length of the text. A
  text with many open brackets no longer takes seconds.

- The memory of `cargo fallow visibility` now grows about linearly with the
  source size. A field access, a method call or a name in macro tokens gave
  one credit to each item with the name, so a generated file with many fields
  of one name needed memory for each pair. Each name now keeps its items once.
  On a generated 5 MB file, the peak memory goes from 8.9 GB to 1.5 GB, and
  the time from 33 s to 1.5 s.

## [0.1.0] - 2026-11-04

### Added

- Each command that reads the workspace with `cargo metadata` accepts
  `--locked`, `--offline` and `--frozen`: also `visibility`, `files`,
  `boundaries`, `size`, `complexity`, `dupes` and `mutants`. `init` accepts
  `--locked` and `--frozen` too.
- The `deps` rule `unused-feature` reports a feature of a `publish = false`
  package that turns on nothing, and that no other feature, no member, no
  target and no file of the repository names. It is a warning.
- `cargo fallow check --hotspots` ranks the files that are complex and change
  often, by git churn times complexity density. `--file-scores` adds the
  maintainability index of each file. `--save-snapshot` saves the health
  score, and `--trend` or `--trend-from` compares a run with a snapshot.
- `ci/gitlab-ci.yml` is a GitLab CI template. The job writes the code quality
  report of the merge request widget, and with `CARGO_FALLOW_COMMENT` posts or
  updates one merge request note with the markdown report.
- `--trace` shows why a check keeps or reports one item.
  `visibility --trace <crate>::<path>` lists each use and each credit of an
  item, with the verdict. `deps --trace <dependency>` lists each declaration
  and each use site. `dupes --trace <fingerprint>` lists the instances of one
  clone group. The JSON documents have the kinds `visibility-trace`,
  `deps-trace` and `dupes-trace`.
- `cargo fallow check --format badge` writes an SVG badge with the health
  grade, or a `no score` badge for a run with no score.
- The GitHub Action has the inputs `changed-since`, `auto-changed-since`,
  `baseline` and `save-baseline`, and `comment` posts or updates one sticky
  pull request comment with the gates and the findings.
- `cargo fallow init` prints the configuration tables with the limits at the
  current maximum, and saves the current findings as a baseline. `--write`
  adds them to the root `Cargo.toml`. `--ci github` prints a workflow. The
  `baseline` key of `[workspace.metadata.fallow]` names the baseline of each
  run.
- `cargo fallow explain <rule>` gives the meaning, an example and the fix of a
  rule, also as JSON. `--explain` adds `_meta` with the rules of the findings
  to the JSON report of `check` and of each static command.
- One suppression model for each check: `// fallow-ignore-next-line <kind>`
  now also works for `visibility`, `size`, `boundaries` and `files`,
  `// fallow-ignore-file <kind>` hides the findings of a file, and
  `# fallow-ignore-next-line <rule>` in `Cargo.toml` hides a `deps` finding.
  A comment that hides no finding gives a `stale-suppression` warning, and
  each report has a `suppressed` count.
- `cargo fallow check` reads the workspace and reports the parse gates.
- `cargo fallow mutants` runs mutation tests with schemata and coverage
  selection.
- `cargo fallow deps`, `visibility`, `boundaries`, `size`, `complexity` and
  `dupes` report native checks of the workspace.
- JSON, SARIF and human output, with exit codes 0, 1 and 2.
- Each release archive has an Ed25519 signature (`.sig`). The GitHub Action
  installer checks it before the SHA-256 and fails closed. The installer now
  downloads the archive names that the release workflow writes.
- `cargo fallow fix` removes each unused dependency entry and moves each
  misplaced entry to `[dev-dependencies]`. `--dry-run` shows the edits. The
  edit keeps comments, and a failed load after the edit writes back the old
  bytes. The `deps` actions that `fix` applies now have `auto_fixable: true`.
- `cargo fallow fix` also narrows the visibility keyword of each `visibility`
  finding. It runs `cargo check --workspace --all-targets` before and after
  the edits, and writes back each file when the check after the edits fails.
  The `change-visibility` actions now have `auto_fixable: true`.
- `cargo fallow files` reports each `.rs` file in a target folder that no
  target compiles, with the rule `unused-file` and the gate `unused-files`.
  `check` runs it by default.
- `boundaries`, `size`, `complexity`, `dupes` and `mutants` accept
  `--fail-on-issues` as their gate flag. The older gate flags stay. The GitHub
  Action now gives `--fail-on-issues` to each command.
- `check`, `deps`, `visibility`, `boundaries`, `size`, `complexity` and
  `dupes` accept `--changed-since <REF>`. The report keeps only the findings
  in the files that changed since the fork point of the ref. The JSON report
  has a `changed_since` member.
- `--save-baseline <PATH>` and `--baseline <PATH>` on the same commands. A run
  with a baseline leaves out the saved findings, and the
  `stale-baseline` gate (`--fail-on-stale-baseline`) fails on an entry with no
  finding.
- `cargo fallow schema` prints the JSON Schema of each JSON document, and
  `docs/output-schema.json` holds a copy. A test fails when the two differ.
  The `check` report now has `schema_version` 1, the same as each other
  report.
- `--package <NAME>` (`-p`) on `check` and the static commands keeps only the
  findings of the named workspace members. The analysis still reads the whole
  workspace. An unknown name exits 2.
- `--format compact` gives one line for each finding,
  `path:line:col: rule: message`, and `--format markdown` gives a gate table
  and one table for each check. Each command has both formats, except `fix`
  and `mutants --inspect`.
- `[workspace.metadata.fallow.rules]` sets a rule to `error`, `warn` or `off`.
  The severity, the SARIF level and the gate count of its findings follow
  the level.
- `--format github-annotations` writes GitHub workflow commands, at most 50,
  the errors first, with paths from the repository root.
  `--format github-summary` writes a GitHub job summary. The annotations are
  the same text as the jq filter of the GitHub Action, and the summary shows
  the same in GitHub.

### Changed

- A finding that counts for a gate has the severity `error`: each finding of
  `files`, `visibility`, `boundaries`, `size`, `complexity`, `dupes` and the
  surviving mutants of `mutants`. Before, these findings had `warn` and still
  failed their gate. A rule set to `warn` in the rules table keeps the finding
  as a warning, and the warning does not fail the gate. The rules table
  cannot change `surviving-mutant`.
- The `visibility` rule ids are kebab-case in each output format:
  `dead-public`, `unnecessary-public` and `unnecessary-restricted-visibility`.
  The rules table, `kinds`, a suppression comment and `cargo fallow explain`
  still take the snake_case form. The finding ids did not change, so a saved
  baseline still matches.
- `visibility`, `check` and `fix` treat the exported items of a library that
  a registry can publish as an external API. They get no `dead_public` or
  `unnecessary_public` finding, and `fix` does not narrow them. A library with
  `publish = false` and a binary keep each rule. `cargo new` writes no
  `publish` key, so add `publish = false` to a library that no registry gets,
  to keep the old result. Each item that the API of a published or excluded
  library uses is now live.
- The reference for each command moved from the README to `docs/`. The
  README is now an overview with a logo. The `docs` address of
  `cargo fallow explain` and `_meta.docs` point to the new pages.

### Fixed

- The human output of `check` ends with the gates that fail and a next step.
  The gate block lists files only for the `parse-error` gate. The size
  section names `cargo fallow size --base <ref>`, a command that runs, and
  the dupes section no longer says "No clones found" when a baseline hides
  each clone group. `fix`, `init` and `explain` list only `human` and `json`
  in the help of `--format`.
- `fix` honors the rules table: a rule at `off` gets no edit. `fix` takes
  `--package` (`-p`) to keep only the edits of the named members. When the
  workspace does not build before the edits, the help names the next step.
- `size` skips a file whose first lines hold an `@generated` comment, as the
  `complexity`, `dupes`, `files` and `visibility` checks do.
- `init` does not write `max-file-lines`, because a larger limit only turns
  off the growth rule of `size`. Without `--write`, `init` no longer says
  that a baseline holds findings, and `baseline_findings` is `null`.
- The action input `base: auto` now takes the commit before a push on a push
  event. A new branch and a commit that the checkout cannot fetch give no base
  and a notice. The size self-test runs on push again.
- The `fingerprint` of a `dupes` clone group now depends only on the tokens of
  the group. Before, a new token in another file changed it.
- A `#[path]` attribute in an inline module now resolves from the module
  folder plus the inline module names, the same as rustc.
- A large valid source file no longer counts as too deep. The parse stack is
  sized from the nesting depth of the file, not from its size.
- The JSON `severity` of a finding is now `warn`, not `warning`. The fallow
  JSON output and the shared `GateStatus` use `warn`. SARIF keeps its own
  `warning` level.
- A `threshold` in `[workspace.metadata.fallow.dupes]` no longer fails the run
  on its own. The `duplication-threshold` gate fails the run only with its
  flag, the same as each other gate.
- `cargo fallow visibility` reports no finding in a generated file.
- The `unit_size` penalty of the health score reached its cap on six of
  seven corpus workspaces. Its weight is now 0.2, not 0.5, and
  `formula_version` is 2. `docs/health-score.md` has the data.
- A `check` run without one of the six checks of the health score, for
  example with `--only deps,visibility`, no longer reports a high score. The
  report has no score and no grade, and `health.missing_checks` names the
  checks that the run did not include.
