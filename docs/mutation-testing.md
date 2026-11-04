# Mutation testing

`cargo fallow mutants` makes small changes (mutants) in the source of one
package, runs the tests against each mutant, and reports the mutants that no test
detects. The command needs `cargo-nextest`. It does not change your checkout,
because it makes each change in a copy. Survivors fail the run only when you pass
`--fail-on-survivors`.

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
| `--package <NAME>` | The package to mutate. In a workspace with one package, you can omit this flag. |
| `--in-diff <REF>` | Run only the mutants on changed lines. See [Changed code](#changed-code). |
| `--file <PATH>` | Mutate only the code in this file or folder. Give the path from the workspace root. You can repeat the flag. A value that matches no file is an error. |
| `--test-workspace` | Run each mutant that the package tests do not detect against the tests of each workspace member. |
| `--format human\|json\|sarif\|codeclimate\|compact\|markdown\|github-annotations\|github-summary` | The output format. The default is `human`. `--inspect` supports only `human` and `json`. |
| `--quiet` | Hide the progress lines on stderr. |
| `--locked`, `--offline`, `--frozen` | Give the flag to each cargo call: the `cargo metadata` call that reads the workspace, and each build, test run and `cargo metadata` call in the copies. A new lock file of a copy gets only `--offline`. With `--offline` or `--frozen`, each such call also sets `CARGO_NET_OFFLINE=true`, so a test that starts cargo does not use the network. |
| `--jobs <N>` | The number of test processes that run at the same time. |
| `--max-mutants <N>` | Run at most this many mutants. The other mutants keep the status `pending`. |
| `--timeout <SECONDS>` | The time limit of each mutant run. |
| `--memory-limit <MIB>` | Kill a test process when its memory footprint is more than this value. The default is 4096. |
| `--fail-on-survivors` | Exit 1 when a mutant survives. `--fail-on-issues` is the same flag. See [Gate](#gate). |
| `--inspect <ID>` | Show one mutant and its tests. See [Inspect](#inspect). |
| `--no-build` | With `--inspect`, read the coverage map only from the cache. The command compiles nothing and runs no test. |
| `--list` | List the mutants, and do not run them. See [List](#list). |

`cargo fallow mutants --help` lists the other flags.

## Output

The `human` format starts with a short summary. After the summary come the
survivors, the flaky mutants and the runtime errors, grouped by file. The output
has no colors.

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

The JSON report has these top-level fields: `schema_version`, `version` (the
cargo-fallow version), `kind` (`mutants`), `language` (`rust`), `package`,
`package_version`, `total_issues` (the number of survivors), `summary`,
`gate_outcomes`, `in_diff` (only with `--in-diff`), `stopped_by` (why the run
stopped early, else `null`), `elapsed_ms` and `mutants`. Each part of the run also
has a field: `baseline`, `schema`, `rebuild`, `gaps`, `selection`,
`workspace_tests` and `guard`.

`mutants` has one object for each mutant, sorted by tier, file, line and column.
These fields describe a mutant:

| Member | Meaning |
| --- | --- |
| `id` | The stable id: 16 hexadecimal digits. An edit of other code does not change it. |
| `file`, `line`, `column` | The position. The path is relative to the workspace root. The line and the column start at 1, and the column counts bytes. |
| `operator` | The operator name. |
| `original`, `replacement` | The start of the original text, and the replacement. |
| `tier` | `schema` or `rebuild`. |
| `status` | See [Status](#status). |
| `tests_run`, `doctests_run` | The number of tests that gave a result, over all runs. |
| `duration_ms` | The wall time of the runs. |
| `actions` | The next steps. |

cargo-fallow does not write tests or change your code. Each action is
therefore a read-only command, and `auto_fixable` is always `false`. A `survived`,
`no_coverage` or `flaky` mutant has one `inspect-mutant` action. For the other
mutants, `actions` is an empty array.

The SARIF log has one result for each survivor, on the line of the survivor. The
`mutantId` partial fingerprint contains the mutant id. The results have no column.
The report counts a column in bytes, but SARIF counts it in UTF-16 code units.

In the shared finding shape, each surviving mutant has the rule
`surviving-mutant` and the `error` severity, because it counts for the
survivor gate.

## Gate

`gate_outcomes` always has `surviving-mutants`. In this gate, `observed` is the
number of survivors, and `files` lists each file with its count.

| `status` | Meaning |
| --- | --- |
| `fail` | A mutant survived. With `--fail-on-survivors`, the exit code is 1. |
| `pass` | No mutant survived, and each mutant has a result. |
| `warn` | No mutant survived, but a mutant is `pending` or has `runtime_error`. |
| `skipped` | The run used `--no-run` or `--list`, so no mutant ran. |

Only a `survived` mutant fails the gate. A mutant that no test executes gets
`no_coverage`, and that status does not fail the gate. `summary.no_coverage` gives
the number of these mutants.

When `--time-budget` or `--max-mutants` leaves mutants `pending`, the gate status
is `warn` and the exit code is 0, because you set that limit. A stop for low
memory is not a limit that you set. If the run stops for low memory with
`--fail-on-survivors` and no survivor, the exit code is 2, because the gate has
no pass or fail result. In each of these cases,
the next run reads the finished results from the journal. See
[Cache and journal](#cache-and-journal).

## Changed code

`--in-diff <REF>` runs only the mutants on a changed line:

- The base is the fork point of the ref and `HEAD` (`git merge-base`). A later
  commit on the ref does not count.
- The diff compares the base with the working tree, so an uncommitted edit counts.
  Each line of an untracked file counts as changed, unless git ignores the file.
- The command selects a mutant when a line of its text changed. For a mutant that
  replaces a function body, a changed line in that body is sufficient.
- A deletion alone selects no mutant, because no line of the new text changed.
- The `in_diff` field has `ref`, `base`, `changed_files` and `mutants_outside`.
- When no mutant is on a changed line, the command builds nothing and exits 0.
- When git cannot resolve the ref, the command exits 2. This also occurs outside a
  repository, and in a shallow clone with no fork point. The command does not
  switch to another scope.

With `--in-diff`, the score applies only to the changed code.

## Inspect

`--inspect <ID>` shows one mutant and the tests that execute it. The command runs
no mutant. In a workspace with more than one package, you do not need
`--package`, because cargo-fallow finds the package from the id.

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

The command reads the coverage map of the last run. When the source changed or
the cache has no map, the command builds the map first. To do this, it compiles a
copy with coverage and runs the tests with no mutant, under the memory guard. The
next run uses this map again. This build needs the LLVM tools. In the JSON form,
`kind` is `mutants-inspect`, and the report has the fields `mutant` and `coverage`.
`coverage.state` has one of these values:

| `state` | Meaning |
| --- | --- |
| `covered` | `tests` lists the tests that execute a line of the mutant. |
| `not_covered` | The map has data on the lines, but no test executes them. |
| `no_line_data` | The map has no data on the lines, so a run of the mutant includes each test. |
| `constant_context` | The compiler can evaluate this code, so a run of the mutant includes each test. |

`coverage.tests_without_data` lists the tests with no usable coverage data. A run
includes these tests for each mutant. The report has no status, because the
command does not read an old result. For an unknown id, the command exits 2. The
id of a mutant changes when the text of the mutated code changes.

`--no-build` reads the map only from the cache, so the command compiles nothing
and runs no test. Use this flag when the caller must not start a build. When the
cache has no map for this source, the command exits 2 with the code
`CARGO_FALLOW_NO_CACHED_MAP`. The `help` field gives the `--inspect` command that
builds the map. The cache key of the map includes `Cargo.lock`. In a workspace
without `Cargo.lock`, Cargo makes a new lockfile in each run, so `--no-build`
cannot find the map. Run `cargo generate-lockfile` first.

## List

`--list` lists the mutants of one package. It does not compile the project and
does not run a test. In a workspace with more than one package, `--list` needs
`--package`. `--in-diff`, `--file`, `--tier`, `--no-schema` and `--no-selection`
select the mutants in the same way as in a run.

The report has the same shape as a run report, and `kind` is `mutants`. These
values are specific to `--list`:

- Each mutant has the status `pending`, and `actions` is empty.
- The gate is `skipped`, so the exit code is 0.
- `schema.candidates` and `rebuild.candidates` give the number of mutants in each
  tier.
- `tier` is the planned tier. If a mutant breaks the schema build, a run moves it
  to the rebuild tier.
- A run can find that no build compiles the code of a rebuild mutant. The run then
  reports that mutant as `no_coverage`.
- `selection.map` is `cached` when the cache has a map for this source, else
  `none`. `cached` means that `--inspect <ID> --no-build` can show the tests of a
  mutant.
- The map affects only the `selection` part of the report.
  `selection.mutants_without_line_data` is always 0, because `--list` does not
  compare a mutant with the map.

To find the map, `--list` and `--inspect --no-build` copy the source tree to a
temporary folder and hash it, as a run does. The time of this copy grows with
the size of the checkout. With `--list`, you can skip the copy with `--no-cache` or `--no-selection`.
`selection.map` is then `none`.

Both modes run `rustc -vV` for the cache key, and with `--in-diff` they also run
`git`. Neither mode compiles the project.

## Tiers

| Tier | Operators | How it runs |
| --- | --- | --- |
| `schema` | In a run of both tiers, each operator outside a constant context. Else, only the type-safe operators. | All mutants go into one build. At run time, a switch selects one mutant in each test process, so a mutant needs no compile of its own. If a mutant breaks the build, the removal loop removes it, and that mutant then runs as a rebuild. |
| `rebuild` | A mutant in a constant context, a deleted arm with a `let` guard, and each mutant that the removal loop removed | Each mutant gets its own rebuild of one scratch copy that the command uses again for each mutant. The rebuild compiles only the test binaries of the selected tests. |

`relational-operator`, `logical-operator` and `negate-condition` are type-safe by
construction. `arithmetic-operator`, `unary-operator`, `return-value`,
`deleted-match-arm` and `match-guard` are not type-safe. For example, the type of
`a + b` can have no `Sub` implementation. In a run of both tiers, these mutants
also go into the schema build. If one of them breaks the build, the removal loop
finds it (from the compiler error, or by bisection) and moves it to the rebuild
tier. In the rebuild tier, a mutant that does not compile gets the status
`compile_error`.

`--tier schema` keeps only the type-safe operators. `--tier rebuild` runs only the
other operators, as rebuilds. `--no-schema` runs each mutant as a rebuild.

A switch has one of these forms:

- For an operator or a match guard, the switch is a `match` on the active mutant
  around the expression.
- For a return value, the switch is the same `match`, as the whole body of the
  function.
- For a deleted arm, the switch is a new guard on the arm. The guard is false when
  the mutant is active, so the value goes to the next arm.

A switch reads a value at run time, but code in a constant context cannot read a
run-time value. For that reason, a mutant in a `const fn` or in a `const` item
stays in the rebuild tier.

## Status

| Status | Meaning |
| --- | --- |
| `killed` | A test failed with the mutant. The test failed again in a second check with no other test process of the run. `killed_by` names the test. |
| `survived` | Each test passed, also in the retry. A test can fail while other tests run in parallel, and then pass when it runs alone. Such a failure also gives this status, and `reason` says so. This applies to a failure in the first run and to a failure in the retry. With `-j 1`, no other test runs in parallel with the mutant, so such a failure in the retry gives `flaky`. |
| `timeout` | The tests did not finish within the time limit of cargo-fallow, or the nextest config of the project stopped a test at its limit. When the limit is at most 10 times the baseline, the mutant gets a retry with twice the limit, because host load can explain the timeout. If a test fails in that retry and the failure does not repeat when the test runs alone, the status stays `timeout`. |
| `flaky` | Two runs of the mutant gave different results. |
| `no_coverage` | No test executes the mutant, no build compiles it (for example, it is under a `cfg` that is off), or the package has no test that passes. `reason` says which. |
| `compile_error` | The mutant does not compile, or its compile took longer than the limit. The limit is 3 times the clean build, with a minimum of 30 seconds. A mutant in a `const fn` can loop in the compiler. |
| `runtime_error` | The run gave no test result. `reason` says why, for example a kill by the memory guard. |
| `pending` | The run stopped before this mutant, because of `--time-budget`, `--max-mutants`, `--no-run` or low memory. `--list` gives each mutant this status. |

The score is `killed + timeout` over `killed + timeout + survived`.

A switch can change the result of a test also when no mutant is active. The
`match` of a switch can drop a temporary value earlier. Also, `column!()` on the
line of a site returns another value. `baseline.regressions` and the human report
name such a test. The test does not run in the schema tier, so each mutant that it
executes runs as a rebuild. A doctest has no line data. When the switch changes
the result of a doctest, therefore, each schema mutant moves to the rebuild tier. This move
also occurs with `--tier schema`. `schema.moved_to_rebuild` gives the number of
moved mutants.

With `--no-retry`, no mutant gets a second run, and no kill gets a second check.

## Safety

A mutant can allocate memory without limit, and a test timeout does not protect
the machine from it. The memory guard is on by default, and it does these steps:

- When one test process of a mutant uses more than `--memory-limit` (default
  4096), the guard kills each test process of that mutant. The mutant gets
  `runtime_error` with a reason.
- When the free memory drops below `--low-memory-percent` (default 25), the guard
  kills the largest process tree.
- When the guard cannot read the memory data, it kills the process trees.
- A test can start a child process that continues after the test. After each test
  run, the command kills each such process that runs a program from the work
  folder. A program from outside the work folder can continue, because it contains
  no mutant.
- When `cargo fallow` dies, a watchdog process kills the process trees and removes
  the temporary work folder. This also works after a signal or a panic.

On macOS, the guard reads the footprint from `top`, because RSS does not count
compressed pages. On Linux, it reads RSS plus swap from `/proc`, and the free
memory from the cgroup limit or `/proc/meminfo`. Other systems have no memory
probe, so the command stops unless you pass `--no-memory-guard`.

`--jobs` defaults to a quarter of the cores, with a minimum of 1 and a maximum of
2. The time limit of each mutant run is 5 times the baseline of the tests that it
runs, with a minimum of 20 seconds. `--timeout` sets another limit.

## Test selection

For each mutant, the command runs only the tests that execute the lines of the
mutant, and it runs the fastest test first. A mutant that no test executes gets
`no_coverage`, and the command does not run it. A rebuild mutant of that kind also
gets no build. A mutant that does not compile can thus get `no_coverage`
instead of `compile_error`.

The command builds the coverage map in three steps:

1. The command compiles the unmodified copy one time with
   `-C instrument-coverage`. This build needs `llvm-profdata` and `llvm-cov` from
   the active toolchain. When they are missing, the command stops with an error.
   To install them, run `rustup component add llvm-tools`.
2. Each test that passes in the baseline runs alone through nextest, with its own
   profile folder.
3. The command converts the profiles of each test to line ranges immediately, and
   then deletes the profiles.

When coverage data is missing, the command adds tests to the selection. A test
runs for each mutant when it wrote no usable data, or when its name is not plain
text. For a
mutant on a line that has no coverage data in any test, each test runs. Each test
also runs for a mutant in a constant context, such as a `const fn`. The compiler
can evaluate that code, so no test process executes it. The `selection` part of
the report counts these tests and these mutants.

Some tests pass in the baseline but fail when they run alone, because they need
another test. In a run of selected tests, the command cannot use the result of
such a test, so that result does not set the status of a mutant.
`selection.tests_fail_alone` and the human report name these tests.

With `--file` or `--in-diff`, the map keeps line data only for the selected files,
and the command does not write that map to the cache. The command still runs each
test one time to build the map, because no other data shows which test executes a
line.

The command adds the coverage flag only to the crates of the selected package.
The other crates compile without coverage counters, and the map has no data on
them.

`--no-selection` runs each test for each mutant, and skips the coverage build. Use
it to check a result. Test selection can change a status in only two ways:

- `survived` or `compile_error` becomes `no_coverage`.
- `killed` and `timeout` can swap when one test fails and another test never ends
  with the same mutant. The run stops at the first result, and test selection puts
  the tests in another order. Both statuses count as detected, so the score does
  not change.

If you see any other difference, report it as a bug.

The score leaves out `no_coverage`, so the first change can raise the score. A
mutant that no test executes no longer counts as a survivor. Read the score
together with the `no_coverage` count.

### Doctests

On stable Rust, doctests have no coverage data. Each doctest is a separate binary,
and the command cannot add its data to the map. Test selection therefore does
not apply to doctests, with these results:

- When the selected tests do not kill a mutant, the command still runs all
  doctests against it, as a run without selection does. A doctest can then kill
  the mutant.
- When no nextest test executes a mutant, the command also runs all doctests
  against it. A failed doctest gives `killed`. When the doctests pass, the mutant
  gets `no_coverage`, with a reason that says so. The map has no data on
  doctests, so the command cannot know if a doctest executed the mutant. For
  that reason, the status is not `survived`. Without selection,
  the same mutant gets `survived`.
- `--no-doctests` skips the doctests, and the `gaps` part of the report records
  this.

### Cache and journal

The command stores two files in `target/fallow-mutation/<package>/` of the
workspace, so `cargo clean` removes them. `--no-cache` disables both files.

- The coverage map has a key that is a hash of each file in the workspace copy,
  the compiler version, `RUSTFLAGS` and the cargo-fallow version. Any change to a
  source file or a test gives a new map.
- The journal has one line for each mutant with a result. Its key also includes
  the options that can change a status. When a run stops (`--time-budget`,
  `--max-mutants`, low memory, a crash), the journal stays on disk. The next run
  with the same key reports those results and runs only the other mutants.
  `--max-mutants` then counts only the new runs. A mutant with `runtime_error` runs
  again, because the state of the machine can cause that status. The command
  deletes the journal when each mutant has a result.

## Limits

The `gaps` part of the report counts the code that a run does not test:

- The command compiles only the default features. A mutant under another feature
  set gets `no_coverage`.
- The command makes no mutants in macro bodies or macro arguments. The report
  counts the operator tokens in them.
- The command makes no mutants in a file behind a module that a macro declares.
  The report counts such files.
- Proc-macro targets have no mutants.
- The copy has no `.git` folder. A test that reads `.git` fails in the baseline,
  so its result does not decide a status.
- A test can lose coverage data and still pass. For example, a child process that
  aborts writes no profile. A mutant that only such a child executes gets
  `no_coverage`. A child with an empty environment writes a profile under the
  default name in the working folder of the test. The command finds that file,
  and the test then runs for each mutant.
- The coverage counters are not atomic. In theory, a race in a test with threads
  can hide an executed line. The command does not enable atomic counters.
- Without `--test-workspace`, only the tests of the package run. The tests of a
  workspace member that depends on the package can also detect a mutant. A
  mutant can therefore survive without the flag and be killed with it.
  `gaps.untested_dependents` and the human report name those members. With the
  flag, each `survived` and `no_coverage` mutant runs each test of the workspace.
  This stage uses no test selection, and a test that passes does not run a second
  time. If a run stops in this stage, the next run repeats the stage.
  `workspace_tests` has the counts.
- A `.cargo/config.toml` above the workspace root also applies to the copy. But a
  relative path in that file then starts at the work folder.
- A test that closes each inherited file descriptor disables the schema switch. A
  system with no `/dev/fd` has the same effect. A mutant can then survive. To
  check such a result, run with `--no-schema`.
