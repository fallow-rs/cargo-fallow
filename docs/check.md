# The check command

`cargo fallow check` runs all static checks and writes one report with a
health score. When you give no command, `cargo fallow` runs `check`.

```bash
cargo fallow                      # same as `cargo fallow check`
cargo fallow check --format json --quiet
cargo fallow check --manifest-path path/to/Cargo.toml
cargo fallow check --fail-on-issues
cargo fallow check --only deps,visibility
cargo fallow check --skip dupes --format codeclimate > gl-code-quality.json
```

You can also run the binary directly: `cargo-fallow check`.

The command runs these static checks: `parse`, `deps`, `files`, `visibility`
(fast mode), `boundaries`, `size`, `complexity` and `dupes`. It reads
`cargo metadata` and builds the module graph one time for all of them. For
the structure checks, it parses each source file one time. The result of a
check is the same as the result of its own command, and the check has its own
gate.

`check` does not run `mutants` or `visibility --precise`. These commands build
and run the tests or rust-analyzer, and they take minutes. Run them as
separate commands.

| Flag | Meaning |
| --- | --- |
| `--manifest-path <PATH>` | The `Cargo.toml` of the workspace. The default is the manifest that Cargo finds from the current directory. |
| `--format human\|json\|sarif\|codeclimate\|compact\|markdown\|github-annotations\|github-summary\|badge` | The output format. The default is `human`. `badge` is an SVG badge with the health grade (see [Output formats](output.md#output-formats)). |
| `--quiet` | Hide the progress lines on stderr. |
| `--only <CHECK>,...` | Run only these checks. |
| `--skip <CHECK>,...` | Do not run these checks. |
| `--fail-on-issues` | Enforce the gate of each check that ran. |
| `--fail-on-parse-error` | Enforce only the `parse-error` gate. |
| `--changed-since <REF>` | Report only the findings in the files that changed since the fork point of `REF`. See [Changed files](adoption.md#changed-files). |
| `--save-baseline <PATH>`, `--baseline <PATH>`, `--fail-on-stale-baseline` | Save the findings to a file, or leave out the findings that a saved file lists. See [Baselines](adoption.md#baselines). |
| `--package <NAME>`, `-p <NAME>` | Report only the findings of this workspace member. You can repeat the flag. See [Packages](adoption.md#packages). |
| `--locked`, `--offline`, `--frozen` | Give the flag to each `cargo metadata` call. Only the call of `deps` resolves the dependencies, so only that call can use the network. |
| `--file-scores` | Add `file_scores`. See [File scores and hotspots](#file-scores-and-hotspots). |
| `--hotspots` | Add `hotspots` and `hotspot_summary`. See [File scores and hotspots](#file-scores-and-hotspots). |
| `--since <DURATION>` | The git history window of `--hotspots`: `6m`, `90d`, `2w`, `1y` or a date such as `2025-06-01`. The default is `6m`. |
| `--min-commits <N>` | The least number of commits in the window for a hotspot. The default is 3. |
| `--save-snapshot [PATH]` | Save a health snapshot. See [Snapshots and trend](#snapshots-and-trend). |
| `--trend`, `--trend-from <PATH>` | Add `health_trend`: the change against a snapshot. |

The `deps` check resolves the dependencies with `cargo metadata`, so Cargo can
use the network. The run does not change `Cargo.lock`. For a run without
network access, use `--offline`. For a run that does not resolve the
dependencies, use `--skip deps`.

Each check uses the same configuration as its command, in
`[workspace.metadata.fallow.<check>]`. `check` has no flag that changes a
limit. To change a limit, edit the table. In a `check` run, the `size` check
has no old version to compare with, so its `file-growth` gate is `skipped`.
For the growth rule, run `cargo fallow size --base <REF>`.

## Health score

The report has a health score for the Rust code. The score uses the model of
the health score in the JavaScript CLI. The score starts at 100, and each
penalty subtracts points. The result is rounded to one decimal and clamped to
the range 0 to 100. The whole number part of the score gives the grade: `A`
from 85, `B` from 70, `C` from 55, `D` from 40, and `F` below 40. The score is
for Rust code only, and it has `language: "rust"`. cargo-fallow does not
compute one score over Rust and another language.

| Penalty | Check | Input | Points | Cap |
| --- | --- | --- | --- | --- |
| `dead_public` | `visibility` | `dead_public` findings per `pub` item, in percent | percent x 0.2 | 15 |
| `complexity` | `complexity` | functions with cyclomatic complexity of 50 or more, in percent | percent x 4 | 20 |
| `unit_size` | `size` | functions over 60 lines, per 1000 functions | per mille x 0.2 | 10 |
| `unused_deps` | `deps` | `error` findings, per dependency entry of the members, in percent | percent x 0.5 | 25 |
| `boundary_violations` | `boundaries` | violations, per source file, in percent | percent x 0.5 | 25 |
| `duplication` | `dupes` | duplicated lines, in percent | percent - 5 | 10 |

The JavaScript CLI counts unused dependencies per 1000 files. A Rust
workspace has fewer and larger files. With the JavaScript rate, two unused
dependencies in 30 files give the full penalty. For that reason, the Rust
formula divides the unused dependencies by the declared dependencies.

Rust functions are longer than JavaScript functions, so the `unit_size`
weight is 0.2, not 0.5. [`docs/health-score.md`](health-score.md) has the
corpus data for this weight.

The code rounds each input to one decimal before it applies the weight. It
rounds each penalty again after the weight. When a check did not run, it gives
no penalty, and `penalties` has no key for that check. The `metrics` object
has the inputs.

Some code does not count:

- The function count of `size` leaves out the files in the `exclude` list of
  `size`. The function count of `complexity` leaves out the files in the
  `exclude` list of `complexity`.
- The `pub` items of a crate in `exclude-crates` of `visibility`, and of a
  library that a registry can publish, do not count.

`formula_version` is `2`. The maintainers increment it when they change a
weight, a cap or an input.

The score and the grade need all six checks. A run without one of them has no
`score` and no `grade`, for example a run with `--only deps,visibility`. In
that case, the `missing_checks` array names the checks that the run did not
include, in the order of `checks`. The human output then says
`Health (rust): no score.` The report still has the penalties of the checks
that ran. When no check with a penalty runs, for example with `--only parse`,
the report has no `health` object.

## File scores and hotspots

`--file-scores` and `--hotspots` need the complexity check. A run that leaves
it out exits 2. The model is `fallow health` for JavaScript.

`file_scores` has one entry for each source file that the complexity check
measures, the least maintainable first. Each entry has `path`,
`maintainability_index`, `complexity_density` (the cyclomatic complexity of
all functions per line), `total_cyclomatic`, `total_cognitive`,
`function_count` and `lines`. A run with the visibility check adds
`dead_code_ratio`: the `dead_public` findings per `pub` item of the file.

```text
maintainability_index = 100 - density x 30 x min(lines / 50, 1) - dead_code_ratio x 20
```

The index is clamped to 0..100. The JavaScript formula also has a fan-out
term. The Rust formula leaves it out.

`hotspots` ranks the files that are complex and change often, the riskiest
first. The run reads `git log --numstat` for the `--since` window, without
merge commits. A commit of 90 days ago counts half, so `weighted_commits`
favors recent work. A file needs `--min-commits` commits and some complexity:
a file with no branch is not a hotspot. The score is the
weighted commits times the complexity density, each divided by the largest
value of the ranked files, on a scale of 0 to 100. Each entry has `path`,
`score`, `commits`, `weighted_commits`, `lines_added`, `lines_deleted`,
`complexity_density` and `trend`. The trend compares the commits in the newer
half of the `--since` window with the commits in the older half:
`accelerating` over 1.5 times, `cooling` under 0.67 times, else `stable`. A
month counts 30 days. A path can hold any character, because the run reads
the git log with NUL separators.
`hotspot_summary` has `since`, `min_commits`, `files_analyzed`,
`files_excluded` and `shallow_clone`.

A shallow clone has a cut history, so the run gives a warning and no hotspot.
In GitHub Actions, use `fetch-depth: 0`.

## Snapshots and trend

`--save-snapshot` writes the health score, the grade, the penalties and the
issue count of the run to a JSON file with the `kind` `health-snapshot`.
Without a path, the file goes to `.fallow/snapshots/` in the workspace root,
named by the time of the run, for example `2026-11-04T06-30-59Z.json`. A
second run in the same second adds a suffix, so no file is replaced. A
relative path is relative to the current folder. The snapshot also records
the checks of the run and whether a scope (`--changed-since`, `--package` or
`--baseline`) kept part of the findings.

`--trend` compares the run with the newest snapshot in `.fallow/snapshots/`.
Only the file names that `--save-snapshot` gives count. A file that does not
read gives a warning, and the next older file counts.
`--trend-from <PATH>` compares it with one file. `health_trend` has
`compared_to` (the time, the commit, the score and the grade of the
snapshot), `snapshots_loaded`, `overall_direction` and `metrics`. Each metric
has `name` (`score`, a penalty name or `total_issues`), `previous`, `current`,
`delta`, `direction` and `unit`. A higher score is better. A lower penalty or
issue count is better. A change under 0.05 is `stable`. Without a snapshot,
the run gives a warning and no `health_trend`. When the snapshot has another
formula version, the trend compares only `total_issues`. `warnings` names
each difference between the runs: the formula version, the checks and the
scope. An invalid `--trend-from` file stops the run with exit code 2 before
the analysis. A run with `--trend` and
`--save-snapshot` reads the old snapshot first.

