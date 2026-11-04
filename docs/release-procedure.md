# Release procedure

This document describes how the owner releases `cargo-fallow`. The tag comes
last. The release workflow publishes the crate while the tag is still absent.
Then the owner creates the signed tag and the GitHub release on the release
commit.

## What the release workflow does

The owner starts `.github/workflows/release.yml` by hand on `main`. The run has
two inputs:

- `version`: the version to release, for example `0.2.0`. A dry run can leave
  it empty. The run then uses the version in `Cargo.toml`.
- `dry-run`: true by default. A dry run checks the package, builds the
  binaries and publishes nothing.

The workflow has no tag trigger. It never creates a tag or a GitHub release.
The jobs run in this order:

1. `plan` runs `scripts/verify-release-context.mjs`. A release stops when one
   of these is true:
   - the ref is not `main`;
   - the subject of the HEAD commit is not exactly `chore: release vX.Y.Z`;
   - `origin/main` is not the HEAD commit;
   - the version input is not the version in `Cargo.toml`;
   - the tag `vX.Y.Z` already exists on the remote.

   Then `plan` runs `scripts/verify-release-metadata.mjs` on `CHANGELOG.md`.
   A release stops when the `## [X.Y.Z] - YYYY-MM-DD` section is missing, is
   empty, has no real date, has an em dash, or repeats a `###` heading. It
   also stops when `[Unreleased]` is not empty or when a fragment is left in
   `changelog.d/`. Then `plan` checks the package file list and runs
   `cargo publish --dry-run`. A dry run reports the context and metadata
   problems as warnings.

   Last, a release runs `scripts/verify-release-ci.mjs` on the release
   commit. A pull request runs only part of CI, and the push to `main` runs
   the rest. The script waits up to 20 minutes for each push run and each
   manual run of that commit. It does not count pull request runs and
   weekly runs. For each workflow, the newest run decides. A release stops
   when that run failed, or when one of these workflows has no successful
   run: `CI`, `Commitlint`, `Fuzz Smoke`, `GitHub Action` and
   `GitLab template`. A dry run skips this step.
2. `build` builds `cargo-fallow` for five targets:
   `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`,
   `x86_64-apple-darwin`, `aarch64-apple-darwin` and `x86_64-pc-windows-msvc`.
   The job writes a `.sha256` file for each archive.
3. `attest` writes a build provenance attestation for each archive.
4. `sign` uses the `ED25519_BINARY_SIGNING_PRIVATE_KEY` secret to write an
   Ed25519 signature `<archive>.sig` for each archive. Then the job verifies
   each signature with the public key in `action/scripts/verify-signature.mjs`.
   This key pair is for cargo-fallow only.
5. `publish-crate` publishes the crate to crates.io with trusted publishing.
   The job gets a short-lived token through OIDC. The repository stores no
   crates.io token. A run again after a partial failure skips a version that
   is already published.
6. `release-assets` checks that the tag is still absent. Then it puts the
   archives, the `.sha256` files, the `.sig` files and one `SHA256SUMS` file
   into one flat `release-assets` artifact. It checks `SHA256SUMS` and the
   count of the files.
7. `release-ready` ("Release ready for signed tag") checks that the tag is
   still absent. Its summary shows the command for the last step.

Jobs 3 to 7 run only for a release. Each job that holds a credential (`attest`,
`sign` and `publish-crate`) runs in the `release` environment.
`.github/scripts/release-workflow.test.mjs` checks the trigger, the order of
the jobs, the permissions, the environment of each job and the action pins.

After the owner publishes the GitHub release, `release-published.yml` runs
`scripts/verify-release-metadata.mjs` on the title and the body. It checks the
`vX.Y.Z: ` title prefix, a body that is not empty, the compare URL and the em
dash. A failure there is repairable: the title and the body of an immutable
release can change with `gh release edit`.

## Changelog fragments

A pull request does not edit `CHANGELOG.md`. It adds one fragment file to
`changelog.d/`. `changelog.d/README.md` describes the format. The
`Changelog fragments` CI step runs
`node scripts/changelog-fragments.mjs check --base origin/main`. The
pre-commit hook runs the same check with `--pending-release`. The release
commit collects the fragments.

## Prepare the release commit

1. Make a branch from the newest `main`: `git switch -c chore/release-vX.Y.Z`.
2. Run `node scripts/changelog-fragments.mjs collect`. The command moves each
   fragment into `[Unreleased]` and deletes the fragment files.
3. In `CHANGELOG.md`, rename `## [Unreleased]` to
   `## [X.Y.Z] - YYYY-MM-DD` and add a new empty `## [Unreleased]` section
   above it. Check each entry against the commits since the previous tag.
4. Set `version = "X.Y.Z"` in `Cargo.toml`, and run `cargo check` so that
   `Cargo.lock` has the new version.
5. Run `node scripts/verify-release-metadata.mjs --tag vX.Y.Z`. It runs the
   same metadata check as the `plan` job.
6. Commit with the subject `chore: release vX.Y.Z`. The pre-commit hook sees
   an emptied `[Unreleased]` section with the new version heading, and accepts
   it when the heading has the version of `Cargo.toml`. The CI check accepts
   a new version heading only in a branch with the commit
   `chore: release vX.Y.Z` of that version. If an older hook rejects the
   `collect` result, set `HK_SKIP_STEPS=changelog-fragments` for this commit
   only.
7. Open a pull request. When every check passes, merge it with the exact
   subject. A default squash subject adds ` (#N)`, and the `plan` job then
   stops:

   ```bash
   gh pr merge <N> --squash --match-head-commit <head SHA> \
     --subject "chore: release vX.Y.Z"
   ```

8. Wait until CI on the release commit passes:

   ```bash
   node scripts/verify-release-ci.mjs --sha <release commit SHA> --dispatch-missing
   ```

   The script waits for each push run of the commit. `Fuzz Smoke` has a
   path filter, so a commit can have no run of it. `--dispatch-missing`
   then starts it one time with `gh workflow run`, while `main` is the
   release commit. The `plan` job runs the same check without
   `--dispatch-missing`. `REQUIRED_WORKFLOWS` in the script lists the
   workflows. Its test fails when a workflow that runs on a push to `main`
   is not in the list.

## Write the release notes

Write the notes in a file outside the repository, for example
`~/.cache/cargo-fallow-release-vX.Y.Z.md`. Write them from the CHANGELOG
section. The notes must:

- tell the user what changed, not repeat the commit subjects;
- contain no em dash;
- end with the compare URL to the previous tag,
  `https://github.com/fallow-rs/cargo-fallow/compare/vP.Q.R...vX.Y.Z`. The URL
  of a first release is
  `https://github.com/fallow-rs/cargo-fallow/commits/vX.Y.Z`.

The title is `vX.Y.Z: <summary>`. The published release is immutable, so
check the notes before the last step.

## Run the release

1. Run a dry run on `main`:

   ```bash
   gh workflow run release.yml --ref main
   ```

   Make sure that each job passes. A dry run on the release commit shows no
   warning.

2. Start the release:

   ```bash
   gh workflow run release.yml --ref main -f version=X.Y.Z -f dry-run=false
   ```

3. Read back the run and make sure that its `headSha` is the release commit.
   Each time the run asks for approval, approve the `release` environment.
   GitHub asks one time for each of the jobs `attest`, `sign` and
   `publish-crate`.
4. Wait until `Release ready for signed tag` passes.

## Create the tag and the GitHub release

Run the last step right after `release-ready` passes, from a clean checkout
with your signing key:

```bash
node scripts/publish-github-release.mjs --run <run ID> --version X.Y.Z \
  --title "vX.Y.Z: <summary>" --notes ~/.cache/cargo-fallow-release-vX.Y.Z.md
```

Add `--check-only` first to run each check and change nothing. The script:

1. checks the title and the notes file;
2. checks that the run is a successful `Release` run on `main` with a passed
   `Release ready for signed tag` job;
3. checks that the `origin` remote is `fallow-rs/cargo-fallow`, that
   `origin/main` is the commit of the run, that its subject is
   `chore: release vX.Y.Z`, and that the tag is absent on the remote and in
   the clone;
4. downloads `release-assets` from the run. It checks that the set is
   complete, that each archive matches `SHA256SUMS`, and that each signature
   is valid;
5. creates the signed tag with `git tag -s` on the release commit, checks it
   with `git verify-tag`, and pushes it;
6. runs `gh release create --verify-tag` with the title, the notes file and
   the assets.

If the workflow fails, repair it and run it again. No tag is used yet. If the
release creation fails after the tag push, keep the signed tag. Remove only an
incomplete draft, then run `gh release create` again with the same values.
Never move or create the signed tag again.

## Change the release environment after this procedure merges

The `release` environment admitted only the tag rule `v*` for the 0.1.0
release. A release now runs on `main` and never on a tag. Make this change one
time, after the pull request that adds this procedure merges:

1. Open **Settings**, **Environments**, `release`.
2. Under **Deployment branches and tags**, remove the tag rule `v*`.
3. Add the branch rule `main`. Keep **Required reviewers** with yourself.

Until you make this change, a release run on `main` cannot start the jobs in
the `release` environment. The crates.io trusted publisher stays the same: it
names `release.yml` and the environment `release`.

## The package

`include` in `Cargo.toml` limits the package to the source under `src/`, the
manifest, the lock file, `LICENSE`, `README.md` and `CHANGELOG.md`. Test
fixtures, the fuzz crate, corpus data, the changelog fragments and repository
tools are not in the package. `scripts/check-package.mjs` reads the output of
`cargo package --list`. The script fails when it finds a file outside that
list, a missing required file, or a file over 128 KiB.

The `Package` CI job runs on each pull request:

```bash
cargo package --list --locked | node scripts/check-package.mjs
cargo package --locked --features precise
sed -i '/^publish = false$/d' Cargo.toml
cargo publish --dry-run --locked --allow-dirty
git checkout -- Cargo.toml
```

`cargo publish --dry-run` fails for a package with `publish = false`. The CI
job and the dry run of the release workflow remove the line only in their own
checkout, and put it back after the dry run. The `sed` step stays as a guard.
A release does not edit the manifest. If the line comes back in `Cargo.toml`,
the `plan` job of a release fails.

## Repository settings

These settings exist since the 0.1.0 release:

- The `release` environment has the owner as the required reviewer and the
  secret `ED25519_BINARY_SIGNING_PRIVATE_KEY`, the 32-byte Ed25519 seed in
  base64.
- A tag ruleset for `v*` lets only the owner create, update or delete a
  release tag.
- Immutable releases are on.
- On crates.io, the trusted publisher of `cargo-fallow` is the repository
  `fallow-rs/cargo-fallow`, the workflow `release.yml` and the environment
  `release`. Do not rename the workflow file or the environment.
