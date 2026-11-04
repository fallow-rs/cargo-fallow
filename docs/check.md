# The check command

`cargo fallow check` runs each static check in one run and writes one report
with a health score. `cargo fallow` with no command runs `check`.

```bash
cargo fallow                      # same as `cargo fallow check`
cargo fallow check --format json --quiet
cargo fallow check --manifest-path path/to/Cargo.toml
cargo fallow check --fail-on-issues
cargo fallow check --only deps,visibility
cargo fallow check --skip dupes --format codeclimate > gl-code-quality.json
```

You can also run the binary directly: `cargo-fallow check`.

`cargo fallow check` runs the static checks in one run: `parse`, `deps`,
`files`, `visibility` (fast mode), `boundaries`, `size`, `complexity` and `dupes`. The
run reads `cargo metadata` once, builds the module graph once and parses each
source file once for the structure checks. Each check gives the same result as
its own command, and keeps its own gate. `mutants` and `visibility --precise`
are not part of the run, because they build and run the tests or rust-analyzer
and take minutes. Run them as separate commands.

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
| `--save-baseline <PATH>`, `--baseline <PATH>`, `--fail-on-stale-baseline` | Save the findings, or leave out the saved findings. See [Baselines](adoption.md#baselines). |
| `--package <NAME>`, `-p <NAME>` | Report only the findings of this workspace member. The flag can repeat. See [Packages](adoption.md#packages). |
| `--locked`, `--offline`, `--frozen` | Give the flag to the `cargo metadata` call of `deps`. |

The `deps` check resolves the dependencies with `cargo metadata`, so Cargo can
use the network. `Cargo.lock` stays as it was before the run. Use `--offline`,
or `--skip deps` for a run that does not resolve the dependencies.

The configuration of each check is the same as for its command, in
`[workspace.metadata.fallow.<check>]`. A `check` run has no flag that changes
a limit: change the table instead. The `size` check has no old version to
compare with in a `check` run, so its `file-growth` gate is `skipped`. Use
`cargo fallow size --base <REF>` for the growth rule.

## Health score

The report has a health score for the Rust code. The model is the health score
of the JavaScript CLI: the score starts at 100, each penalty subtracts points,
the score is rounded to one decimal and clamped to 0 to 100. The grade comes
from the whole number: `A` from 85, `B` from 70, `C` from 55, `D` from 40, else
`F`. The score has `language: "rust"`. The tool never writes one score for more
than one language.

| Penalty | Check | Input | Points | Cap |
| --- | --- | --- | --- | --- |
| `dead_public` | `visibility` | `dead_public` findings per `pub` item, in percent | percent x 0.2 | 15 |
| `complexity` | `complexity` | functions with cyclomatic complexity of 50 or more, in percent | percent x 4 | 20 |
| `unit_size` | `size` | functions over 60 lines, per 1000 functions | per mille x 0.2 | 10 |
| `unused_deps` | `deps` | `error` findings, per dependency entry of the members, in percent | percent x 0.5 | 25 |
| `boundary_violations` | `boundaries` | violations, per source file, in percent | percent x 0.5 | 25 |
| `duplication` | `dupes` | duplicated lines, in percent | percent - 5 | 10 |

The JavaScript CLI counts unused dependencies per 1000 files. A Rust workspace
has fewer and larger files, so that rate gives the full penalty for two unused
dependencies in 30 files. The Rust formula counts them against the declared
dependencies instead. Rust functions are longer than JavaScript functions, so
the `unit_size` weight is 0.2, not 0.5. [`docs/health-score.md`](health-score.md)
has the corpus data for this weight. Each input is rounded to one decimal before its weight,
and each penalty after it. A check that did not run gives no penalty, and its key is not in
`penalties`. The `metrics` object holds the inputs. A function counts when no
`exclude` list of `size` or `complexity` names its file. The `pub` items of a
crate in `exclude-crates` of `visibility` do not count. `formula_version` is
`2`. A change of a weight, a cap or an input increments it.

The score and the grade need all six checks. A run without one of them, for
example `--only deps,visibility`, has no `score` and no `grade`. Its
`missing_checks` array names the checks that the run did not include, in the
order of `checks`, and the human output says `Health (rust): no score.` The
penalties of the checks that ran stay in the report. A run without a check
that has a penalty, for example `--only parse`, has no `health` object.
