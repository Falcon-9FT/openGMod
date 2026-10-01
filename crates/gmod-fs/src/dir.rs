//! A folder of loose files, looked up without regard to case.
//!
//! On Windows and macOS the disk already ignores case; on Linux it does not, and Source
//! content is written with any capitalisation (`Materials/Models/...` in a map, `materials/
//! models/...` on disk). Each folder is read once into a table from lower-case name to the
//! name on disk, and a path is resolved component by component through these tables.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};

use crate::io_util::MAX_FILE;
use crate::path;

/// Folders deeper than this are not walked (a symlink loop would otherwise never end).
const MAX_DEPTH: usize = 64;

#[derive(Default)]
struct Listing {
    /// lower-case name → (name on disk, is a folder)
    entries: HashMap<String, (OsString, bool)>,
}

/// A mounted folder.
pub struct DirMount {
    root: PathBuf,
    /// Contents of the folders read so far, by normalized path (`""` = the root).
    listings: RwLock<HashMap<String, Arc<Listing>>>,
}

impl fmt::Debug for DirMount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DirMount").field("root", &self.root).finish()
    }
}

impl DirMount {
    pub fn new(root: impl Into<PathBuf>) -> DirMount {
        DirMount { root: root.into(), listings: RwLock::new(HashMap::new()) }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Forgets the folder contents read so far, after files were added or removed.
    pub fn forget(&self) {
        self.listings.write().unwrap_or_else(PoisonError::into_inner).clear();
    }

    fn listing(&self, dir: &str, real: &Path) -> Option<Arc<Listing>> {
        if let Some(listing) = self.listings.read().unwrap_or_else(PoisonError::into_inner).get(dir) {
            return Some(listing.clone());
        }
        let mut items: Vec<(OsString, bool)> = std::fs::read_dir(real)
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| {
                let is_dir = match entry.file_type() {
                    Ok(t) if t.is_symlink() => entry.path().is_dir(),
                    Ok(t) => t.is_dir(),
                    Err(_) => false,
                };
                (entry.file_name(), is_dir)
            })
            .collect();
        // names that differ only in case (possible on Linux): the same one wins every time
        items.sort();
        let mut listing = Listing::default();
        for (name, is_dir) in items {
            let lower = name.to_string_lossy().to_lowercase();
            if lower.contains([':', '\0', '/', '\\']) || lower == "." || lower == ".." {
                continue;
            }
            listing.entries.entry(lower).or_insert((name, is_dir));
        }
        let listing = Arc::new(listing);
        self.listings.write().unwrap_or_else(PoisonError::into_inner).insert(dir.to_string(), listing.clone());
        Some(listing)
    }

    /// The file or folder at a normalized path: its path on disk and whether it is a folder.
    fn resolve(&self, path: &str) -> Option<(PathBuf, bool)> {
        debug_assert!(path::is_normalized(path), "{path:?}");
        let mut real = self.root.clone();
        if path.is_empty() {
            return real.is_dir().then_some((real, true));
        }
        let mut is_dir = true;
        let mut dir_end = 0;
        for component in path.split('/') {
            if !is_dir {
                return None;
            }
            let listing = self.listing(&path[..dir_end], &real)?;
            let (name, d) = listing.entries.get(component)?;
            real.push(name);
            is_dir = *d;
            dir_end = if dir_end == 0 { component.len() } else { dir_end + 1 + component.len() };
        }
        Some((real, is_dir))
    }

    /// `Some(is a folder)` if something is at the normalized `path`.
    pub fn contains(&self, path: &str) -> Option<bool> {
        self.resolve(path).map(|(_, is_dir)| is_dir)
    }

    /// The file's path on disk.
    pub fn disk_path(&self, path: &str) -> Option<PathBuf> {
        self.resolve(path).filter(|(_, is_dir)| !is_dir).map(|(real, _)| real)
    }

    /// Reads the file at the normalized `path`.
    pub fn read(&self, path: &str) -> io::Result<Vec<u8>> {
        match self.resolve(path) {
            Some((real, false)) => {
                let len = std::fs::metadata(&real)?.len();
                if len > MAX_FILE {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, format!("{}: {len} bytes is too large", real.display())));
                }
                std::fs::read(real)
            }
            Some((real, true)) => Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{} is a folder", real.display()))),
            None => Err(io::Error::new(io::ErrorKind::NotFound, format!("{path} is not in {}", self.root.display()))),
        }
    }

    /// The names (lower case) directly inside the normalized folder `dir`, each with whether
    /// it is a folder, sorted by name.
    pub fn list(&self, dir: &str) -> Vec<(String, bool)> {
        let Some((real, true)) = self.resolve(dir) else { return Vec::new() };
        let Some(listing) = self.listing(dir, &real) else { return Vec::new() };
        let mut out: Vec<(String, bool)> = listing.entries.iter().map(|(name, (_, is_dir))| (name.clone(), *is_dir)).collect();
        out.sort();
        out
    }

    /// Every file below the root, as normalized paths, sorted.
    pub fn walk(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.walk_into("", 0, &mut out);
        out.sort();
        out
    }

    fn walk_into(&self, dir: &str, depth: usize, out: &mut Vec<String>) {
        if depth > MAX_DEPTH {
            return;
        }
        for (name, is_dir) in self.list(dir) {
            let child = path::join(dir, &name);
            if is_dir {
                self.walk_into(&child, depth + 1, out);
            } else {
                out.push(child);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn case_insensitive_lookup() {
        let tmp = TempDir::new();
        let root = tmp.path().join("garrysmod");
        std::fs::create_dir_all(root.join("Materials/Models")).unwrap();
        std::fs::write(root.join("Materials/Models/Chair.VMT"), b"vmt").unwrap();
        std::fs::write(root.join("readme.txt"), b"hi").unwrap();

        let mount = DirMount::new(&root);
        assert_eq!(mount.read("materials/models/chair.vmt").unwrap(), b"vmt");
        assert_eq!(mount.contains("materials/models"), Some(true));
        assert_eq!(mount.contains("materials/models/chair.vmt"), Some(false));
        assert_eq!(mount.contains("materials/models/chair.vmt/x"), None);
        assert_eq!(mount.contains("nothing"), None);
        assert_eq!(mount.contains(""), Some(true));
        assert!(mount.read("materials").is_err());
        assert_eq!(mount.list(""), [("materials".to_string(), true), ("readme.txt".to_string(), false)]);
        assert_eq!(mount.walk(), ["materials/models/chair.vmt", "readme.txt"]);
        assert_eq!(mount.disk_path("readme.txt"), Some(root.join("readme.txt")));

        // a file added later shows up after `forget`
        std::fs::write(root.join("new.txt"), b"").unwrap();
        assert_eq!(mount.contains("new.txt"), None);
        mount.forget();
        assert_eq!(mount.contains("new.txt"), Some(false));
    }
}
