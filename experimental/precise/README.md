# cargo-fallow-precise (experiment)

This crate is the `--precise` prototype of the M2 gap measurement. It is not
part of the cargo-fallow build, and it is not published.

The fast check of `cargo fallow visibility` lists in `undecided` the findings
that only an untyped credit stops: a method call, a field access, or a name in
macro tokens or in an attribute. This program loads the workspace into
rust-analyzer (`ra_ap_*` 0.0.358), with build scripts and proc-macros, and
finds the uses of each undecided item with name resolution and type
inference. It does not check the other findings.

## Build and run

```bash
cd experimental/precise
cargo build --release
cargo fallow visibility --manifest-path ../../Cargo.toml --format json --quiet > fast.json
RUSTUP_TOOLCHAIN=1.99.0 ./target/release/cargo-fallow-precise ../../Cargo.toml fast.json
```

The proc-macro server comes from the sysroot of the selected toolchain, so the
toolchain must have the `rust-analyzer-proc-macro-srv` binary.

`Cargo.lock` pins `unicode-ident` 1.0.24 and `unicode-properties` 0.1.4. A
fresh resolve of the `ra_ap_*` crates does not build without these pins.

## Limits

- rust-analyzer does not model doctests. A credit from a doc code block
  stays, so such a finding is not in `undecided`.
- The result for an undecided item comes from its direct uses, not from
  reachability from the production roots.
