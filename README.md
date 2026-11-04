# cargo-fallow

`cargo fallow` is the Rust part of Fallow. It is a Cargo subcommand that
analyzes a Cargo workspace. It uses `cargo metadata` and syntactic analysis
with `ra_ap_syntax`, the parser of rust-analyzer. It runs on stable Rust and needs no nightly toolchain.

Do not run `cargo install fallow` for Rust. That crate installs fallow for
TypeScript and JavaScript. Install `cargo-fallow`, then run `cargo fallow`.

## Status

Pre-release. The `check` command runs the static checks in one run and writes
one report with a health score. The `mutants` command runs mutation tests on
one package. Each check also has its own command. The JSON shapes can change
before the first release.

## Install

Install from source only. The crate is not on crates.io.

```bash
git clone https://github.com/fallow-rs/cargo-fallow.git
cd cargo-fallow
cargo install --path . --locked
```

`--locked` builds with the versions in `Cargo.lock`, the versions that CI
tests.

`cargo-fallow` installs one binary, `cargo-fallow`. It never installs a binary
named `fallow`. That name belongs to the JavaScript CLI.

## Usage

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
| `--format human\|json\|sarif\|codeclimate` | The output format. The default is `human`. |
| `--quiet` | Hide the progress lines on stderr. |
| `--only <CHECK>,...` | Run only these checks. |
| `--skip <CHECK>,...` | Do not run these checks. |
| `--fail-on-issues` | Enforce the gate of each check that ran. |
| `--fail-on-parse-error` | Enforce only the `parse-error` gate. |
| `--changed-since <REF>` | Report only the findings in the files that changed since the fork point of `REF`. See [Changed files](#changed-files). |
| `--locked`, `--offline`, `--frozen` | Give the flag to the `cargo metadata` call of `deps`. |

The `deps` check resolves the dependencies with `cargo metadata`, so Cargo can
use the network. `Cargo.lock` stays as it was before the run. Use `--offline`,
or `--skip deps` for a run that does not resolve the dependencies.

The configuration of each check is the same as for its command, in
`[workspace.metadata.fallow.<check>]`. A `check` run has no flag that changes
a limit: change the table instead. The `size` check has no old version to
compare with in a `check` run, so its `file-growth` gate is `skipped`. Use
`cargo fallow size --base <REF>` for the growth rule.

### Changed files

`--changed-since <REF>` scopes a run to the files that a branch changed. The
flag is on `check`, `deps`, `visibility`, `boundaries`, `size`, `complexity`
and `dupes`. Use it for a pull request gate that judges only the changed code.

```bash
cargo fallow check --changed-since origin/main --fail-on-issues
```

- The analysis still reads the whole workspace. Only the report changes.
- The base is the fork point of `REF` and `HEAD`, the same rule as
  `mutants --in-diff`. A committed, a staged, an unstaged and an untracked
  change count.
- The report keeps a finding when its file changed.
- A finding in a `Cargo.toml` (for example a `deps` finding) stays when a file
  of its package changed: the manifest, or a file in the package folder that
  no nested member owns.
- A clone group of `dupes` stays when one of its copies is in a changed file.
- The gates count only the kept findings. The `duplication-threshold` gate
  measures the whole workspace, so a scoped run does not give it.
- The `parse-error` gate always judges each target root file. A file that does
  not parse makes each check incomplete.
- The JSON report has a `changed_since` member with `ref`, `base` (the fork
  point) and `changed_files`. The human output has a `Scope:` line: first for
  a single command, and under the header for `check`.
- The `dupes` summary counts only the kept groups and their families. Its
  line, token and file counts stay those of the whole workspace.
- With `visibility --precise --fail-on-issues`, a run that leaves a candidate
  undecided exits 2 also when the candidate is outside the scope.
- A `check` run with the flag has no health score: its `health` object has
  `scope: "changed"` and no penalties.
- An unknown ref, a ref that starts with `-`, a shallow clone without the fork
  point, or a run outside a git work tree exits 2. The run never falls back to
  the whole workspace.

### Health score

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
the `unit_size` weight is 0.2, not 0.5. [`docs/health-score.md`](docs/health-score.md)
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

## Mutation testing

`cargo fallow mutants` makes small changes (mutants) in the source of one
package, runs the tests against each mutant, and reports the mutants that no test
detects. It needs `cargo-nextest`. It never changes the checkout: it works in a
copy. A survivor fails the run only with `--fail-on-survivors`.

```bash
cargo fallow mutants --package my-crate
cargo fallow mutants --in-diff main --fail-on-survivors --format sarif --quiet
cargo fallow mutants --tier schema --format json --quiet
cargo fallow mutants --max-mutants 200 --time-budget 3600
cargo fallow mutants --inspect 04658712a47ab204
cargo fallow mutants --package my-crate --list --format json
```

| Flag | Meaning |
| --- | --- |
| `--manifest-path <PATH>` | The `Cargo.toml` of the workspace or the package. |
| `--package <NAME>` | The package to mutate. A workspace with one package needs no value. |
| `--in-diff <REF>` | Run only the mutants on changed lines. See [Changed code](#changed-code). |
| `--file <PATH>` | Put mutants only in this file or folder, by the path from the workspace root. The flag can repeat. A value that matches no file is an error. |
| `--test-workspace` | Run each mutant that the tests of the package do not detect against the tests of each workspace member. |
| `--format human\|json\|sarif\|codeclimate` | The output format. The default is `human`. `--inspect` has no SARIF or CodeClimate form. |
| `--quiet` | Hide the progress lines on stderr. |
| `--jobs <N>` | The test processes that run at the same time. |
| `--max-mutants <N>` | Run at most this many mutants. The other mutants stay `pending`. |
| `--timeout <SECONDS>` | The time limit of each mutant run. |
| `--memory-limit <MIB>` | Kill a test process above this footprint. The default is 4096. |
| `--fail-on-survivors` | Exit 1 when a mutant survives. `--fail-on-issues` is the same flag. See [Gate](#gate). |
| `--inspect <ID>` | Show one mutant and its tests. See [Inspect](#inspect). |
| `--no-build` | With `--inspect`: read the coverage map only from the cache. Nothing compiles and no test runs. |
| `--list` | List the mutants, and run nothing. See [List](#list). |

`cargo fallow mutants --help` lists the other flags.

### Output

The human form starts with a short summary. The survivors, the flaky mutants and
the runtime errors follow, grouped by file. The output has no colors.

```text
cargo fallow mutants: my-crate 0.1.0
6 mutants, score 50.0%: 2 killed, 2 survived, 2 no coverage
test selection: 1 tests in the map, 0 with no data, 6 test runs, 0 doctest runs

Survived (2)
  src/a.rs
    :8:5 relational-operator `a == 2` -> `!=` [3f6c1d2a9b804e57]
  src/b.rs
    :8:5 relational-operator `a == 2` -> `!=` [a17e40c2d95b3f68]

Gates:
  surviving-mutants: fail (2 files), not enforced

Inspect a mutant: cargo fallow mutants --inspect <id>
```

The JSON report has `schema_version`, `version` (the tool), `kind` (`mutants`),
`language` (`rust`), `package`, `package_version`, `total_issues` (the survivors),
`summary`, `gate_outcomes`, `in_diff` (with the flag only), `mutants`, and the
parts of the run: `baseline`, `schema`, `rebuild`, `gaps`, `selection` and `guard`.
`mutants` has one object for each mutant, in a fixed order: tier, file, line, column.
These members describe the mutant:

| Member | Meaning |
| --- | --- |
| `id` | The stable id: 16 hexadecimal digits. An edit of other code does not change it. |
| `file`, `line`, `column` | The position. The path is relative to the workspace root. The line and the column start at 1, and the column counts bytes. |
| `operator` | The operator name. |
| `original`, `replacement` | The start of the original text, and the replacement. |
| `tier` | `schema` or `rebuild`. |
| `status` | See [Status](#status). |
| `tests_run`, `doctests_run` | The tests that gave a result, over all runs. |
| `duration_ms` | The wall time of the runs. |
| `actions` | The next steps. |

The tool never writes a test and never changes code. So each action is a read-only
command, and `auto_fixable` is always `false`. A `survived`, `no_coverage` or `flaky`
mutant has one `inspect-mutant` action. The other mutants have an empty array.

The SARIF log has one result for each survivor, at its line. The `mutantId` partial
fingerprint holds the id. The log gives no column, because the report counts bytes and
a SARIF column counts UTF-16 units.

### Gate

`gate_outcomes` always has `surviving-mutants`. `observed` is the number of survivors,
and `files` names each file with its count.

| `status` | Meaning |
| --- | --- |
| `fail` | A mutant survived. With `--fail-on-survivors` the exit code is 1. |
| `pass` | No mutant survived, and each mutant has a result. |
| `warn` | No mutant survived, but a mutant is `pending` or has `runtime_error`. |
| `skipped` | `--no-run` or `--list` ran no mutant. |

The gate counts only `survived`. A mutant that no test executes gets `no_coverage`, and
that status does not fail the gate. Read `summary.no_coverage` to see this code.

A `--time-budget` or a `--max-mutants` that leaves mutants `pending` gives `warn` and
exit 0, because you chose that limit. A stop for low memory with `--fail-on-survivors`
and no survivor gives exit 2: the machine chose the limit, so the gate has no verdict.
The journal keeps the results for the next run.

### Changed code

`--in-diff <REF>` runs only the mutants with a changed line:

- The base is the fork point of the ref and `HEAD`. A later commit on the ref does not
  count.
- The diff compares the base with the working tree, so an uncommitted edit counts. An
  untracked file that git does not ignore counts on each line.
- A mutant counts when a line of its text changed. A mutant that replaces a function
  body counts when a line of that body changed.
- A deleted line changes no line of the new text, so it selects no mutant.
- The `in_diff` member has `ref`, `base`, `changed_files` and `mutants_outside`.
- With no mutant on a changed line, the command builds nothing and exits 0.
- A ref that git cannot resolve exits 2, also outside a repository or in a shallow clone
  with no fork point. The command never falls back to another scope.

The score of such a run describes only the changed code.

### Inspect

`--inspect <ID>` shows one mutant and the tests that execute it. It runs no mutant. A
workspace with more packages needs no `--package`, because the id finds the package.

```text
mutant 3f6c1d2a9b804e57
  src/a.rs:8:5
  operator: relational-operator
  tier: schema

Change
  -     a == 2
  +     a != 2

Tests (coverage map read from the cache)
  my-crate::my_crate$tests::check_works

Doctests have no coverage data. A run has them when no other test fails.
```

The command reads the coverage map of the last run. When the source changed or the
cache has no map, it builds the map first: it compiles a copy with coverage and runs
the plain tests under the memory guard. The next run reuses this map. The command needs
the LLVM tools for that build. The JSON form has `kind` `mutants-inspect`, `mutant` and
`coverage`. `coverage.state` is one of these values:

| `state` | Meaning |
| --- | --- |
| `covered` | `tests` lists the tests that execute a line of the mutant. |
| `not_covered` | The map has data on the lines, and no test executes them. |
| `no_line_data` | The map has no data on the lines, so a run has each test. |
| `constant_context` | The compiler can evaluate this code, so a run has each test. |

`coverage.tests_without_data` lists the tests with no usable coverage data. A run has
these tests for each mutant. The report has no status: the command does not read an old
result. An unknown id exits 2. An id changes when the text of its code changes.

`--no-build` reads the map only from the cache, so the command compiles nothing and
runs no test. A tool that must not start a build uses this flag. When the cache has no
map for this source, the command exits 2 with the code `CARGO_FALLOW_NO_CACHED_MAP`.
The `help` field gives the `--inspect` command that builds the map. The key of the map
includes `Cargo.lock`. A workspace without `Cargo.lock` gets a new lockfile in each
run, so `--no-build` cannot find its map. Run `cargo generate-lockfile` first.

### List

`--list` lists the mutants of one package and runs nothing: it compiles nothing and
runs no test. A workspace with more packages needs `--package`. `--in-diff`, `--file`,
`--tier`, `--no-schema` and `--no-selection` select the mutants as in a run.

The report has the shape of a run report, with `kind` `mutants`:

- Each mutant has the status `pending`, and `actions` is empty.
- The gate is `skipped`, so the exit code is 0.
- `schema.candidates` and `rebuild.candidates` count the mutants of each tier.
- `tier` is the planned tier. A run moves a mutant that breaks the schema build to the
  rebuild tier.
- A run can find that no build reads a rebuild mutant, and reports it with
  `no_coverage`.
- `selection.map` is `cached` when the cache has a map for this source, else `none`.
  `cached` means that `--inspect <ID> --no-build` can show the tests of a mutant.
- The map gives only the `selection` part. `selection.mutants_without_line_data` is
  always 0, because the list does not compare a mutant with the map.

To find the map, `--list` and `--inspect --no-build` copy the source tree to a
temporary folder and hash it, as a run does. A large checkout makes this copy slow.
With `--list`, `--no-cache` or `--no-selection` skips the copy, and `selection.map`
is then `none`.
Both modes run `rustc -vV` for the key of the map, and `--in-diff` runs `git`. Neither
compiles the project.

### Tiers

| Tier | Operators | How it runs |
| --- | --- | --- |
| `schema` | Each operator outside a constant context in a run of both tiers, else the type-safe operators | All mutants go into one build. A runtime switch selects one mutant in each test process, so a mutant costs no compile. The removal loop removes a mutant that breaks the build, and that mutant then runs in a rebuild. |
| `rebuild` | A mutant in a constant context, a deleted arm with a `let` guard, and each mutant that the removal loop removed | Each mutant gets its own rebuild of one reused scratch copy. The rebuild builds only the test binaries of the selected tests. |

`relational-operator`, `logical-operator` and `negate-condition` are type-safe by
construction. `arithmetic-operator`, `unary-operator`, `return-value`,
`deleted-match-arm` and `match-guard` are not: `a + b` can be a type with no `Sub`.
In a run of both tiers, such a mutant also goes into the schema. When it breaks the
build, the removal loop finds it through the compiler error and moves it to the
rebuild tier, where it gets the status `compile_error`. `--tier schema` keeps only
the type-safe operators, and `--tier rebuild` and `--no-schema` run the other
operators as rebuilds. A switch has these forms:

- An operator or a match guard: a `match` on the active mutant around the expression.
- A return value: the same `match` as the whole body of the function.
- A deleted arm: a new guard on the arm that is false when the mutant is active, so
  the value goes on to the next arm.

A switch reads a value at run time, and a constant context cannot. So a mutant in a
`const fn` or in a `const` item stays in the rebuild tier.

### Status

| Status | Meaning |
| --- | --- |
| `killed` | A test failed with the mutant, also in a second look with no other test process of the run. `killed_by` names the test. |
| `survived` | Each test passed, also in the retry. A failure next to another test that does not repeat one at a time also gives this status, and `reason` says so. This applies to a failure in the first run and to a failure in the retry. With `-j 1` no other test runs next to the mutant, so such a failure in the retry gives `flaky`. |
| `timeout` | The tests did not end within the limit of the tool, or the nextest config of the project ended a test at its limit. A limit at most 10 times the baseline gets a retry with twice the limit, because host load can explain it. A failure in that retry that does not repeat one at a time keeps the timeout. |
| `flaky` | Two runs of the mutant disagree. |
| `no_coverage` | No test executes the mutant, no build reads it (for example a `cfg` that is off), or the package has no test that passes. `reason` says which. |
| `compile_error` | The mutant does not compile, or its compile passed the limit: 3 times the clean build, with a minimum of 30 seconds. A mutant in a `const fn` can loop in the compiler. |
| `runtime_error` | The run has no test result. `reason` says why, for example a kill by the memory guard. |
| `pending` | The run stopped first: `--time-budget`, `--max-mutants`, `--no-run` or low memory. `--list` gives each mutant this status. |

The score is `killed + timeout` over `killed + timeout + survived`.

A switch can change the result of a test with no active mutant: the match of a switch
ends a temporary earlier, and `column!()` on the line of a site gets another value.
`baseline.regressions` names such a test, and the human report does too. The test does
not run in the schema tier, so each mutant that it executes runs as a rebuild. A
doctest has no line data, so a changed doctest moves each mutant. `--tier schema` also
moves them. `schema.moved_to_rebuild` counts them.

`--no-retry` gives no mutant a second run, also no second look at a kill.

### Safety

A mutant can allocate without limit, and a test timeout does not protect the
machine. The memory guard is on by default:

- It kills the test processes of a mutant when one process passes
  `--memory-limit` (default 4096). The mutant gets `runtime_error` with a
  reason.
- It kills the largest process tree when the free memory drops under
  `--low-memory-percent` (default 25).
- It kills the process trees when it cannot read the memory data.
- A test can leave a child that runs on. After each test run the command kills each
  such process that runs a program from the work folder. A plain tool that a test
  starts can stay, because a plain tool has no mutant.
- A watchdog process kills the process trees when `cargo fallow` dies, and it removes
  the temporary work folder. That covers a signal and a panic.

macOS reads the footprint from `top`, because RSS does not count compressed
pages. Linux reads RSS plus swap from `/proc`, and the free memory from the
cgroup limit or `/proc/meminfo`. Other systems have no probe, so the command
stops unless you pass `--no-memory-guard`.

`--jobs` defaults to a quarter of the cores, with a maximum of 2. Each mutant run
has a time limit of 5 times the baseline of the tests that it runs, with a minimum
of 20 seconds. `--timeout` sets another value.

### Test selection

A mutant runs only the tests that execute its lines, and the fastest test runs
first. A mutant that no test executes gets `no_coverage` and does not run. A
rebuild mutant then also gets no build, so a mutant that does not compile can get
`no_coverage` and not `compile_error`.

The selection comes from a coverage map:

1. One build of the unmodified copy has `-C instrument-coverage`. The build needs
   `llvm-profdata` and `llvm-cov` from the active toolchain. The command stops with
   an error when they are missing. Run `rustup component add llvm-tools`.
2. Each test that passes in the baseline runs alone through nextest, with its own
   profile folder.
3. The run converts the profiles of the test to line ranges at once and deletes
   them.

The rule is on the safe side. A test also runs for each mutant when it wrote no
usable data or when its name is not plain text. Each test runs for a mutant on a line that has no coverage data in any test,
and for a mutant in a constant context such as a `const fn`, because the compiler
can evaluate that code and no test process then executes it.
The `selection` part of the report counts these tests and these mutants.

A test that passes in the baseline and fails alone needs another test, so its result
says nothing about a mutant in a run of selected tests. Such a test decides nothing.
`selection.tests_fail_alone` and the human report name it. `--file` and `--in-diff`
keep the line data of their files only, and such a map stays out of the cache. They
still run each test once for the map, because no other data says which test executes
a line.

The coverage flag reaches only the crates of the selected package. A test binary is
then smaller and faster, and the map holds no other crate.

`--no-selection` runs each test for each mutant, with no coverage build. Use it to
check a result. The selection changes a status in only two ways:

- `survived` or `compile_error` becomes `no_coverage`.
- `killed` and `timeout` can swap, when one test fails and another test never ends
  with the same mutant. The run stops at the first result, and the selection changes
  the order of the tests. Both count as detected, so the score does not change.

Another difference is a bug.

The score leaves out `no_coverage`, so the first change can raise the score: a
mutant that no test reaches no longer counts as a survivor. Read the score together
with the `no_coverage` count.

#### Doctests

Doctests have no coverage data on stable Rust, because each doctest is a separate
binary that the map cannot read. So the selection does not apply to them:

- A mutant that the selected tests do not kill still runs all doctests, as without
  selection. A doctest can kill it.
- A mutant that no nextest test executes also runs all doctests. A failed doctest
  gives `killed`. When the doctests pass, the mutant gets `no_coverage` with a
  reason that says so, because the map cannot show that a doctest reached it.
  Without selection the same mutant gets `survived`.
- `--no-doctests` leaves them out, and the `gaps` part of the report says so.

#### Cache and journal

The run keeps two files in `target/fallow-mutation/<package>/` of the workspace, so
`cargo clean` removes them. `--no-cache` turns both off.

- The coverage map. Its key is a hash of each file of the workspace copy, the
  compiler version, `RUSTFLAGS` and the tool version. Each change of a source or a
  test gives a new map.
- The journal: one line for each mutant with a result. The key also has the options
  that change a verdict. A run that stops (`--time-budget`, `--max-mutants`, low
  memory, a crash) leaves the journal, and the next run with the same key reports
  those results and runs only the other mutants. `--max-mutants` then counts only
  the new runs. A `runtime_error` runs again, because it can come from the state of
  the machine. The journal goes when each mutant has a result.

### Limits

The `gaps` part of the report counts the code that a run does not cover:

- Only the default features compile. A mutant under another feature set gets
  `no_coverage`.
- Macro bodies and macro arguments have no mutants. The report counts their
  operator tokens.
- A file behind a macro-declared module has no mutants. The report counts it.
- Proc-macro targets have no mutants.
- `.git` is not in the copy, so a test that reads it fails in the baseline and
  decides nothing.
- A test can lose coverage data and still pass: a child process that aborts writes
  no profile. A mutant that only such a child executes gets `no_coverage`. A child
  with an empty environment writes a profile under the default name in the working
  folder of the test. The run sees that file, and the test then runs for each mutant.
- The coverage counters are not atomic. A race in a test with threads can in theory
  hide an executed line. The tool does not turn on atomic counters.
- Without `--test-workspace` only the tests of the package run. The tests of a member
  that depends on the package can see a mutant, so a mutant can survive here and die
  with the flag. `gaps.untested_dependents` and the human report name those members.
  With the flag, each `survived` and `no_coverage` mutant then runs each test of the
  workspace, with no selection and no second run of a pass. A stop in this stage
  repeats this stage in the next run. `workspace_tests` has the counts.
- A `.cargo/config.toml` above the workspace root reaches the copy, but a relative
  path in such a file then starts at the work folder.
- A test that closes each inherited file descriptor, or a system with no `/dev/fd`,
  turns the schema switch off. Then a mutant can survive. `--no-schema` checks that.

## Unused dependencies

`cargo fallow deps` reports the dependencies in `Cargo.toml` that no code of
the package uses. It reads the first segment of each path, `use`, `extern
crate`, macro path and attribute path in each target, also in macro tokens,
attributes, `cfg_attr` and doc tests.

```bash
cargo fallow deps
cargo fallow deps --fail-on-issues --format sarif --quiet
```

| Flag | Meaning |
| --- | --- |
| `--manifest-path <PATH>` | The `Cargo.toml` of the workspace. |
| `--format human\|json\|sarif\|codeclimate` | The output format. The default is `human`. |
| `--quiet` | Hide the progress lines on stderr. |
| `--fail-on-issues` | Exit 1 when a finding has the `error` severity. |
| `--changed-since <REF>` | Report only the findings in the files that changed since the fork point of `REF`. See [Changed files](#changed-files). |
| `--locked` | Give `--locked` to `cargo metadata`. |
| `--offline` | Give `--offline` to `cargo metadata`. |
| `--frozen` | Give `--frozen` to `cargo metadata`. |

The command resolves the dependencies: it runs `cargo metadata` with all
features and without `--no-deps`, so it gets the library name and the `links`
key of each dependency. Cargo can use the network for this. Use `--offline` or
`--frozen` to prevent it. The command keeps `Cargo.lock` as it was before the
run: it removes a lock file that Cargo created and writes back the old bytes
of a lock file that Cargo changed. All features are on, so a feature that
needs a dependency that Cargo cannot get fails the run with exit code 2.

| Rule | Severity | Meaning |
| --- | --- | --- |
| `unused-dependency` | error | No code of the package uses the `[dependencies]` entry. |
| `unused-dev-dependency` | error | No test, bench, example, doc test or `cfg(test)` code uses the entry. |
| `unused-build-dependency` | error | The build script does not use the entry. |
| `misplaced-dependency` | error | Only tests, benches, examples or doc tests use the `[dependencies]` entry. `used_by` names them. |
| `unused-workspace-dependency` | error | No member inherits the `[workspace.dependencies]` entry, or each member entry that inherits it is unused. |
| `unused-optional-dependency` | warn | No code uses the optional dependency. A feature enables it. |
| `feature-only-dependency` | warn | No code uses the dependency, and a feature sets its features (`dep/feature`). |
| `redundant-ignore` | warn | An ignore entry hides no finding. |

Each finding has `package`, `manifest_path`, `line`, `section`, `platform`,
`dependency` (the key in `Cargo.toml`), `package_name` (for a rename) and an
`actions` array. The tool does not edit `Cargo.toml`, so `auto_fixable` is
always `false`. Only `error` findings count for the `error-severity-findings`
gate, and only `--fail-on-issues` enforces it.

The check stays silent where the code can use a dependency without a name:

- A dependency with a `links` key gets no error, because it can link a native
  library. An optional one gets the warning.
- `include!("path")` with one string literal adds the code of that file, and
  the check reads it like the other code. Another `include!`, for example
  `include!(concat!(env!("OUT_DIR"), "/gen.rs"))`, can add code that the check
  cannot read. When the package has a build script, that code can use each
  `name::` in the text of the build script and the runtime crate of an
  `X-build` or `X-codegen` build dependency. Without a build script, it can
  use each dependency. A literal path that the check cannot read or parse
  gets the same credit.
- A package with a file that does not parse is not judged. The report lists
  it in `skipped`.
- A dev-dependency that repeats a normal dependency is not reported. It often
  turns on more features for the tests.
- A package with `trybuild`, `macrotest`, `ui_test` or `compiletest_rs` in
  `[dev-dependencies]` gets no dev-dependency findings. These harnesses build
  test files that no target declares.
- When code uses a macro crate `X_derive`, `X_derives`, `X_macro`,
  `X_macros`, `X_impl`, `X_internal` or `X_attributes`, the crate `X` is in
  use too, because the macro output names it (`serde_derive` and `serde`).
  Two pairs with other names are known: `num-derive` gives credit to
  `num-traits`, and `serde_repr` gives credit to `serde`. For another pair,
  add the runtime crate to the `ignored` list.
- Every `cfg` branch counts, so a `[target.'cfg(..)'.dependencies]` entry that
  `#[cfg(..)]` code uses is in use.

A crate that is in `Cargo.toml` only to turn on a feature of another
dependency looks unused. Add its name to an ignore list:

```toml
[package.metadata.fallow.deps]
ignored = ["chrono"] # unifies the features with a workspace build

[workspace.metadata.fallow.deps]
ignored = ["getrandom"]
```

The command also reads `ignored` in `[package.metadata.cargo-shear]` and
`[workspace.metadata.cargo-shear]`, so a workspace can move from cargo-shear
without a change. Only the cargo-fallow lists get `redundant-ignore` findings.

## Unused files

`cargo fallow files` reports each `.rs` file in a target folder of a package
that no target compiles. The compiler never reads such a file, so its code is
dead and no other check sees it.

```bash
cargo fallow files
cargo fallow files --format json --fail-on-issues
```

A file counts as used when one of these reaches it from a target root:

- a `mod name;` item, also in an inline module and in the tokens of a macro
  call such as `cfg_if!`,
- a `#[path = "..."]` attribute or a `path` value in a `cfg_attr` attribute,
- a literal `include!`, `include_str!` or `include_bytes!` call,
- a Cargo target: a library, a binary, a test, a benchmark, an example or a
  build script.

The check gives credit when the syntax cannot decide:

- It reads only `src/`, `tests/`, `benches/`, `examples/` and the folder of
  each target root that is not the package folder.
- It skips a folder with no reached `.rs` file, for example the inputs of a
  compile test. So a folder of old modules with no `mod` item is not reported:
  only the file that names the folder is.
- It skips each folder with one of these names: `fixture`, `fixtures`,
  `testdata`, `test-data`, `test_data`, `data`, `snapshots`, `ui`,
  `compile-fail`, `compile_fail`, `corpus`, `samples`, `inputs` and
  `resources`.
- It skips a folder with its own `Cargo.toml`, a hidden folder, `target`, and
  a file with a generator header or an `@generated` comment.
- A `.rs` string literal in a macro call counts as an include, because a
  macro can give it to `include_str!`.
- It skips each file of a package when the syntax cannot follow the code: a
  computed `include!` path (an `OUT_DIR` path is not a source file), a file
  that `include!` reads and that declares a module, a `path` attribute on an
  inline module, a module in a function body, a module form in a macro that
  the scan cannot read, or a file that does not parse. A macro that declares a
  module from its input skips each package of the workspace.
  `summary.skipped_packages` names each such package with the reason.

The check cannot see a module that a macro of another crate, for example a
dependency, declares. Add such a file to `exclude`.

Each finding has the rule `unused-file`, the `warning` severity and a
`delete-file` action with `auto_fixable: false`. `--fail-on-issues` enforces
the `unused-files` gate. The health score does not change.

```toml
[workspace.metadata.fallow.files]
exclude = ["crates/app/src/scratch.rs"]
```

`exclude` lists files and folders that the check skips.

## Visibility

`cargo fallow visibility` reports items whose visibility is larger than their
uses need. The model is closed-world: the workspace is the only user of its
libraries, unless you exclude a library with `--exclude-crate`.

| Kind | Meaning | Change |
| --- | --- | --- |
| `dead_public` | A `pub` item that no code reaches. | `pub` to `pub(crate)`. Then the compiler shows the dead code. |
| `unnecessary_public` | A `pub` item that only its own crate uses. | `pub` to `pub(crate)`. |
| `unnecessary_restricted_visibility` | A `pub(crate)`, `pub(super)` or `pub(in path)` item that only its own module and its child modules use. | Remove the keyword. |

```bash
cargo fallow visibility
cargo fallow visibility --format json --quiet --kind dead-public
cargo fallow visibility --production my-cli:my-cli --exclude-crate my_api --fail-on-issues
cargo fallow visibility --precise
```

| Flag | Meaning |
| --- | --- |
| `--production <PACKAGE:BIN>` | A production binary. The flag can repeat. Without it, each binary is a production binary. Tests, benches, examples and doctests are other roots: an item that only they use has `test_only: true`. |
| `--exclude-crate <CRATE>` | A library whose public items are an external API. The flag can repeat. |
| `--uniform-fields` | The fields of one struct with one written visibility change as a group. |
| `--kind <KIND>` | Report only this kind: `dead-public`, `unnecessary-public` or `unnecessary-restricted-visibility`. The flag can repeat. |
| `--fail-on-issues` | Exit 1 when the run has a finding. |
| `--changed-since <REF>` | Report only the findings in the files that changed since the fork point of `REF`. See [Changed files](#changed-files). |
| `--precise` | Decide the undecided candidates with rust-analyzer. See below. |

The table `[workspace.metadata.fallow.visibility]` in the root `Cargo.toml`
can set the same options. A flag wins over the table: a list flag replaces the
list of the table, and `--uniform-fields` turns the option on. An unknown key
stops the run with exit code 2 and the code `CARGO_FALLOW_INVALID_CONFIG`.

```toml
[workspace.metadata.fallow.visibility]
production = ["my-cli:my-cli"]
exclude-crates = ["my_api"]
uniform-fields = true
kinds = ["dead-public", "unnecessary-public"]
```

The check uses syntax and the module graph of the workspace. When the syntax
cannot decide, the check gives credit and does not report. Credit goes to:

- a method call or a field access: each item with that name, because the type
  of the receiver is unknown,
- a name in macro tokens, in an attribute or in a documentation code block,
- items of trait impls, `#[macro_export]` macros and proc-macro entry points,
- items under an attribute macro, for example the fields of a
  `#[napi(object)]` struct,
- items under `allow(dead_code)` or `expect(dead_code)`, also when the
  attribute is on an enclosing item, module or file,
- items behind a glob re-export.

The check does not evaluate `cfg`: each branch counts as code.

A generator writes the visibility of a generated file, so the check reports
no finding in such a file. A file is generated when a comment in its first ten
lines holds `@generated`, `AUTOGENERATED FILE. DO NOT EDIT.`, or
`Generated by <x>. Do not edit.` The items of the file still count as uses
of other items. A file in `OUT_DIR` is not in the source tree, so the check
never reports it.

The graph cannot read the file of an `include!` with a path that is not a
literal, for example `include!(concat!(env!("OUT_DIR"), "/gen.rs"))`. That
code can name the items of its own crate and the `pub` items of its
dependencies. Those candidates go to `undecided`, not to `findings`. When the
package has a build script, only the dependencies that the build script names
as `name::` count, as in `cargo fallow deps`.

### Output

The JSON report has `schema_version`, `version`, `kind` (`visibility`),
`language`, `mode` (`fast` or `precise`), `total_issues`, `summary`,
`gate_outcomes`, `precise`, `graph`, `crates`, `findings` and `undecided`.

Each finding has an `actions` array with one action of type
`change-visibility`. It gives the written keyword (`from`), the new keyword
(`to`, an empty text for private) and the `range` of the keyword. An enum
variant has no keyword, so its action has the type `remove-item`.
`auto_fixable` is false: a change can need a change in other code, for
example in a `pub use`.

`undecided` lists the candidates that only an untyped credit keeps. They are
not findings. `--format sarif` gives one result for each finding, with the
level `error` when `--fail-on-issues` is on. The gate is
`visibility-findings`.

### Precise mode

`--precise` decides only the undecided candidates: method calls on a type that
the syntax cannot know, and names in macro output. A worker process loads the
workspace into rust-analyzer, runs the build scripts and the proc-macros, and
finds the uses of each candidate with name resolution and type inference.

Precise mode needs a build with the `precise` feature. The feature adds about
150 crates to the build.

```bash
cargo install --path . --locked --features precise
```

The proc-macro server comes from the sysroot of the active toolchain, and the
standard library source comes from the `rust-src` component. The default
rustup profile does not install `rust-src`:

```bash
rustup component add rust-src
```

Without the standard library source, or when a proc-macro crate does not
load, rust-analyzer misses uses. Then the run stops with exit code 2 and the
code `CARGO_FALLOW_PRECISE_FAILED`. The worker runs `cargo check` in the
`rust-analyzer` folder of the target folder, so it does not wait for the build
lock of your own builds.

| Flag | Default | Meaning |
| --- | --- | --- |
| `--precise-timeout <SECONDS>` | 1200 | The time limit of the worker. |
| `--precise-memory-limit <MIB>` | 8192 | The memory guard stops the worker above this footprint. |
| `--precise-file-limit <KIB>` | 1024 | The largest file that the search reads with name resolution. A candidate whose name is in a larger file keeps its visibility, because the search cannot see what the name means there. |

rust-analyzer loads the workspace with all features and with tests. It does not
see code under a `cfg` that is off in that build, for example a condition for
another target. A candidate with a use under such a `cfg` stays undecided.
rust-analyzer does not search the target folder either, so a candidate that
code from `OUT_DIR` can name stays undecided.

When a limit or a failure stops the worker, the decisions that it made stay,
and the other candidates stay undecided. `precise.status` is then `failed`,
`time_limit` or `memory_limit`, and a warning goes to stderr. The SARIF log
has `executionSuccessful: false` and a notification with the id
`visibility-precise`. With `--fail-on-issues`, the run stops with exit code 2
and the code `CARGO_FALLOW_PRECISE_FAILED`, because a gate must not pass on a
part of the decisions.

`precise.elapsed_ms`, `precise.load_ms` and `precise.search_ms` are run times,
so they change from run to run. The other members of the JSON report are the
same in each run on the same input.

## Output formats

This section describes `check`. [Output](#output) describes `mutants`.

- `human`: the score, then the text of each check, then one gate block, on
  stdout. Progress lines go to stderr.
- `json`: the report on stdout. The report has `schema_version` (2),
  `version`, `kind`, `language`, `workspace`, `checks`, `total_issues`,
  `health`, `findings`, `gate_outcomes` and `sections`. `health` has no
  `score` and no `grade` when `health.missing_checks` names a check. `sections` holds the
  JSON report of each check that ran, keyed by check name. Each section is the
  same as the output of the command of the same name. `findings` holds the
  findings of each check in one shape: `id`, `check`, `rule`, `severity`,
  `message`, `location`, `related_locations` and `actions`.
- `sarif`: a SARIF 2.1.0 log on stdout, with one run. The results, the rules
  and the gate notifications are those of the SARIF log of each check.
- `codeclimate`: a JSON array with one CodeClimate issue for each finding, for
  the code quality report of GitLab. Each command has this format.

A finding `id` and a CodeClimate `fingerprint` are the same value: 16 hex
digits of an FNV-1a hash of the check, the rule and the identity of the
finding. The identity uses names where a name is unique, for example the
dependency key, or the crate and the item path, so an edit above a finding does
not change its id. The hash is the same as in the JavaScript CLI.

The gates of `check` are the gates of the checks that ran: `parse-error`,
`error-severity-findings` (deps), `unused-files` (files),
`visibility-findings`, `boundary-violations`, `file-growth` (size),
`health-findings` (complexity), `duplication-findings` and
`duplication-threshold` (dupes). Each gate except `unused-files` has the same
name as a gate of the JavaScript CLI. A gate fails the run when its `status`
is `fail` and `enforced` is `true`.

The tool parses each target root file on a thread with a stack that is sized
from the file length, so deep nesting cannot crash the run. A file that needs
more stack than the tool can reserve counts for the `parse-error` gate with
`reason: "nesting-too-deep"`.

A target root file that cannot be read as UTF-8 text is not a parse error.
The tool writes a warning to stderr, and the `parse-error` gate does not
judge the file. `--quiet` does not hide the warning.

With `--format json`, an error goes to stdout as an error document:
`{ "error": true, "message": "...", "exit_code": 2, "code": "..." }`. In the
other formats, the error goes to stderr.

## Structure checks

Four commands check the structure of the workspace. Each command has the
`--manifest-path`, `--format human|json|sarif|codeclimate`, `--quiet` and
[`--changed-since <REF>`](#changed-files) flags, and its own
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

The `size` growth rule is the rule of `scripts/check-file-size.mjs` in fallow,
for `.rs` files: a file over the limit must not grow, and a new file must not be
over it. Test files, test and snapshot folders, and files with a generator header
are not checked. `--base <REF>` compares HEAD with the merge base, and
`--staged` compares the index with HEAD.

The complexity and clone models are the models of fallow for JavaScript. A unit
is a `fn` with a body or a closure. Put `// fallow-ignore-next-line complexity`
above a function, or `// fallow-ignore-next-line code-duplication` above a
clone, to suppress it.

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

## GitHub Action

The repository is also a GitHub Action. It runs one command, writes the job
summary, shows each finding as an annotation, and can upload the SARIF log to
code scanning. [action/README.md](action/README.md) lists the inputs and the
outputs.

```yaml
- uses: fallow-rs/cargo-fallow@<sha>
  with:
    command: check
```

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | No enforced gate failed. |
| `1` | An enforced gate failed. For `mutants`, a survivor with `--fail-on-issues` or `--fail-on-survivors`. For `visibility`, a finding with `--fail-on-issues`. |
| `2` | The input was not valid, or the run could not complete. |

Each command uses these codes and the same error document.

## License

MIT. See `LICENSE`.
