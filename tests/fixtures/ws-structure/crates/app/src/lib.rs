//! The application.

mod engine;
mod render;

/// Starts a child in a new process group.
pub fn spawn(command: &mut std::process::Command) {
    // A comment that names process_group(0) does not count.
    command.process_group(0);
}
