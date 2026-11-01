# cargo-fallow

`cargo fallow` is the Rust part of Fallow. It is a Cargo subcommand that
analyzes a Cargo workspace. It uses `cargo metadata` and syntactic analysis
with `ra_ap_syntax`, the parser of rust-analyzer. It runs on stable Rust and needs no nightly toolchain.

## Status

Pre-release. The `check` command reads the workspace and reports two gates. The
`mutation` command runs mutation tests on one package. The JSON shapes can change
before the first release.

## Install

Install from source only. The crate is not on crates.io.

```bash
git clone https://github.com/fallow-rs/cargo-fallow.git
cd cargo-fallow
cargo install --path .
```

`cargo-fallow` installs one binary, `cargo-fallow`. It never installs a binary
named `fallow`. That name belongs to the JavaScript CLI.

## Usage

```bash
cargo fallow                      # same as `cargo fallow check`
cargo fallow check --format json --quiet
cargo fallow check --manifest-path path/to/Cargo.toml
cargo fallow check --fail-on-parse-error
```

You can also run the binary directly: `cargo-fallow check`.

| Flag | Meaning |
| --- | --- |
| `--manifest-path <PATH>` | The `Cargo.toml` of the workspace. The default is the manifest that Cargo finds from the current directory. |
| `--format human\|json\|sarif` | The output format. The default is `human`. |
| `--quiet` | Hide the progress lines on stderr. |
| `--fail-on-parse-error` | Exit 1 when a target root file does not parse. |

`cargo fallow` runs `cargo metadata --no-deps`. It does not resolve
dependencies, it does not use the network and it does not write `Cargo.lock`.


## Mutation testing

`cargo fallow mutation` makes small changes (mutants) in the source of one
package, runs the tests against each mutant, and reports the mutants that no test
detects. It needs `cargo-nextest`. It never changes the checkout: it works in a
copy. A survivor does not fail the run.

```bash
cargo fallow mutation --package my-crate
cargo fallow mutation --tier schema --format json --quiet
cargo fallow mutation --max-mutants 200 --time-budget 3600
```

### Tiers

| Tier | Operators | How it runs |
| --- | --- | --- |
| `schema` | `relational-operator`, `logical-operator`, `negate-condition` | All mutants go into one build. A runtime switch selects one mutant in each test process, so a mutant costs no compile. The removal loop removes a mutant that breaks the build, and that mutant then runs in a rebuild. |
| `rebuild` | `arithmetic-operator`, `unary-operator`, `return-value`, `deleted-match-arm` | Each mutant gets its own rebuild of one reused scratch copy, because these operators are not type-safe by construction. |

### Status

| Status | Meaning |
| --- | --- |
| `killed` | A test failed with the mutant. `killed_by` names the first one. |
| `survived` | Each test passed, also in the retry. |
| `timeout` | The tests did not end within the limit. A limit at most 10 times the baseline gets a retry with twice the limit, because host load can explain it. |
| `flaky` | Two runs of the mutant disagree. |
| `no_coverage` | No test executes the mutant, no build reads it (for example a `cfg` that is off), or the package has no test that passes. `reason` says which. |
| `compile_error` | The mutant does not compile. |
| `runtime_error` | The run has no test result. `reason` says why, for example a kill by the memory guard. |
| `pending` | The run stopped first: `--time-budget`, `--max-mutants`, `--no-run` or low memory. |

The score is `killed + timeout` over `killed + timeout + survived`.

### Safety

A mutant can allocate without limit, and a test timeout does not protect the
machine. The memory guard is on by default:

- It kills the test processes of a mutant when one process passes
  `--memory-limit-mib` (default 4096). The mutant gets `runtime_error` with a
  reason.
- It kills the largest process tree when the free memory drops under
  `--low-memory-percent` (default 25).
- It kills the process trees when it cannot read the memory data.
- A watchdog process kills the process trees when `cargo fallow` dies.

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
usable data, when it failed in the coverage build, or when its name is not plain
text. Each test runs for a mutant on a line that has no coverage data in any test,
and for a mutant in a constant context such as a `const fn`, because the compiler
can evaluate that code and no test process then executes it.
The `selection` part of the report counts these tests and these mutants.

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
- A test can lose coverage data and still pass: a child process that the test
  starts with an empty environment or that aborts writes no profile. A mutant that
  only such a child executes gets `no_coverage`.
- The coverage counters are not atomic. A race in a test with threads can in theory
  hide an executed line. The tool does not turn on atomic counters.
- A `rustflags` key under `[target]` in the Cargo config replaces the coverage
  flag. Then each test has no data, a warning says so, and each mutant runs each test.

## Output formats

- `human`: a short summary on stdout. Progress lines go to stderr.
- `json`: the report on stdout. The report has `schema_version`, `version`,
  `kind`, `language`, `workspace`, `total_issues`, `findings` and
  `gate_outcomes`.
- `sarif`: a SARIF 2.1.0 log on stdout. Each file of a failed gate is a
  result, and each failed gate is a tool execution notification. The level
  is `error` for an enforced failure and `warning` for other failures.

The gates are `error-severity-findings` and `parse-error`. They use the same
names as the gates of the JavaScript CLI. A gate fails the run when its
`status` is `fail` and `enforced` is `true`.

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

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | No enforced gate failed. |
| `1` | An enforced gate failed. |
| `2` | The input was not valid, or the run could not complete. |

## License

MIT. See `LICENSE`.
