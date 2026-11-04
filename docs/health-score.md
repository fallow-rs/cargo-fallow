# Health score calibration

This page records how the Rust health score got its `unit_size` weight, and
why a partial run has no score. `check.md` describes the score for users.
The formula is in `src/health.rs`.

## The problem

Formula version 1 used the weight of fallow for JavaScript for `unit_size`:
0.5 points for each function over 60 lines per 1000 functions, with a cap of
10 points. With this weight, a workspace gets the full cap at 20 per 1000. Six
of the seven corpus workspaces got the full cap. Thus the penalty did not
separate good code from bad code.

## Method

1. Run `cargo fallow check --format json --quiet` on each workspace of the
   corpus.
2. Read `health.metrics.size_functions` (all functions and closures) and
   `health.metrics.large_functions` (functions over 60 lines).
3. Read `sections.size.summary.large_functions` for the length of each large
   function. This gives the counts over 80, 100 and 150 lines.
4. Compare the rate per 1000 functions of each workspace with the cap of
   each candidate weight.

The corpus is the M3 corpus: fallow `500709f1`, ripgrep `91f5d22e`, typos
`4c8b7f51`, git-cliff `4f65432b`, taplo `08f343be`, mdbook `7fb5bc8e` and
gitui `d7214ecc`. The run used a debug build of `main` at `c9745d4`.

## Data

| Workspace | Functions | Over 60 | Over 80 | Over 100 | Over 150 | Over 60 per 1000 | Over 60 per 1000, without tests |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| ripgrep | 3296 | 48 | 21 | 11 | 2 | 14.6 | 12.7 |
| fallow | 69264 | 1619 | 611 | 213 | 35 | 23.4 | 17.1 |
| typos | 797 | 22 | 12 | 8 | 5 | 27.6 | 15.1 |
| mdbook | 1077 | 30 | 17 | 8 | 5 | 27.9 | 22.3 |
| gitui | 2676 | 94 | 49 | 22 | 4 | 35.1 | 27.3 |
| taplo | 1111 | 46 | 23 | 17 | 12 | 41.4 | 39.6 |
| git-cliff | 674 | 28 | 23 | 16 | 14 | 41.5 | 38.6 |

"Without tests" leaves out the functions in a `tests/` or `benches/` folder
and the functions in a `tests` module. The score counts test code. Without
tests, the order of the workspaces is almost the same, so the formula keeps
the test code.

## Decision

- The limit stays at 60 lines. fallow for JavaScript uses the same limit,
  and so does the default of the `size` check. Thus the penalty and the size
  findings agree.
- The input is still the rate per 1000 functions.
- The weight changes from 0.5 to 0.2. The cap stays at 10 points, so a
  workspace now gets the full cap at 50 per 1000.
- `formula_version` changes from 1 to 2.

Rust functions are longer than JavaScript functions. In Rust, a `match` with
many arms, a builder or a parser loop is one function. The corpus median is
about 28 per 1000. With the weight 0.2, the median gets about half of the
cap, and the worst corpus workspace stays under the cap.

| Workspace | `unit_size`, version 1 | `unit_size`, version 2 | Score, version 1 | Score, version 2 |
| --- | ---: | ---: | ---: | ---: |
| ripgrep | 7.3 | 2.9 | 87.8 (A) | 92.2 (A) |
| fallow | 10.0 | 4.7 | 84.1 (B) | 89.4 (A) |
| typos | 10.0 | 5.5 | 78.8 (B) | 83.3 (B) |
| mdbook | 10.0 | 5.6 | 87.4 (A) | 91.8 (A) |
| gitui | 10.0 | 7.0 | 78.8 (B) | 81.8 (B) |
| taplo | 10.0 | 8.3 | 73.8 (B) | 75.5 (B) |
| git-cliff | 10.0 | 8.3 | 77.2 (B) | 78.9 (B) |

The unit test `unit_size_separates_the_corpus_workspaces` in `src/health.rs`
has the counts of this table.

## Partial runs

Each penalty comes from one check. A run with `--only` or `--skip` can leave
out a check, and version 1 then gave no penalty for it. A run of
`check --only deps,visibility` on the `ws-ok` fixture got 85.0 (A) from two
of the six inputs. With version 1, such a run gets 100.0 when those two
checks find nothing.

From version 2, the score and the grade need all six checks:

- A run without one of them has no `score` and no `grade` in the JSON report.
- `health.missing_checks` names the checks that the run did not include.
- The penalties and the metrics of the checks that ran stay in the report.
- The human output says `Health (rust): no score. The run did not include
  <checks>.`
- A run without a check that has a penalty, for example `--only parse`, has no
  `health` object.
- A check that ran with zero inputs gives a penalty of 0, and the run still
  gets a score. An example is `deps` on a workspace without dependencies.

## Repeat the measurement

```bash
for ws in fallow ripgrep typos git-cliff taplo mdbook gitui; do
  (cd "$CORPUS/$ws" && cargo fallow check --format json --quiet > "$OUT/$ws.json")
done
jq '.health.metrics | {size_functions, large_functions}' "$OUT"/*.json
```
