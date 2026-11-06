//! The `cargo-fallow` binary. The library crate holds the logic.

use std::process::ExitCode;

/// The allocator of musl is slow when several threads allocate at the same
/// time, and the checks run in parallel threads (fallow-rs/cargo-fallow#147).
#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> ExitCode {
    cargo_fallow::run(std::env::args_os().collect())
}
