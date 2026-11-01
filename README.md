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
| `no_coverage` | No build reads the mutant (for example a `cfg` that is off), or the package has no test that passes. |
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
has a time limit of 5 times the baseline of its build, with a minimum of 20
seconds. `--timeout` sets another value.

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
