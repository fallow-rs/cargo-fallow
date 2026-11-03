//! The two modes of `mutants` that compile nothing and run no test: `--list` and
//! `--inspect <id> --no-build`. Both read the coverage map only from the cache.

use crate::error::CliError;
use crate::mutation::coverage::CoverageMap;
use crate::mutation::engine::{Engine, Stages, selection_info, sort_rows};
use crate::mutation::plan::{rebuild_mutants, schema_mutants};
use crate::mutation::report::{Baseline, MapSource, MutantRow, RebuildInfo, SchemaInfo};
use crate::mutation::selection::Selector;

impl Engine<'_> {
    /// The map of this source from the cache, or `None`. The key of the map comes
    /// from the scratch copy, so the caller copies the source tree first. This reads
    /// files and asks `rustc` for its version, and it compiles nothing.
    pub fn cached_map(&self) -> Result<Option<CoverageMap>, CliError> {
        if !self.options.cache || !self.options.selection {
            return Ok(None);
        }
        let package = self.dirs.scratch.join(&self.package_dir);
        Ok(self
            .open_cache(&package)?
            .and_then(|cached| cached.cache.read_map(cached.key)))
    }

    /// Each mutant of this run with the status `pending`, in the order of a report.
    /// A rebuild mutant is a candidate: a run can find that no build reads it. A
    /// map in the cache fills only the selection part.
    pub fn list(&self) -> Result<(Vec<MutantRow>, Stages), CliError> {
        let (rewrites, rewrites_outside_diff) = self.find_rewrites();
        let schema = if self.options.tier.has_schema() {
            schema_mutants(self.files)
        } else {
            Vec::new()
        };
        let rebuild = rebuild_mutants(self.files, &rewrites);
        let mut rows: Vec<MutantRow> = schema
            .iter()
            .chain(&rebuild)
            .map(|planned| planned.row.clone())
            .collect();
        sort_rows(&mut rows);
        let selector = self.cached_map()?.map(Selector::new);
        let source = if selector.is_some() {
            MapSource::Cached
        } else {
            MapSource::None
        };
        let selection = selection_info(source, 0, selector.as_ref(), 0, &rows);
        let stages = Stages {
            baseline: Baseline::default(),
            schema: SchemaInfo {
                candidates: schema.len(),
                ..SchemaInfo::default()
            },
            rebuild: RebuildInfo {
                candidates: rebuild.len(),
                unknown_return_types: rewrites.iter().map(|file| file.unknown_return_types).sum(),
                probe_verified: false,
            },
            stopped_by: None,
            doctests: false,
            selection,
            rewrites_outside_diff,
            workspace: None,
        };
        Ok((rows, stages))
    }
}
