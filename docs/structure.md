# Structure checks

Four commands check the structure of the workspace. Each command has the
`--manifest-path`, `--format human|json|sarif|codeclimate|compact|markdown|github-annotations|github-summary` and `--quiet` flags,
the [`--changed-since <REF>`](adoption.md#changed-files), [`--package`](adoption.md#packages) and
[baseline](adoption.md#baselines) flags,
and its own
gate. A gate is in the report each time, but it fails the run only with its flag.
Each command accepts `--fail-on-issues` as its gate flag. The older flag of
each command is the same flag. `size` has no growth gate without `--base` or
`--staged`, so `size --fail-on-issues` alone exits 0. `size --fail-on-growth`
alone exits 2, because that flag asks for the growth gate.

| Command | Reports | Gate flag | Gate |
| --- | --- | --- | --- |
| `cargo fallow boundaries` | Violations of the crate, source and module rules | `--fail-on-issues` or `--fail-on-violations` | `boundary-violations` |
| `cargo fallow size` | Files and functions over the line limits, and growth of a large file | `--fail-on-issues` or `--fail-on-growth`. The gate needs `--base <REF>` or `--staged` | `file-growth` |
| `cargo fallow complexity` | Functions over the cyclomatic or the cognitive limit | `--fail-on-issues` or `--fail-on-complexity` | `health-findings` |
| `cargo fallow dupes` | Clone groups on normalized token streams | `--fail-on-issues` or `--fail-on-clones`, and `--threshold <PERCENT>` | `duplication-findings`, `duplication-threshold` |

The configuration is in the root `Cargo.toml`, in one table for each check. The
name of each table is the name of its command. Cargo
already parses the manifest, so the tool reads the tables from `cargo metadata`.
An unknown key is an error with the code `CARGO_FALLOW_INVALID_CONFIG`.

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

A `threshold` in the table puts the `duplication-threshold` gate in the
report. The gate fails the run only with `--threshold`, or with
`--fail-on-issues` for `check`, the same as each other gate.

`cargo fallow dupes --trace <FINGERPRINT>` shows the instances of one clone
group. See [Trace](trace.md).

The `size` growth rule is the rule of `scripts/check-file-size.mjs` in fallow,
for `.rs` files: a file over the limit must not grow, and a new file must not be
over it. Test files, test and snapshot folders, and files with a generator header
are not checked. `--base <REF>` compares HEAD with the merge base, and
`--staged` compares the index with HEAD.

The complexity and clone models are the models of fallow for JavaScript. A unit
is a `fn` with a body or a closure. Put `// fallow-ignore-next-line complexity`
above a function, or `// fallow-ignore-next-line code-duplication` above a
clone, to suppress it. See [Suppressions](adoption.md#suppressions).

The complexity counts have these known differences from other tools:

- Code in a macro call adds nothing. The parser keeps the arguments of a macro
  call as tokens, so a condition in `assert!(a && b)` or in `vec![...]` is not
  visible.
- The `?` operator adds nothing, although it is an early return.
- `loop` adds 1 to the cyclomatic count and 1 plus the nesting level to the
  cognitive count, the same as `while` and `for`.
- The cognitive count of a sequence of logical operators keeps the last
  operator through a group in parentheses. So `a && (b && c)` adds 1, and
  `a && (b || c)` adds 2. A sequence ends at an operand with no logical
  operator in it, so `(a && b) || (c && d)` adds 3. This is the model of
  fallow for JavaScript.

`skipped_files` in the summary of `size`, `complexity` and `dupes` lists each
skipped file with its `path` and a `reason`. These limits keep each run bounded:

- The parse refuses a file that nests too deep, has a postfix chain of more than
  2048 steps (for example `a.x.x` or `a???`), or gives a syntax tree deeper than
  3072 levels (for example a `1 + 1 + ...` chain). The reason is
  `the file nests too deep for the parser`.
- `dupes` skips a file with more than 250000 tokens.
- In a run of a repeated pattern of up to 4 tokens, such as a data table, `dupes`
  compares only the first 256 tokens.
- A clone group keeps at most 128 instances.
- `dupes` stops with exit code 2 and the code `CARGO_FALLOW_TIMED_OUT` after
  300 s.

The growth rule of `size` runs git with a clean environment: it removes
`GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE` and the other variables that move
git to another repository. `--base` must name a commit. A value that starts with
`-` is an error. A changed path that is not UTF-8 text is in `skipped_files`.
