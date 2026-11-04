# cargo-fallow GitHub Action

The action runs one `cargo fallow` command in a GitHub Actions job, and reports
the result in these ways:

- It writes the gates and the findings to the job summary.
- Each finding shows as an annotation on its line.
- With `sarif: true`, it uploads the SARIF log to code scanning.
- The job fails when an enforced gate fails, or when the run has an execution
  error.

The action is defined in `action.yml` at the root of this repository.

## Requirements

- Install a Rust toolchain before the action step, because each command runs
  `cargo metadata`.
- `mutants` also needs `cargo-nextest` and the `llvm-tools` component.
- When you use `base` with `size` or `mutants`, the checkout needs the history of
  the base ref. Set `fetch-depth: 0` on the checkout step.
- `sarif: true` needs `permissions: security-events: write`. A private or an
  internal repository also needs GitHub Advanced Security.

## Examples

This example runs `deps` as a check on each pull request:

```yaml
jobs:
  cargo-fallow:
    runs-on: ubuntu-latest
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@<sha>
        with:
          persist-credentials: false
      - uses: dtolnay/rust-toolchain@<sha>
        with:
          toolchain: stable
      - uses: fallow-rs/cargo-fallow@<sha>
        with:
          command: deps
```

This example runs the mutation tests on the changed lines of a pull request:

```yaml
      - uses: actions/checkout@<sha>
        with:
          persist-credentials: false
          fetch-depth: 0
      - uses: dtolnay/rust-toolchain@<sha>
        with:
          toolchain: stable
          components: llvm-tools
      - uses: taiki-e/install-action@<sha>
        with:
          tool: cargo-nextest
      - uses: fallow-rs/cargo-fallow@<sha>
        with:
          command: mutants
          base: auto
          mutants-package: my-crate
          mutants-time-budget: '1800'
```

With this configuration, the complexity findings go to code scanning, and the job
does not fail on them:

```yaml
    permissions:
      contents: read
      security-events: write
    steps:
      # checkout and toolchain as above
      - uses: fallow-rs/cargo-fallow@<sha>
        with:
          command: complexity
          fail-on-issues: 'false'
          sarif: 'true'
```

## Inputs

| Input | Default | Meaning |
| --- | --- | --- |
| `command` | `check` | `check`, `deps`, `files`, `visibility`, `boundaries`, `size`, `complexity`, `dupes` or `mutants`. |
| `working-directory` | `.` | The folder in which the command runs. |
| `manifest-path` | empty | The `Cargo.toml` of the workspace, relative to `working-directory`. |
| `format` | `human` | The format of the report in the log and in the `report` output: `human`, `json` or `sarif`. |
| `fail-on-issues` | `true` | Pass the gate flag to the command, so a failed gate fails the job. See [Gate flags](#gate-flags). |
| `args` | empty | More arguments, separated by spaces. The action does not expand them in a shell. The action rejects `--format` and `--quiet`, because it sets them itself. For example, `--changed-since origin/main` reports only the findings in the changed files. |
| `base` | empty | The ref to compare with. The action passes it as `--base` to `size` and as `--in-diff` to `mutants`. `auto` uses the base commit of the pull request, or on a push, the commit before the push. See [Base](#base). |
| `install` | `auto` | See [Install](#install). |
| `version` | empty | The version for `install: release` and `install: cargo`. When empty, the action uses the version of its own ref. |
| `sarif` | `false` | Upload the SARIF log to code scanning. The action does not upload for a pull request from a fork, because the token of that run is read-only. |
| `sarif-category` | empty | The code scanning category. When empty, the action uses `cargo-fallow-<command>`. Give each call its own category when two calls run the same command on other workspaces. Else the second upload replaces the alerts of the first. |
| `annotations` | `true` | Show each finding as an annotation. |
| `max-annotations` | `50` | The maximum number of annotations, and of findings in the job summary. Errors come first. GitHub shows at most 10 annotations of each level for a step. |
| `summary` | `true` | Write the job summary. |
| `changed-since` | empty | Report only the findings in the files that changed since this ref. The action passes it as `--changed-since`. Only the static commands use it. |
| `auto-changed-since` | `false` | On a pull request, pass the base commit of the pull request as `--changed-since` when `changed-since` is empty. |
| `baseline` | empty | A baseline file from `--save-baseline`, relative to `working-directory`. The action passes it as `--baseline`. |
| `save-baseline` | empty | Write the findings of the run to this file (`--save-baseline`). The run then exits 0. |
| `comment` | `false` | Post one sticky comment on the pull request, or update it. See [Pull request comment](#pull-request-comment). |
| `comment-key` | empty | The key of the sticky comment. Use letters, digits, `.`, `-` and `_`. When empty, the action uses the command name. Give each call its own key when two calls run the same command. |
| `artifacts-dir` | empty | The folder for the report files. When empty, the action makes a new folder in `RUNNER_TEMP` for each call. |
| `github-token` | `github.token` | The token for the code scanning check, the SARIF upload and the pull request comment. |
| `mutants-package` | empty | `--package` |
| `mutants-max-mutants` | empty | `--max-mutants` |
| `mutants-time-budget` | empty | `--time-budget`, in seconds |
| `mutants-jobs` | empty | `--jobs` |
| `mutants-timeout` | empty | `--timeout`, in seconds |

The `mutants-*` inputs apply only to `command: mutants`. For other flags, use
`args`. The `changed-since`, `auto-changed-since`, `baseline` and `save-baseline`
inputs apply only to the static commands. A `mutants` run ignores them and writes
a warning.

### Base

`base: auto` selects the commit to compare with in this way:

- On a pull request, it uses the base commit of the pull request.
- On a push, it uses the commit before the push (`github.event.before`).
- The push of a new branch or tag has no commit before it. The run then compares
  with no base and writes a notice. `size` then has no growth gate, and `mutants`
  runs each mutant.
- On another event, for example `workflow_dispatch`, `schedule` or
  `merge_group`, there is no commit to use, so the run also compares with no base.

A shallow checkout can miss the commit. In that case, the action fetches only that
commit with `--depth=1`, and the clone gets it as a shallow commit. When the fetch
fails, the run compares with no base, and the notice tells you to use
`fetch-depth: 0`. After a force push, the old commit can be missing from the
remote. Use `fetch-depth: 0` for a full comparison.

### Pull request comment

With `comment: true`, the action posts one comment on the pull request. The
comment has the same text as the job summary, with the gates and the findings.
The comment starts with a hidden marker that contains the `comment-key`. On the
next run of the job, the action finds the comment from this marker and updates
it. The action does not add a second comment.

```yaml
    permissions:
      contents: read
      pull-requests: write
    steps:
      - uses: actions/checkout@<sha>
        with:
          persist-credentials: false
          fetch-depth: 0
      - uses: fallow-rs/cargo-fallow@<sha>
        with:
          command: check
          auto-changed-since: 'true'
          comment: 'true'
```

- The step needs `pull-requests: write`. Without that permission, for example with
  the token of a Dependabot pull request, the action writes a warning and posts no
  comment. The job still passes or fails on its gates.
- A pull request from a fork has a read-only token, so the action posts no comment
  and writes a notice. In a `pull_request_target` run from a fork, the action also
  posts no comment.
- On an event with no pull request, the action posts no comment.
- The action updates a comment only when the author of that comment is the login
  of the token. For the default token, the author is `github-actions[bot]`.
- When the summary is longer than 60000 bytes, the action cuts the comment and
  adds a line that points to the job summary.
- `auto-changed-since` and `changed-since` need the history of the base commit, so
  set `fetch-depth: 0`.

### Gate flags

The action passes `--fail-on-issues` to each command. The commands in this table
also accept an older flag for the same gate. When `args` has the older flag or
`--fail-on-issues`, the action does not add `--fail-on-issues`.

| Command | Older flag |
| --- | --- |
| `boundaries` | `--fail-on-violations` |
| `size` | `--fail-on-growth` |
| `complexity` | `--fail-on-complexity` |
| `dupes` | `--fail-on-clones` |
| `mutants` | `--fail-on-survivors` |

`size` has a growth gate only with `--base` or `--staged`. To enable the gate, set
the `base` input.

An execution error (exit code 2) fails the job, also with
`fail-on-issues: false`.

### Without the action

`cargo fallow` can write the same annotations as the action, and a job summary
that looks the same in GitHub:

```yaml
- run: cargo fallow check --format github-annotations --quiet
- run: cargo fallow check --format github-summary --quiet >> "$GITHUB_STEP_SUMMARY"
```

The action does not use these two formats. It builds the annotations and the
summary with jq, from the JSON report and the SARIF log. A second run of
`cargo fallow mutants` takes minutes, so the action does not run the binary again
for each format.

## Outputs

| Output | Meaning |
| --- | --- |
| `exit-code` | The exit code of the JSON run: `0`, `1` or `2`. |
| `results` | The path of the JSON report, or of the JSON error document. |
| `sarif` | The path of the SARIF log. The paths in the log are relative to the repository root. Empty when the run has no SARIF log. |
| `report` | The path of the report in the `format` input. After an execution error, or for `mutants` with `format: human`, this is the path of the JSON report. |
| `issues` | The `total_issues` value of the report. |
| `gates-failed`, `gates-warned`, `gates-passed`, `gates-skipped` | The names of the gates with each status, separated by commas. |
| `mutation-score` | For `mutants` only, the mutation score in percent, with one decimal. |
| `error-code` | The `code` of the JSON error document, else empty. |
| `install-method` | `path`, `release`, `cargo` or `source`. |

## Install

| Method | What it does |
| --- | --- |
| `auto` | Uses `cargo-fallow` on the PATH when no `version` is set, or when its version is the `version` input. Else, it downloads the release binary of the version. When no release binary is available for the runner, it builds the source of the action ref. |
| `path` | Uses `cargo-fallow` on the PATH. |
| `release` | Downloads `cargo-fallow-<version>-<target>.tar.gz` (`.zip` on Windows) from the GitHub release `v<version>`. It checks the Ed25519 signature in the `.sig` file, then the SHA-256 in the `.sha256` file. Linux runners get the static `musl` binary. |
| `cargo` | Runs `cargo install --locked cargo-fallow` from crates.io. |
| `source` | Runs `cargo install --locked --path` on the source of the action ref. |

The action checks the signature first, and then the checksum:

- The signature check uses the Ed25519 public key in
  `action/scripts/verify-signature.mjs`. The release workflow signs each archive
  with the private key of that pair. If someone replaces an archive and its
  checksum file, this check fails.
- The checksum file comes from the same release as the archive, so the checksum
  check finds a damaged download.
- The signature does not include the version. After the unpack, the action runs
  `cargo-fallow --version`, and stops the job when the version is not the
  requested version. This stops an older signed archive from replacing a newer
  one.
- A bad signature, a missing `.sig` file or a checksum mismatch stops the job.
  After such a failure, `auto` does not build from source.
- The check needs `node`, and each GitHub-hosted runner has `node`. On a runner
  without `node`, `release` stops the job, and `auto` builds the source.

## How the action runs a command

The action first runs the command with `--format json`. The action uses the exit
code of that run to pass or fail the job. For a static command, the action runs the command
again for the SARIF log. When `format` is `human`, it runs the command a third
time for the human report. `mutants` runs the tests of each mutant, so the action
runs `mutants` only one time. The SARIF log of `mutants` comes from
`action/jq/mutants-sarif.jq`. `action/tests/run.sh` compares the output of that
filter with the SARIF output of a real run.

## Tests

```bash
shellcheck -x action/scripts/*.sh action/tests/run.sh
bash action/tests/run.sh
```

The `GitHub Action` workflow runs these tests, and runs the action on this
repository. Pull requests and pushes to `main` use only Linux. The weekly run and
a manual run also use macOS. The action self-test also runs on Windows in those
two runs.
