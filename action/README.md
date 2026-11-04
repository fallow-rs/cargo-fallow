# cargo-fallow GitHub Action

The action runs one `cargo fallow` command in a GitHub Actions job. It:

- writes the gates and the findings to the job summary,
- shows each finding as an annotation on its line,
- uploads the SARIF log to code scanning when you set `sarif: true`,
- fails the job when an enforced gate fails, or when the run has an error.

The action file is `action.yml` at the root of this repository.

## Requirements

- Install a Rust toolchain before the action. Each command runs
  `cargo metadata`.
- `mutants` also needs `cargo-nextest` and the `llvm-tools` component.
- `size` and `mutants` with `base` need the history of the base ref, so set
  `fetch-depth: 0` on the checkout.
- `sarif: true` needs `permissions: security-events: write`. A private
  repository also needs GitHub Advanced Security.

## Examples

A check on each pull request:

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

Mutation tests on the changed lines of a pull request:

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

Complexity findings in code scanning, without a failed job:

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
| `format` | `human` | The report in the log and in the `report` output: `human`, `json` or `sarif`. |
| `fail-on-issues` | `true` | Give the gate flag of the command, so a failed gate fails the job. See [Gate flags](#gate-flags). |
| `args` | empty | More arguments, separated by spaces. The action does no shell expansion on them. `--format` and `--quiet` are not permitted. For example, `--changed-since origin/main` keeps only the findings in the changed files. |
| `base` | empty | The ref to compare with: `--base` for `size`, `--in-diff` for `mutants`. `auto` takes the base commit of the pull request, or on a push the commit before the push. See [Base](#base). |
| `install` | `auto` | See [Install](#install). |
| `version` | empty | The version for `install: release` and `install: cargo`. When empty, the version of the action ref. |
| `sarif` | `false` | Upload the SARIF log to code scanning. A pull request from a fork does not upload, because its token is read-only. |
| `sarif-category` | empty | The code scanning category. When empty, `cargo-fallow-<command>`. Give each call its own category when two calls run the same command on other workspaces. |
| `annotations` | `true` | Show each finding as an annotation. |
| `max-annotations` | `50` | The most annotations, and the most findings in the job summary. The errors come first. GitHub shows at most 10 annotations of each level for a step. |
| `summary` | `true` | Write the job summary. |
| `changed-since` | empty | `--changed-since`: report only the findings in the files that changed since this ref. The static commands take it. |
| `auto-changed-since` | `false` | On a pull request, give the base commit of the pull request as `--changed-since`, when `changed-since` is empty. |
| `baseline` | empty | `--baseline`: a baseline file from `--save-baseline`, relative to `working-directory`. |
| `save-baseline` | empty | `--save-baseline`: write the findings of the run to this file. The run then exits 0. |
| `comment` | `false` | Post or update one sticky comment on the pull request. See [Pull request comment](#pull-request-comment). |
| `comment-key` | empty | The key of the sticky comment. When empty, the command. Give each call its own key when two calls run the same command. |
| `artifacts-dir` | empty | The folder for the report files. When empty, each call gets a new folder in `RUNNER_TEMP`. |
| `github-token` | `github.token` | The token for the code scanning check and the upload. |
| `mutants-package` | empty | `--package` |
| `mutants-max-mutants` | empty | `--max-mutants` |
| `mutants-time-budget` | empty | `--time-budget`, in seconds |
| `mutants-jobs` | empty | `--jobs` |
| `mutants-timeout` | empty | `--timeout`, in seconds |

The `mutants-*` inputs apply only to `command: mutants`. For other flags, use
`args`.

### Base

`base: auto` picks the commit to compare with:

- On a pull request, the base commit of the pull request.
- On a push, the commit before the push (`github.event.before`).
- On the push of a new branch, no commit: the run compares with no base and
  writes a notice. `size` then gives no growth gate, and `mutants` runs each
  mutant.

- On another event, for example `workflow_dispatch`, `schedule` or
  `merge_group`, no commit.

A shallow checkout can miss the commit. The action then fetches that one
commit with `--depth=1`, so the clone gets it as a shallow commit. When the
fetch fails, the run compares with no base, and the notice names
`fetch-depth: 0`. After a force push, the old commit can be gone from the
remote. Use `fetch-depth: 0` for a full comparison.

### Pull request comment

`comment: true` posts one comment on the pull request with the gates and the
findings, the same text as the job summary. A hidden marker names the comment
of each `comment-key`, so the next run of the job updates the comment and adds
no new one.

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

- The step needs `pull-requests: write`. A token without it, for example the
  token of a Dependabot pull request, gives a warning and no comment. The run
  still passes or fails on its gates.
- A pull request from a fork has a read-only token, so the step posts no
  comment and writes a notice. A `pull_request_target` run from a fork also
  posts no comment.
- An event with no pull request posts no comment.
- The step updates only a comment of its own author: the login of the token,
  or `github-actions[bot]` for the default token.
- A comment over 60000 characters is cut, with a line that points to the job
  summary.
- `auto-changed-since` and `changed-since` need the history of the base
  (`fetch-depth: 0`).

### Gate flags

The action gives `--fail-on-issues` to each command. The commands below also
accept an older flag for the same gate. When `args` holds the older flag, the
action does not add `--fail-on-issues`.

| Command | Older flag |
| --- | --- |
| `boundaries` | `--fail-on-violations` |
| `size` | `--fail-on-growth` |
| `complexity` | `--fail-on-complexity` |
| `dupes` | `--fail-on-clones` |
| `mutants` | `--fail-on-survivors` |

`size` has a growth gate only with `--base` or `--staged`. Set the `base`
input to turn the gate on.

An execution error (exit code 2) fails the job also with
`fail-on-issues: false`.

## Outputs

| Output | Meaning |
| --- | --- |
| `exit-code` | The exit code of the JSON run: `0`, `1` or `2`. |
| `results` | The path of the JSON report, or of the JSON error document. |
| `sarif` | The path of the SARIF log. Its paths start at the repository root. |
| `report` | The path of the report in the `format` input. |
| `issues` | `total_issues` of the report. |
| `gates-failed`, `gates-warned`, `gates-passed`, `gates-skipped` | Gate names, separated by commas. |
| `mutation-score` | `mutants` only: the score in percent, with one decimal. |
| `error-code` | The `code` of the JSON error document, else empty. |
| `install-method` | `path`, `release`, `cargo` or `source`. |

## Install

| Method | What it does |
| --- | --- |
| `auto` | Uses `cargo-fallow` on the PATH, when it has the `version` input or no `version` is set. Else it downloads the release binary of the version. Else it builds the source of the action ref. |
| `path` | Uses `cargo-fallow` on the PATH. |
| `release` | Downloads `cargo-fallow-<version>-<target>.tar.gz` (`.zip` on Windows) from the GitHub release `v<version>`. It checks the Ed25519 signature in the `.sig` file, then the SHA-256 in the `.sha256` file. Linux runners get the static `musl` binary. |
| `cargo` | Runs `cargo install --locked cargo-fallow` from crates.io. |
| `source` | Runs `cargo install --locked --path` on the source of the action ref. |

The action checks the signature before it checks the checksum:

- The signature check uses the Ed25519 public key in
  `action/scripts/verify-signature.mjs`. The release signs each archive with
  the private key of that pair. A replaced archive with a new checksum file
  fails this check.
- The checksum file comes from the same release as the archive, so it finds a
  damaged download.
- The signature does not name the version. After the unpack, the action runs
  `cargo-fallow --version` and stops the job when the version is not the
  requested version. An older signed archive cannot replace a newer one.
- A bad signature, a missing `.sig` file or a checksum mismatch stops the job.
  `auto` does not fall back to a source build after such a failure.
- The check needs `node`, which each GitHub-hosted runner has. On a runner
  without `node`, `release` stops the job and `auto` builds the source.

## How the action runs a command

The JSON run gives the verdict. For a static command, the action runs the
command again for the SARIF log, and again for the human report when
`format` is `human`. `mutants` runs the tests of each mutant, so the action
runs it one time. The SARIF log of `mutants` comes from
`action/jq/mutants-sarif.jq`, and `action/tests/run.sh` compares that filter
with the SARIF output of a real run.

## Tests

```bash
shellcheck -x action/scripts/*.sh action/tests/run.sh
bash action/tests/run.sh
```

The `GitHub Action` workflow runs these tests and runs the action on this
repository on Linux, macOS and Windows.
