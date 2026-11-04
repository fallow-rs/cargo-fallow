# Release procedure

This document describes how the owner releases `cargo-fallow`. First, it
lists the one-time steps before the first release, 0.1.0. Then it gives the
procedure for each later release.

## What the release workflow does

`.github/workflows/release.yml` has two modes.

- **Dry run.** A manual run (`workflow_dispatch`) is a dry run by default. It
  checks the package file list, runs `cargo publish --dry-run`, builds the
  binaries and writes their checksums. A dry run does not publish anything.
- **Release.** A push of a `v*` tag starts a release. A manual run on a tag
  ref with `dry-run` set to false is also a release. Use a manual release only
  to finish a release that stopped. A run on a branch is always a dry run.

A release runs these jobs in this order:

1. The `plan` job checks that the tag is `v` plus the package version. It
   also checks that `Cargo.toml` has no `publish = false` line, and that `main`
   contains the tag commit. Then the job writes the release notes from the
   CHANGELOG section of the version, checks the package file list, and runs
   `cargo publish --dry-run`.
2. The `build` job builds `cargo-fallow` for five targets:
   `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`,
   `x86_64-apple-darwin`, `aarch64-apple-darwin` and `x86_64-pc-windows-msvc`.
   The job writes a `.sha256` file for each archive.
3. The `attest` job writes a build provenance attestation for each archive.
4. The `sign` job uses the `ED25519_BINARY_SIGNING_PRIVATE_KEY` secret to
   write an Ed25519 signature `<archive>.sig` for each archive. Then the job
   verifies each signature with the public key in
   `action/scripts/verify-signature.mjs`. This key pair is for cargo-fallow
   only. The fallow repo uses a different key pair.
5. The `publish-crate` job publishes the crate to crates.io with trusted
   publishing. The job gets a short-lived token through OIDC. The repository
   does not store a crates.io token.
6. The `github-release` job creates the GitHub release with the notes, the
   archives, the `.sha256` files, the `.sig` files and one `SHA256SUMS` file.

Jobs 3 to 6 run only for a release, in the `release` environment.
`.github/scripts/release-workflow.test.mjs` checks the triggers, the
permissions, the environment of each job and the action pins.

## The package

`include` in `Cargo.toml` limits the package to the source under `src/`, the
manifest, the lock file, `LICENSE`, `README.md` and `CHANGELOG.md`. Test
fixtures, the fuzz crate, corpus data and repository tools are not in the
package. `scripts/check-package.mjs` reads the output of
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
checkout, and put it back after the dry run. With `--allow-dirty`, Cargo
accepts that one change. The line stays in the repository until the 0.1.0
release commit. A release does not edit the manifest. While the line is in
`Cargo.toml`, the `plan` job fails.

To run the same check on your machine, use a separate git worktree, not your
own work tree:

```bash
git worktree add ../cargo-fallow-dry-run HEAD
cd ../cargo-fallow-dry-run
sed -i.bak '/^publish = false$/d' Cargo.toml && rm Cargo.toml.bak
cargo publish --dry-run --locked --allow-dirty
cd - && git worktree remove --force ../cargo-fallow-dry-run
```

## One-time steps before 0.1.0

Do these steps one time, in this order. Steps 1 to 3 make no public change.

### 1. Run a dry run

```bash
gh workflow run release.yml --ref main
```

The `dry-run` input is true by default. Make sure that each job passes, and
that the summary of the `plan` job shows the release notes that you expect.

### 2. Create the `release` environment

In the repository settings, open **Environments** and create `release`:

- **Deployment branches and tags**: select **Selected branches and tags** and
  add the tag rule `v*`. Do not add a branch rule. Then a run on a branch
  does not get an OIDC token for this environment.
- **Required reviewers**: add yourself. Then each release waits for your
  approval before the jobs that use credentials start. GitHub asks one time
  for each of the four jobs `attest`, `sign`, `publish-crate` and
  `github-release`.
- Add one secret, `ED25519_BINARY_SIGNING_PRIVATE_KEY`, with the private key
  of the cargo-fallow pair. The value is the 32-byte Ed25519 seed in base64.
  Do not use the key of the `fallow-rs/fallow` repository. Without this
  secret, the `sign` job fails. Trusted publishing does not need a crates.io
  secret.

Also add a tag ruleset for `v*` that lets only the owner create, update or
delete a release tag.

### 3. Prepare the 0.1.0 release commit

On a branch, make one commit with the subject `chore: release v0.1.0`:

1. In `Cargo.toml`, set `version = "0.1.0"` and remove the `publish = false`
   line and the comment above it.
2. Run `cargo check --locked`. If it fails because `Cargo.lock` needs an
   update, run `cargo check` without `--locked`. `Cargo.lock` then has
   version 0.1.0.
3. In `CHANGELOG.md`, rename `## [Unreleased]` to `## [0.1.0] - YYYY-MM-DD`
   and add a new empty `## [Unreleased]` section above it.
4. In `README.md`, replace the install section with `cargo install
   cargo-fallow --locked` and a link to the binaries of the GitHub release.
5. Remove the M4 note in the `msrv` job of `.github/workflows/ci.yml`.
6. If the user documentation for Rust is public, set `documentation` in
   `Cargo.toml` to its URL.

When CI passes, merge the pull request.

### 4. Make the repository public

Before you change the visibility, make sure that the history has no secrets
and no private text. Then open **Settings**, **General**, **Danger Zone**, and
select **Change visibility**. The `attest` job (step 3 of the release
workflow) needs a public repository.

### 5. Publish 0.1.0 to crates.io by hand

The crates.io page on trusted publishing (checked on 2026-11-03) states that
the crate must already be on crates.io. The first publish needs an API token,
so publish 0.1.0 by hand. If crates.io later accepts a trusted publisher for a
new crate name, do step 6 first and skip this step.

1. On crates.io, create an API token with the scope `publish-new` for the
   crate `cargo-fallow` and a short expiry.
2. In a clean checkout of the release commit on `main`, run this command:

   ```bash
   cargo publish --locked
   ```

3. Delete the token on crates.io.

### 6. Register the trusted publisher

On crates.io, open the settings of `cargo-fallow`, then **Trusted
Publishing**, and add a GitHub publisher with these values:

| Field | Value |
| --- | --- |
| Repository owner | `fallow-rs` |
| Repository name | `cargo-fallow` |
| Workflow filename | `release.yml` |
| Environment | `release` |

### 7. Push the tag

```bash
git switch main && git pull --ff-only
git tag -s v0.1.0 -m "cargo-fallow v0.1.0"
git push origin v0.1.0
```

Each time the workflow asks for approval, approve the `release` environment.
After step 5, version 0.1.0 is already on crates.io. The `publish-crate` job
reports that the version is published, and the workflow continues. The
workflow also attests the archives and creates the GitHub release.

## Each later release

1. Make a commit `chore: release vX.Y.Z`. In this commit, set the version in
   `Cargo.toml` and `Cargo.lock`. Also move the `[Unreleased]` entries of the
   CHANGELOG to a `## [X.Y.Z] - YYYY-MM-DD` section. Merge the commit to
   `main`.
2. Run a dry run on `main` (step 1 above).
3. Push a signed tag `vX.Y.Z` on the merged commit. Then approve the
   `release` environment.

If a release stops after the crate is on crates.io, run the workflow again on
the tag with `dry-run` set to false:

```bash
gh workflow run release.yml --ref vX.Y.Z -f dry-run=false
```

The `publish-crate` job skips a version that is already published, so the
new run does only the steps that are missing.
