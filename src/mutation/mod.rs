//! Mutation testing for one package on stable Rust.
//!
//! A mutant is a small change in the source. The engine runs the tests of the
//! package against each mutant and reports the mutants that no test detects.
//!
//! The engine has two tiers:
//!
//! - The schema tier puts all operators that are type-safe by construction into
//!   one build, and a runtime switch selects one mutant in each test process.
//! - The rebuild tier runs each other operator in its own rebuild of one reused
//!   scratch copy, in the style of cargo-mutants.
//!
//! The parts are:
//!
//! - [`sites`] and [`rewrites`] find the mutants in one file, and [`id`] gives
//!   each mutant its stable id.
//! - [`schema`] writes one file with a runtime switch at each site.
//! - [`files`] walks the module tree and copies a source tree.
//! - [`removal`] compiles, removes the mutants that break the build, and repeats.
//! - [`testrun`] runs nextest and the doctests and reads the results.
//! - [`guard`], [`probe`] and [`process`] keep the machine safe: a mutant runs
//!   only under a time limit and a memory limit.
//! - [`status`] holds the status set and the retry rules.
//! - [`plan`] lists the mutants, [`engine`] runs the stages, [`workdir`] holds the
//!   copies, [`run`] is the entry point, and [`report`] holds the output.

mod engine;
mod files;
pub mod guard;
mod id;
mod plan;
mod probe;
mod process;
mod removal;
pub mod report;
mod rewrites;
pub mod run;
mod schema;
mod sites;
mod status;
mod testrun;
mod workdir;
