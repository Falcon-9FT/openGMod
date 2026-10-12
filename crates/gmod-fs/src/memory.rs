//! Files held in memory: what a server sent its client (its client Lua), mounted for one
//! game and gone with it.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::index;
use crate::path::normalize;

#[derive(Debug, Clone)]
struct Entry {
    path: String,
    data: Arc<[u8]>,
}

/// Files by normalized path.
#[derive(Debug, Clone)]
pub struct MemoryMount {
    /// What to call it where a folder or an archive would show its path.
    name: PathBuf,
    /// Sorted by path.
    entries: Vec<Entry>,
}

impl MemoryMount {
    /// The files given, by game path; a path that does not normalize (`../x`) is left out,
    /// and of one given twice the last stays.
    pub fn new(name: impl Into<PathBuf>, files: impl IntoIterator<Item = (String, Vec<u8>)>) -> MemoryMount {
        let mut entries: Vec<Entry> =
            files.into_iter().filter_map(|(path, data)| Some(Entry { path: normalize(&path)?, data: data.into() })).collect();
        // stable: of equal paths the last given ends last, and is the one kept
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        entries.reverse();
        entries.dedup_by(|later, earlier| later.path == earlier.path);
        entries.reverse();
        MemoryMount { name: name.into(), entries }
    }

    pub fn name(&self) -> &Path {
        &self.name
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `Some(is a folder)` if something is at the normalized `path`.
    pub fn contains(&self, path: &str) -> Option<bool> {
        if index::find(&self.entries, |e| &e.path, path).is_some() {
            Some(false)
        } else {
            index::has_dir(&self.entries, |e| &e.path, path).then_some(true)
        }
    }

    /// The names directly inside the normalized folder `dir`, each with whether it is a folder.
    pub fn list(&self, dir: &str) -> Vec<(String, bool)> {
        index::children(&self.entries, |e| &e.path, dir)
    }

    /// Reads the file at a normalized path.
    pub fn read(&self, path: &str) -> io::Result<Vec<u8>> {
        index::find(&self.entries, |e| &e.path, path)
            .map(|e| e.data.to_vec())
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{path} is not in {}", self.name.display())))
    }

    /// Every file, as normalized paths, sorted.
    pub fn paths(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.path.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_in_memory_are_found_as_in_a_folder() {
        let files =
            [("lua/autorun/client/a.lua", "a"), ("LUA\\vgui\\b.lua", "b"), ("lua/autorun/client/a.lua", "again"), ("../outside.lua", "no")];
        let mount = MemoryMount::new("server", files.iter().map(|(p, d)| (p.to_string(), d.as_bytes().to_vec())));
        assert_eq!(mount.paths(), ["lua/autorun/client/a.lua", "lua/vgui/b.lua"]);
        assert_eq!(mount.read("lua/autorun/client/a.lua").unwrap(), b"again");
        assert_eq!(mount.contains("lua/autorun"), Some(true));
        assert_eq!(mount.contains("lua/vgui/b.lua"), Some(false));
        assert_eq!(mount.contains("lua/nothing"), None);
        assert_eq!(mount.list("lua"), [("autorun".to_string(), true), ("vgui".to_string(), true)]);
        assert!(mount.read("outside.lua").is_err());
    }
}
