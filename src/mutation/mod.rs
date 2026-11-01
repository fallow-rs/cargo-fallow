//! Mutant schemata at source level, without type data (the M1 spike).
//!
//! The spike answers one question: can all mutants of a crate go into one
//! build on stable Rust when the tool reads only syntax? The parts are:
//!
//! - [`sites`] finds the mutation sites in one file.
//! - [`schema`] writes one file with a runtime switch at each site.
//! - [`files`] walks the module tree and copies a source tree.
//! - [`compile`] compiles, removes the mutants that break the build, and repeats.
//! - [`baseline`] runs the tests with no active mutant and compares two runs.
//! - [`spike`] connects the parts and builds the report.
//!
//! No part runs a mutant.

mod baseline;
mod compile;
mod files;
mod process;
mod schema;
mod sites;
pub mod spike;
