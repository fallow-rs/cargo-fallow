//! The probe builds and the generated files of the schema build.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;

use crate::mutation::files::CrateFile;
use crate::mutation::removal::{Build, probed_mutants};
use crate::mutation::schema::Rendered;

/// The result of the probe builds.
pub struct Probed {
    /// The mutants that a build reads.
    pub read: BTreeSet<u64>,
    /// False when a build had another error, so the probe proves nothing.
    pub verified: bool,
}

/// Finds the mutants that a build reads.
///
/// cargo stops at the library when the library has an error, so a mutant in a
/// binary target needs a second pass with no probe left in the library. The loop
/// ends when a pass finds no new mutant.
pub fn read_by_build<B>(all: &BTreeSet<u64>, mut compile: B) -> io::Result<Probed>
where
    B: FnMut(&BTreeSet<u64>) -> io::Result<Build>,
{
    let mut read = BTreeSet::new();
    let mut left = all.clone();
    while !left.is_empty() {
        let build = compile(&left)?;
        let found: BTreeSet<u64> = probed_mutants(&build)
            .intersection(&left)
            .copied()
            .collect();
        if found.is_empty() {
            return Ok(Probed {
                read,
                verified: build.success,
            });
        }
        left.retain(|id| !found.contains(id));
        read.extend(found);
    }
    Ok(Probed {
        read,
        verified: true,
    })
}

/// Writes the generated files that changed since the last round.
pub struct SchemaWriter<'a> {
    files: &'a [CrateFile],
    dir: &'a Path,
    written: Vec<String>,
}

impl<'a> SchemaWriter<'a> {
    pub fn new(files: &'a [CrateFile], dir: &'a Path) -> Self {
        let written = files.iter().map(|file| file.original.clone()).collect();
        Self {
            files,
            dir,
            written,
        }
    }

    /// Returns the total size of the generated text.
    pub fn write(&mut self, rendered: &[Rendered]) -> io::Result<usize> {
        let mut total = 0;
        for (index, output) in rendered.iter().enumerate() {
            total += output.text.len();
            if self.written[index] != output.text {
                fs::write(self.dir.join(&self.files[index].path), &output.text)?;
                self.written[index].clone_from(&output.text);
            }
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mutation::removal::CompilerError;
    use crate::mutation::schema::PROBE_PREFIX;

    fn probe_error(id: u64) -> CompilerError {
        CompilerError {
            message: format!("cannot find value `{PROBE_PREFIX}{id:x}` in this scope"),
            ..CompilerError::default()
        }
    }

    fn failing(errors: Vec<CompilerError>) -> Build {
        Build {
            success: false,
            errors,
        }
    }

    #[test]
    fn a_second_pass_finds_the_mutants_after_the_library() {
        let all: BTreeSet<u64> = [1, 2, 3].into();
        let mut calls = Vec::new();
        let Ok(probed) = read_by_build(&all, |left| {
            calls.push(left.clone());
            Ok(if left.contains(&1) {
                failing(vec![probe_error(1)])
            } else if left.contains(&2) {
                failing(vec![probe_error(2)])
            } else {
                Build {
                    success: true,
                    errors: Vec::new(),
                }
            })
        }) else {
            panic!("the probe has no I/O");
        };
        assert_eq!(probed.read, [1, 2].into());
        assert!(probed.verified);
        assert_eq!(calls.len(), 3);
    }

    #[test]
    fn another_error_proves_nothing() {
        let all: BTreeSet<u64> = [1].into();
        let Ok(probed) = read_by_build(&all, |_| Ok(failing(vec![CompilerError::default()])))
        else {
            panic!("the probe has no I/O");
        };
        assert!(probed.read.is_empty());
        assert!(!probed.verified);
    }
}
