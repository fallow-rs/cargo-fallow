# Structure checks

The `boundaries`, `size`, `complexity` and `dupes` commands check the
structure of the workspace. They have these flags:

- `--manifest-path`, `--format human|json|sarif|codeclimate|compact|markdown|github-annotations|github-summary` and `--quiet`,
- [`--changed-since <REF>`](adoption.md#changed-files) and [`--package`](adoption.md#packages),
- the [baseline](adoption.md#baselines) flags.

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
| `cargo fallow size` | Files and functions over the line limits, and growth of a large file | `--fail-on-issues` or `--fail-on-growth`. The gate needs `--base <REF>` or `--staged` | `file-growth` |
| `cargo fallow complexity` | Functions over the cyclomatic or the cognitive limit | `--fail-on-issues` or `--fail-on-complexity` | `health-findings` |
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

[workspace.metadata.fallow.dupes]
mode = "mild"             # strict, mild, weak or semantic
min-tokens = 50
min-lines = 5
threshold = 0.0           # 0 means no limit
```

When the table sets `threshold`, the report shows the
`duplication-threshold` gate. As with the other gates, this gate fails the
run only when you give a flag. The flag is `--threshold` for `dupes`, or
`--fail-on-issues` for `check`.

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
