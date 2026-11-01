//! The cache folder of one package: the coverage map and the journal.
//!
//! The folder is in the target folder of the workspace, so `cargo clean` removes
//! it and no run writes into the source tree. A failed write only costs time in
//! the next run, so it gives a warning and the run continues.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use xxhash_rust::xxh3::{Xxh3, xxh3_64};

use crate::mutation::coverage::{CoverageMap, MAP_FORMAT};
use crate::render;

/// The prefix of a map file. The folder keeps one map for each package.
const MAP_PREFIX: &str = "coverage-";

/// The hash of each file in a source tree, in a sorted walk. The input has the
/// relative path and the content of each file, and the target of each link.
pub fn tree_hash(root: &Path) -> io::Result<u64> {
    let mut hasher = Xxh3::new();
    hash_entries(root, Path::new(""), &mut hasher)?;
    Ok(hasher.digest())
}

fn hash_entries(root: &Path, relative: &Path, hasher: &mut Xxh3) -> io::Result<()> {
    let mut entries = fs::read_dir(root.join(relative))?.collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = relative.join(entry.file_name());
        add_part(hasher, path.to_string_lossy().as_bytes());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            hash_entries(root, &path, hasher)?;
        } else if kind.is_symlink() {
            let target = fs::read_link(entry.path())?;
            add_part(hasher, target.to_string_lossy().as_bytes());
        } else {
            add_part(hasher, &fs::read(entry.path())?);
        }
    }
    Ok(())
}

/// The length prefix keeps `ab` + `c` different from `a` + `bc`.
fn add_part(hasher: &mut Xxh3, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

/// The hash of some text parts.
pub fn parts_hash(parts: &[&str]) -> u64 {
    let mut bytes = Vec::new();
    for part in parts {
        bytes.extend_from_slice(&(part.len() as u64).to_le_bytes());
        bytes.extend_from_slice(part.as_bytes());
    }
    xxh3_64(&bytes)
}

/// The cache folder of one package.
#[derive(Debug, Clone)]
pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    /// The cache folder of `package` in a target folder. The name of a package is
    /// a safe folder name, because Cargo allows only a small character set.
    pub fn new(target_dir: &Path, package: &str) -> Self {
        Self {
            dir: target_dir.join("fallow-mutation").join(package),
        }
    }

    fn map_path(&self, key: u64) -> PathBuf {
        self.dir.join(format!("{MAP_PREFIX}{key:016x}.json"))
    }

    /// The journal of one run setup.
    pub fn journal_path(&self, key: u64) -> PathBuf {
        self.dir.join(format!("journal-{key:016x}.jsonl"))
    }

    /// Reads the map of `key`. A file with another layout or a torn file counts
    /// as no file.
    pub fn read_map(&self, key: u64) -> Option<CoverageMap> {
        let text = fs::read_to_string(self.map_path(key)).ok()?;
        let map: CoverageMap = serde_json::from_str(&text).ok()?;
        (map.format == MAP_FORMAT).then_some(map)
    }

    /// Writes the map of `key`, and removes the maps of older sources.
    pub fn write_map(&self, key: u64, map: &CoverageMap) {
        if let Err(err) = self.try_write_map(key, map) {
            render::warn(&format!("cannot write the coverage map: {err}"));
        }
    }

    fn try_write_map(&self, key: u64, map: &CoverageMap) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let path = self.map_path(key);
        // A rename means that a crash never leaves half a map.
        let partial = path.with_extension("partial");
        fs::write(&partial, serde_json::to_vec(map)?)?;
        fs::rename(&partial, &path)?;
        for entry in fs::read_dir(&self.dir)? {
            let old = entry?.path();
            let is_map = old
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(MAP_PREFIX));
            if is_map && old != path {
                fs::remove_file(old)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder() -> tempfile::TempDir {
        let Ok(dir) = tempfile::tempdir() else {
            panic!("a temporary directory");
        };
        dir
    }

    fn hash(dir: &Path) -> u64 {
        let Ok(value) = tree_hash(dir) else {
            panic!("the folder can be read");
        };
        value
    }

    fn write(path: &Path, text: &str) {
        let Ok(()) = fs::write(path, text) else {
            panic!("write a file");
        };
    }

    #[test]
    fn the_hash_changes_with_content_and_with_names() {
        let dir = folder();
        write(&dir.path().join("a"), "one");
        let first = hash(dir.path());
        assert_eq!(hash(dir.path()), first);
        write(&dir.path().join("a"), "two");
        let changed = hash(dir.path());
        assert_ne!(changed, first);
        let Ok(()) = fs::rename(dir.path().join("a"), dir.path().join("b")) else {
            panic!("rename a file");
        };
        assert_ne!(hash(dir.path()), changed);
    }

    #[test]
    fn parts_keep_their_borders() {
        assert_ne!(parts_hash(&["ab", "c"]), parts_hash(&["a", "bc"]));
    }

    #[test]
    fn a_map_is_read_back_and_replaces_an_older_map() {
        let dir = folder();
        let cache = Cache::new(dir.path(), "p");
        let map = CoverageMap {
            format: MAP_FORMAT,
            ..CoverageMap::default()
        };
        assert_eq!(cache.read_map(1), None);
        cache.write_map(1, &map);
        assert_eq!(cache.read_map(1), Some(map.clone()));
        cache.write_map(2, &map);
        assert_eq!(cache.read_map(1), None);
        assert_eq!(cache.read_map(2), Some(map));
    }

    #[test]
    fn a_map_with_another_layout_or_a_torn_file_counts_as_no_map() {
        let dir = folder();
        let cache = Cache::new(dir.path(), "p");
        let map = CoverageMap {
            format: MAP_FORMAT + 1,
            ..CoverageMap::default()
        };
        cache.write_map(1, &map);
        assert_eq!(cache.read_map(1), None);
        write(&cache.map_path(1), "{\"format\":");
        assert_eq!(cache.read_map(1), None);
    }
}
