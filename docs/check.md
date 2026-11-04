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
| `--locked`, `--offline`, `--frozen` | Pass the flag to the `cargo metadata` call of `deps`. |

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
- The `pub` items of a crate in `exclude-crates` of `visibility` do not count.

`formula_version` is `2`. The maintainers increment it when they change a
weight, a cap or an input.

The score and the grade need all six checks. A run without one of them has no
`score` and no `grade`, for example a run with `--only deps,visibility`. In
that case, the `missing_checks` array names the checks that the run did not
include, in the order of `checks`. The human output then says
`Health (rust): no score.` The report still has the penalties of the checks
that ran. When no check with a penalty runs, for example with `--only parse`,
the report has no `health` object.
