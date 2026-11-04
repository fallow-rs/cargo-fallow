# GitLab CI

`ci/gitlab-ci.yml` is a template for GitLab CI. The job runs one static
command of cargo-fallow and writes a code quality report, so the merge request
widget shows the findings. It can also post one merge request note with the
markdown report, and it updates that note on the next run.

```yaml
include:
  - remote: 'https://raw.githubusercontent.com/fallow-rs/cargo-fallow/<ref>/ci/gitlab-ci.yml'

cargo-fallow:
  extends: .cargo-fallow
  variables:
    CARGO_FALLOW_REF: "<ref>"
    CARGO_FALLOW_COMMENT: "true"
```

Use the same `<ref>` in the include and in `CARGO_FALLOW_REF`: the job
downloads `ci/scripts/run.sh` and `ci/scripts/comment.sh` from that ref. When
you vendor the template and `ci/scripts/` into your repository, the job uses
the vendored scripts.

## Variables

| Variable | Default | Meaning |
| --- | --- | --- |
| `CARGO_FALLOW_COMMAND` | `check` | `check`, `deps`, `files`, `visibility`, `boundaries`, `size`, `complexity` or `dupes`. The template does not run `mutants`, because it builds and runs the tests of each mutant. |
| `CARGO_FALLOW_ARGS` | empty | More flags for the command, split on white space. |
| `CARGO_FALLOW_FAIL_ON_ISSUES` | `true` | Fail the job when an enforced gate fails. |
| `CARGO_FALLOW_CHANGED_SINCE` | empty | A ref for `--changed-since`. |
| `CARGO_FALLOW_AUTO_CHANGED_SINCE` | `true` | In a merge request pipeline without `CARGO_FALLOW_CHANGED_SINCE`, give `--changed-since` the merge request base. The code quality report always has the full scope: GitLab compares it with the report of the target branch. |
| `CARGO_FALLOW_COMMENT` | `false` | Post or update one merge request note. |
| `CARGO_FALLOW_COMMENT_KEY` | the command | The marker of the note. Give each job with a note its own key. |
| `CARGO_FALLOW_INSTALL` | `cargo` | `cargo` runs `cargo install cargo-fallow`. `path` uses the `cargo-fallow` on the `PATH`. |
| `CARGO_FALLOW_VERSION` | empty | The crates.io version for `cargo`. Empty means the newest version. Pin it: the cache then keeps the binary, and the next job does not build it. |
| `CARGO_FALLOW_REF` | empty | The ref of the scripts to download. Give the ref of the include. A job with no vendored scripts and no ref stops with an error. |
| `CARGO_FALLOW_SCRIPTS_DIR` | `ci/scripts` | The folder of vendored scripts. |

The template sets `GIT_DEPTH: "0"`, because the changed-file scope needs the
base commit of the merge request. A job that sets its own `before_script`
replaces the install and the script download of the template, so it must do
them itself.

## Outputs

The job keeps three artifacts, also when it fails:

- `gl-code-quality-report.json`: the `--format codeclimate` report, which
  GitLab reads as the code quality report of the job.
- `cargo-fallow-results.json`: the `--format json` report. Its exit code is
  the verdict of the job.
- `cargo-fallow-report.md`: the `--format markdown` report, with
  `CARGO_FALLOW_COMMENT`.

## The merge request note

The note needs `GITLAB_TOKEN`, a project or personal access token with the
`api` scope, as a masked CI/CD variable. `CI_JOB_TOKEN` cannot write notes.
Without the token, or when an API call fails, the job gives a warning and
posts no note. The gates of the run decide the job, not the note. A hidden
marker, `<!-- cargo-fallow-comment:<key> -->`, names the note of each key. A
report over 900000 characters is cut.
