# Structure checks

The `boundaries`, `size`, `complexity` and `dupes` commands check the
structure of the workspace. They have these flags:

- `--manifest-path`, `--format human|json|sarif|codeclimate|compact|markdown|github-annotations|github-summary` and `--quiet`,
- [`--changed-since <REF>`](adoption.md#changed-files) and [`--package`](adoption.md#packages),
- the [baseline](adoption.md#baselines) flags,
- `--locked`, `--offline` and `--frozen`, which go to `cargo metadata`.

Each finding has the `error` severity, because it counts for the gate of
the command. See [Rule severity](adoption.md#rule-severity).

Each command also has its own gate. The report always shows the gate, but the
gate fails the run only when you give its flag. `--fail-on-issues` is the
gate flag of each command. The older gate flag of a command, for example
`--fail-on-violations`, does the same thing, with one exception for `size`.

Without `--base` or `--staged`, the growth gate of `size` does not run. Thus
`size --fail-on-issues` alone exits 0. `size --fail-on-growth` alone exits 2,
because that flag asks for the growth gate.

| Command | Reports | Gate flag | Gate |
| --- | --- | --- | --- |
| `cargo fallow boundaries` | Violations of the crate, source and module rules | `--fail-on-issues` or `--fail-on-violations` | `boundary-violations` |
| `cargo fallow size` | Files and functions over the line limits, and growth of a large file | `--fail-on-issues` or `--fail-on-growth`. The gate needs `--base <REF>` or `--staged`, also in `cargo fallow check` | `file-growth` |
| `cargo fallow complexity` | Functions over the cyclomatic or the cognitive limit. With `--coverage <LCOV>`, also functions at or over the CRAP limit | `--fail-on-issues` or `--fail-on-complexity` | `health-findings` |
| `cargo fallow dupes` | Clone groups on normalized token streams | `--fail-on-issues` or `--fail-on-clones`, and `--threshold <PERCENT>` | `duplication-findings`, `duplication-threshold` |

You configure the checks in the root `Cargo.toml`, with one table for each
check. A table has the name of its command. cargo-fallow reads the tables
from the output of `cargo metadata`, because Cargo already parses the
manifest. An unknown key gives an error with the code
`CARGO_FALLOW_INVALID_CONFIG`.

```toml
[workspace.metadata.fallow.boundaries.groups]
protocol = ["app-cli", "app-lsp"]

# A package of `from` must not depend on a package of `deny`. `*` is each
# workspace package. `kinds` limits the Cargo.toml sections.
[[workspace.metadata.fallow.boundaries.crates]]
rule = "core-must-not-depend-on-protocol"
from = ["app-core"]
deny = ["@protocol"]

# A file below `paths` must not contain a pattern of `deny`. A pattern matches
# the same tokens, so a comment or a string does not match. Use
# `match = "text"` to match the plain text.
[[workspace.metadata.fallow.boundaries.sources]]
rule = "process-tree-owned-by-process-crate"
paths = ["crates/cli/src"]
deny = ["process_group(0)", "libc::waitid"]
owner = "app-process"

# A module of `from` must not use a module of `deny`. The rule reads `use`
# items and paths that start with `crate`, `self` or `super`.
[[workspace.metadata.fallow.boundaries.modules]]
rule = "render-does-not-run"
package = "app-cli"
from = ["crate::render"]
deny = ["crate::engine"]

[workspace.metadata.fallow.size]
max-file-lines = 2000     # a file over this must not grow
max-function-lines = 60

[workspace.metadata.fallow.complexity]
max-cyclomatic = 20
max-cognitive = 15
cyclomatic-policy = "standard"  # or "flat-match"
max-crap = 30.0           # only with --coverage; 0 turns the CRAP check off

[workspace.metadata.fallow.dupes]
mode = "mild"             # strict, mild, weak or semantic
min-tokens = 50
min-lines = 5
threshold = 0.0           # 0 means no limit
production = false        # true leaves out test code
```

With `production = true`, or with `cargo fallow dupes --production`, the
clone search leaves out test code. Test code is a test target, a file below
a test module declaration, and an item with a test attribute, for example
`#[cfg(test)] mod tests` or `#[test]`. Bench and example targets stay in the
search. The line counts
and the duplication percentage then count only production code, so the
`threshold` judges production code. `--no-production` keeps test code also
when the table sets `production = true`. A clone never spans a test item that
the search leaves out. A file that production code and test code both reach
counts as production code. The summary has `production: true`. This is the
`production` option of fallow for JavaScript for the duplication check.
`cargo fallow check` reads the same table key, and the key changes only the
duplication part of `check`. The test attributes are `#[cfg(test)]` and the
test macros `test`, `bench`, `rstest`, `test_case` and `quickcheck`, also
with a path, for example `#[tokio::test]`. Code in a macro call, for example
a test in `proptest! { ... }`, stays production code, because the parser keeps
a macro call as tokens.

When the table sets `threshold`, the report shows the
`duplication-threshold` gate. As with the other gates, this gate fails the
run only when you give a flag. The flag is `--threshold` for `dupes`, or
`--fail-on-issues` for `check`. The gate does not follow the level of the
`code-duplication` rule. To gate the percentage only, set
`code-duplication = "off"`: the report then has no clone group, and the gate
and the health penalty still use the percentage of the whole workspace. The
compact and annotation formats show findings only, so read the gate in the
human, markdown or JSON output.

`cargo fallow dupes --trace <FINGERPRINT>` shows the instances of one clone
group. See [Trace](trace.md).

The growth rule of `size` is the rule of `scripts/check-file-size.mjs` in
fallow, applied to `.rs` files. A file over the limit must not grow, and a new
file must not be over the limit. The rule does not check test files, files in
test and snapshot folders, or files with a generator header or an
`@generated` comment. `--base <REF>` compares HEAD with the merge base.
`--staged` compares the index with HEAD.

`complexity` and `dupes` use the same models as fallow for JavaScript. A
unit is a `fn` with a body, or a closure. To suppress a complexity finding,
put `// fallow-ignore-next-line complexity` above the function. To suppress a
clone, put `// fallow-ignore-next-line code-duplication` above the clone. See
[Suppressions](adoption.md#suppressions).

`cyclomatic-policy` sets how the `max-cyclomatic` limit counts. The default,
`standard`, adds 1 for each `match` arm whose pattern is not `_`. With
`flat-match`, a flat `match` adds at most 1. A `match` is flat when no arm
has a guard and no arm has a decision, for example a lookup table. A nested
`match` in an arm is a decision, so the outer `match` is not flat. A closure
in an arm has its own count, so it does not change this. When the policy
changes the count of a finding, `cyclomatic` holds the policy count and
`standard_cyclomatic` holds the standard count. The human line and the
message show `(standard N)`. The severity band reads the policy count. The
health score of `check` and the hotspots keep the standard count. fallow for
JavaScript has the same key, `health.cyclomaticPolicy`, with other values.

The complexity counts have these known differences from other tools:

- Code in a macro call adds nothing. The parser keeps the arguments of a macro
  call as tokens, so `complexity` does not count a condition in
  `assert!(a && b)` or in `vec![...]`.
- The `?` operator adds nothing, although it is an early return.
- `loop` adds 1 to the cyclomatic count and 1 plus the nesting level to the
  cognitive count, the same as `while` and `for`.
- In the cognitive count, a sequence of logical operators continues into a
  group in parentheses. A sequence of the same operator adds 1, and each
  change of the operator adds 1 more. Thus `a && (b && c)` adds 1, and `a && (b || c)` adds 2. A
  sequence ends at an operand with no logical operator in it, so
  `(a && b) || (c && d)` adds 3. fallow for JavaScript uses the same model.

## CRAP score

`complexity --coverage <LCOV>` and `check --coverage <LCOV>` read an LCOV
file and give each function a CRAP score (change risk anti-pattern). The
score is high for a function with high complexity and low test coverage. The
formula is the same as in fallow for JavaScript:

```text
CRAP = CC^2 * (1 - cov)^3 + CC
```

`CC` is the cyclomatic complexity that the limit reads, so it follows
`cyclomatic-policy`. `cov` is the share of the `DA` lines in
the span of the function that have a count above 0. The lines inside a
closure or an inner `fn` belong to that unit, not to the parent. Make the
LCOV file with `cargo llvm-cov`:

```bash
cargo llvm-cov --lcov --output-path lcov.info
cargo fallow complexity --coverage lcov.info
```

- A function with a score at or over `max-crap` (default 30) is a finding
  with the rule `complexity-crap`. The value 0 turns the CRAP check off.
- The finding has the fields `crap`, `coverage_pct` and
  `coverage_source: "lcov"`. `exceeded` is `crap`, `cyclomatic_crap`,
  `cognitive_crap` or `all` when the CRAP limit is part of the finding.
- A function with no `DA` line gets `crap: null` and no CRAP finding. The
  summary counts it in `functions_without_coverage`.
- A function with a `cfg` attribute in its body, for example on a block or a
  statement, also gets `crap: null`. The build can leave the code out, and
  then the LCOV file has no line for a branch that the complexity counts.
- Test units get no score: `#[test]` functions, the code in a `#[cfg(test)]`
  module and the files of a test target, for example `tests/`.
- An `SF` path must be a source file of the workspace. When the LCOV file
  comes from another machine, give its workspace root with
  `--coverage-root <PATH>`. When no `SF` path matches, the run exits 2.
- A file that is gone, or a `DA` line after the end of a file, gives a
  warning. The summary lists these files in `stale_files`. The lines of a
  stale file do not match the code, so its functions get no score.
- The gate stays `health-findings`. Without `--coverage`, the report has no
  CRAP field and no CRAP finding.
- To suppress a CRAP finding, put
  `// fallow-ignore-next-line complexity-crap` above the function. A run
  without `--coverage` does not judge such a comment as stale.
- `complexity-crap` is its own key in `[workspace.metadata.fallow.rules]`.
  The `complexity` key does not change a finding that only the CRAP limit
  gives.

The score uses line coverage only. It does not use branch coverage or an
estimate of the coverage.

The summary of `size`, `complexity` and `dupes` has a `skipped_files` list.
It gives the `path` and a `reason` for each skipped file. These limits put a
bound on the time and the memory of a run:

- The parser rejects a file in these cases. The reason is
  `the file nests too deep for the parser`.
  - The file nests too deep.
  - The file has a postfix chain of more than 2048 steps, for example
    `a.x.x` or `a???`.
  - The syntax tree of the file is deeper than 3072 levels, for example for a
    `1 + 1 + ...` chain. A debug build on Windows has a limit of 2600 levels.
- `dupes` skips a file with more than 250000 tokens.
- A data table can give a long run of a repeated pattern of up to 4 tokens.
  In such a run, `dupes` compares only the first 256 tokens.
- A clone group has at most 128 instances.
- `dupes` stops with exit code 2 and the code `CARGO_FALLOW_TIMED_OUT` after
  300 s.

For the growth rule, `size` runs git with a clean environment. The command
removes `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE` and the other variables
that point git to another repository. `--base` must name a commit, and a value
that starts with `-` is an error. A changed path that is not UTF-8 text goes
in `skipped_files`.
