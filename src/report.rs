//! The versions that each report writes.

/// The version of the report shape of `mutants`. A change that removes a
/// member or changes its type increments it.
pub const SCHEMA_VERSION: u32 = 1;

/// The package version, written to the report.
pub const TOOL_VERSION: &str = env!("CARGO_PKG_VERSION");
