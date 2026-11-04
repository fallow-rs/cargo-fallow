# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in cargo-fallow, please report it responsibly via [GitHub's private vulnerability reporting](https://github.com/fallow-rs/cargo-fallow/security/advisories/new) instead of opening a public issue. If you cannot use GitHub, send the report by email to [security@fallow.tools](mailto:security@fallow.tools).

You should receive a response within 48 hours. Please include:

- A description of the vulnerability
- Steps to reproduce it
- Any relevant version or configuration information

## Supported versions

Only the latest minor release of cargo-fallow receives security fixes. A security fix goes to the `main` branch first.

## Scope

The static checks (`check`, `deps`, `visibility`, `boundaries`, `size`, `complexity`, `dupes` and `files`) run `cargo metadata --no-deps` and parse the source with `ra_ap_syntax`. They do not compile the workspace, they do not run build scripts or procedural macros, and they do not use the network. `--changed-since` and `--hotspots` also run `git` on the workspace.

Some commands and flags write files. `fix --yes` edits the source and the manifests. `init` writes the configuration. `--save-baseline` and `--save-snapshot` write the file that you name.

`cargo fallow mutants` is different. It copies the workspace to a temporary folder, then builds it and runs its tests in that copy. That runs the build scripts, the procedural macros and the tests of the workspace, and Cargo can use the network to fetch dependencies. Run `mutants` only on code that you would build and test yourself.

`cargo metadata` reads the Cargo configuration of the workspace, for example `.cargo/config.toml`. Run cargo-fallow only on a workspace whose Cargo configuration you trust, the same as for `cargo build`.

The crate root uses `#![cfg_attr(not(test), deny(clippy::disallowed_methods))]` to prohibit `std::process::Command::new`. Only the workspace loader (`src/workspace.rs`), the git diff reader (`src/diff.rs`) and the mutation process runner (`src/mutation/process.rs`) are allowed to spawn a process.

## Release binaries

Each release archive has an Ed25519 signature (`<archive>.sig`) next to its
SHA-256 file (`<archive>.sha256`). The release workflow signs each archive
with the `ED25519_BINARY_SIGNING_PRIVATE_KEY` secret of the `release`
environment. The key pair belongs to cargo-fallow only.
The fallow binaries use a different key pair.

The GitHub Action installer (`action/scripts/install.sh`) checks the
signature with the public key in `action/scripts/verify-signature.mjs`, then
the SHA-256, then the version of the unpacked binary. It fails closed: a bad
or missing signature, a checksum mismatch or another version stops the job,
and the installer does not fall back to a source build. To check an archive by
hand, run `node action/scripts/verify-signature.mjs <archive> <archive>.sig`.

Each archive also has a build provenance attestation. Run
`gh attestation verify <archive> --repo fallow-rs/cargo-fallow` to check it.

## Threat model

The analyzed workspace controls the source files that cargo-fallow parses. A source file must not crash the run:

- syn parses with recursion and has no depth limit. `parse::parse_source` runs each parse on a thread with a stack that is sized from the source length, so deep nesting cannot overflow the stack. A file that needs more stack than the cap counts for the `parse-error` gate with the `nesting-too-deep` reason.
- A panic in the parser gives a failed parse of that file, not a crash.
- `.clippy.toml` forbids a direct call of the syn parse functions outside `parse::parse_source`.
- The `fuzz_parse` target in `fuzz/` sends arbitrary input through the bounded parse. The Fuzz Smoke workflow runs it on `main` and every week.

## Agent-instruction surface

AI coding agents read instruction files (`CLAUDE.md`, `AGENTS.md`, `.agents/**`) as trusted context. A dependency install hook, or a pasted "fix", can plant hidden instructions in one of these files for the next agent session to execute.

- `scripts/scan-hidden-unicode.py` runs in the pre-commit hook and in CI. It fails on zero-width and bidirectional-override code points in tracked text files.
- `.github/CODEOWNERS` requires a review from the maintainer for a change to an agent-instruction file or to a workflow.

This is defense-in-depth, not a trust boundary. Agent-instruction files are untrusted by default. Never run a pasted remediation without reading the patch, the URLs, and the package names first.
