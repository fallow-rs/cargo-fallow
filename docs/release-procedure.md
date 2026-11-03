# Release procedure

This document tells the owner how to release `cargo-fallow`. The first part is
a list of one-time steps before the first release, 0.1.0. The second part is
the procedure for each release.

## What the release workflow does

`.github/workflows/release.yml` has two modes.

- **Dry run.** A manual run (`workflow_dispatch`) is a dry run by default. It
  checks the package file list, runs `cargo publish --dry-run`, builds the
  binaries and writes their checksums. It publishes nothing.
- **Release.** A push of a `v*` tag is a release. A manual run on a tag ref
  with `dry-run` set to false is also a release. Use it only to finish a
  release that stopped. A run on a branch is always a dry run.

A release does these steps in this order:

1. The `plan` job checks that the tag is `v` plus the package version, that
   `Cargo.toml` has no `publish = false` line, and that `main` holds the tag
   commit. It writes the release notes from the CHANGELOG section of the
   version, checks the package file list, and runs `cargo publish --dry-run`.
2. The `build` job builds `cargo-fallow` for five targets:
   `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`,
   `x86_64-apple-darwin`, `aarch64-apple-darwin` and `x86_64-pc-windows-msvc`.
   Each archive has a `.sha256` file.
3. The `attest` job writes a build provenance attestation for each archive.
4. The `publish-crate` job publishes the crate to crates.io with trusted
   publishing. It gets a short-lived token through OIDC. The repository has no
   crates.io token.
5. The `github-release` job creates the GitHub release with the notes, the
   archives, the `.sha256` files and one `SHA256SUMS` file.

Jobs 3, 4 and 5 run only for a release, in the `release` environment.
`.github/scripts/release-workflow.test.mjs` checks the triggers, the
permissions, the environment of each job and the action pins.

## The package

`include` in `Cargo.toml` limits the package to the source under `src/`, the
manifest, the lock file, `LICENSE`, `README.md` and `CHANGELOG.md`. Test
fixtures, the fuzz crate, corpus data and repository tools stay out.
`scripts/check-package.mjs` reads the output of `cargo package --list` and
fails on a file outside that list, on a missing required file, and on a file
over 128 KiB.

The `Package` CI job runs on each pull request:

```bash
cargo package --list --locked | node scripts/check-package.mjs
cargo package --locked --features precise
sed -i '/^publish = false$/d' Cargo.toml
cargo publish --dry-run --locked --allow-dirty
git checkout -- Cargo.toml
```

`cargo publish --dry-run` refuses a package with `publish = false`. The CI job
and the dry run of the release workflow remove the line in their own checkout
only and restore it after the dry run. `--allow-dirty` accepts that one
change. The repository keeps the line until the 0.1.0 release commit. A
release never edits the manifest: the `plan` job fails while the line is
there.

To run the same check on your machine, use a copy of the repository, not your
work tree:

```bash
git worktree add ../cargo-fallow-dry-run HEAD
cd ../cargo-fallow-dry-run
sed -i.bak '/^publish = false$/d' Cargo.toml && rm Cargo.toml.bak
cargo publish --dry-run --locked --allow-dirty
cd - && git worktree remove --force ../cargo-fallow-dry-run
```

## One-time steps before 0.1.0

Do these steps once, in this order. Steps 1 to 3 change nothing in public.

### 1. Run a dry run

```bash
gh workflow run release.yml --ref main
```

The `dry-run` input is true by default. Make sure that each job passes and
that the summary of the `plan` job shows the notes that you expect.

### 2. Create the `release` environment

In the repository settings, open **Environments** and create `release`:

- **Deployment branches and tags**: select **Selected branches and tags** and
  add the tag rule `v*`. Add no branch rule. A run on a branch then gets no
  OIDC token for this environment.
- **Required reviewers**: add yourself. Each release then waits for your
  approval before the credential jobs start. GitHub asks once for each of the
  three jobs `attest`, `publish-crate` and `github-release`.
- Add no secrets. Trusted publishing needs none.

Also add a tag ruleset for `v*` that lets only the owner create, update or
delete a release tag.

### 3. Prepare the 0.1.0 release commit

On a branch, make one commit with the subject `chore: release v0.1.0`:

1. In `Cargo.toml`, set `version = "0.1.0"` and remove the `publish = false`
   line and the comment above it.
2. Run `cargo check --locked` again without `--locked` if Cargo asks for it,
   so that `Cargo.lock` has version 0.1.0.
3. In `CHANGELOG.md`, rename `## [Unreleased]` to `## [0.1.0] - YYYY-MM-DD`
   and add a new empty `## [Unreleased]` section above it.
4. In `README.md`, replace the install section with `cargo install
   cargo-fallow --locked` and a link to the binaries of the GitHub release.
5. Remove the M4 note in the `msrv` job of `.github/workflows/ci.yml`.
6. If the user documentation for Rust is public, set `documentation` in
   `Cargo.toml` to its URL.

Merge the pull request when CI passes.

### 4. Make the repository public

Before you change the visibility, make sure that the history has no secret
and no private text. Then open **Settings**, **General**, **Danger Zone**, and
select **Change visibility**. The attestations of step 3 of the workflow need
a public repository.

### 5. Publish 0.1.0 to crates.io by hand

The crates.io page on trusted publishing (checked on 2026-11-03) gives this
prerequisite: the crate must already be on crates.io, and the first publish
needs an API token. So publish 0.1.0 by hand. If crates.io later accepts a
trusted publisher for a new crate name, do step 6 first and skip this step.

1. On crates.io, create an API token with the scope `publish-new` for the
   crate `cargo-fallow` and a short expiry.
2. In a clean checkout of the release commit on `main`:

   ```bash
   cargo publish --locked
   ```

3. Delete the token on crates.io.

### 6. Register the trusted publisher

On crates.io, open the settings of `cargo-fallow`, then **Trusted
Publishing**, and add a GitHub publisher:

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

Approve the `release` environment each time the workflow asks. Version 0.1.0 is
already on crates.io after step 5, so `publish-crate` reports it as published
and continues. The workflow then attests the archives and creates the GitHub
release.

## Each later release

1. Make a commit `chore: release vX.Y.Z` that sets the version in
   `Cargo.toml` and `Cargo.lock` and moves the `[Unreleased]` entries of the
   CHANGELOG to a `## [X.Y.Z] - YYYY-MM-DD` section. Merge it to `main`.
2. Run a dry run on `main` (step 1 above).
3. Push a signed tag `vX.Y.Z` on the merged commit and approve the `release`
   environment.

If a release stops after the crate is on crates.io, run the workflow again on
the tag with `dry-run` set to false:

```bash
gh workflow run release.yml --ref vX.Y.Z -f dry-run=false
```

`publish-crate` skips a version that is already published, so only the
missing steps do work.
