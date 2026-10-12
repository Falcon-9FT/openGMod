//! The game file system: folders and archives stacked in search order, as the engine's
//! search paths are.
//!
//! Each [`Mount`] carries search path IDs (`GAME`, `MOD`, `DOWNLOAD`, ...). A lookup names an
//! ID and goes through the mounts that have it, front to back; the first one with the file
//! wins. This is the whole of the engine's file system that the game reads through - there
//! are no write operations, by design: openGMOD writes only into its own folder
//! ([`crate::UserDir`]).

use std::collections::{BTreeSet, HashSet};
use std::fmt;
use std::io;
use std::path::Path;
use std::sync::Arc;

use crate::dir::DirMount;
use crate::gma::GmaArchive;
use crate::memory::MemoryMount;
use crate::path::{self, normalize};
use crate::vpk::VpkArchive;
use crate::zip::ZipArchive;

/// What a mount reads from.
#[derive(Debug)]
pub enum Backing {
    Dir(DirMount),
    Vpk(VpkArchive),
    Gma(GmaArchive),
    /// A zip; above all the pakfile of the map being played.
    Zip(ZipArchive),
    /// Files in memory: what a server sent.
    Memory(MemoryMount),
}

/// Which paths a mount hides: those it is to have no file at.
pub type Hide = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// One entry of the search order.
pub struct Mount {
    /// Search path IDs, upper case.
    pub ids: Vec<String>,
    pub backing: Backing,
    /// An addon's - a folder of `garrysmod/addons`, a `.gma` - rather than the game's own.
    pub addon: bool,
    /// Files it does not show, as if it had none there (normalized paths): the Lua of a
    /// player's addons on a server that did not send it.
    hidden: Option<Hide>,
}

impl fmt::Debug for Mount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} ({})", self.kind(), self.origin().display(), self.ids.join("+"))
    }
}

impl Mount {
    pub fn new(ids: &[&str], backing: Backing) -> Mount {
        Mount { ids: ids.iter().map(|id| id.to_ascii_uppercase()).collect(), backing, addon: false, hidden: None }
    }

    /// The mount, as an addon's.
    pub fn of_addon(mut self) -> Mount {
        self.addon = true;
        self
    }

    /// Hides files: those `hide` says, from now on (`None`: none).
    pub fn hide(&mut self, hide: Option<Hide>) {
        self.hidden = hide;
    }

    fn hides(&self, path: &str) -> bool {
        self.hidden.as_ref().is_some_and(|hide| hide(path))
    }

    /// Whether lookups under `path_id` see this mount. An empty ID sees every mount.
    pub fn has_id(&self, path_id: &str) -> bool {
        path_id.is_empty() || self.ids.iter().any(|id| id.eq_ignore_ascii_case(path_id))
    }

    /// The folder, or the archive file.
    pub fn origin(&self) -> &Path {
        match &self.backing {
            Backing::Dir(d) => d.root(),
            Backing::Vpk(v) => v.path(),
            Backing::Gma(g) => g.path(),
            Backing::Zip(z) => z.path(),
            Backing::Memory(m) => m.name(),
        }
    }

    pub fn kind(&self) -> &'static str {
        match &self.backing {
            Backing::Dir(_) => "folder",
            Backing::Vpk(_) => "vpk",
            Backing::Gma(_) => "gma",
            Backing::Zip(_) => "zip",
            Backing::Memory(_) => "memory",
        }
    }

    /// `Some(is a folder)` if something is at the normalized `path`.
    pub fn contains(&self, path: &str) -> Option<bool> {
        let found = match &self.backing {
            Backing::Dir(d) => d.contains(path),
            Backing::Vpk(v) => v.contains(path),
            Backing::Gma(g) => g.contains(path),
            Backing::Zip(z) => z.contains(path),
            Backing::Memory(m) => m.contains(path),
        };
        found.filter(|&dir| dir || !self.hides(path))
    }

    /// Reads the file at the normalized `path`.
    pub fn read(&self, path: &str) -> io::Result<Vec<u8>> {
        if self.hides(path) {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("{path} is not there")));
        }
        match &self.backing {
            Backing::Dir(d) => d.read(path),
            Backing::Vpk(v) => v.read(path),
            Backing::Gma(g) => g.read(path),
            Backing::Zip(z) => z.read(path),
            Backing::Memory(m) => m.read(path),
        }
    }

    /// The names directly inside the normalized folder `dir`, each with whether it is a folder.
    pub fn list(&self, dir: &str) -> Vec<(String, bool)> {
        let mut listed = match &self.backing {
            Backing::Dir(d) => d.list(dir),
            Backing::Vpk(v) => v.list(dir),
            Backing::Gma(g) => g.list(dir),
            Backing::Zip(z) => z.list(dir),
            Backing::Memory(m) => m.list(dir),
        };
        if self.hidden.is_some() {
            listed.retain(|(name, is_dir)| *is_dir || !self.hides(&if dir.is_empty() { name.clone() } else { format!("{dir}/{name}") }));
        }
        listed
    }

    /// Every file in the mount, as normalized paths, sorted.
    pub fn files(&self) -> Vec<String> {
        let mut files = match &self.backing {
            Backing::Dir(d) => d.walk(),
            Backing::Vpk(v) => v.entries().iter().map(|e| e.path.clone()).collect(),
            Backing::Gma(g) => g.entries().iter().map(|e| e.path.clone()).collect(),
            Backing::Zip(z) => z.entries().iter().map(|e| e.path.clone()).collect(),
            Backing::Memory(m) => m.paths(),
        };
        files.retain(|f| !self.hides(f));
        files
    }
}

/// Files and folders found by [`Vfs::find`], names only, sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Found {
    pub files: Vec<String>,
    pub dirs: Vec<String>,
}

/// The stack of mounts.
#[derive(Debug, Default)]
pub struct Vfs {
    mounts: Vec<Mount>,
}

fn not_found(path: &str, path_id: &str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("{path} was not found (search path {path_id:?})"))
}

impl Vfs {
    pub fn new() -> Vfs {
        Vfs::default()
    }

    /// Adds a mount behind all others.
    pub fn push(&mut self, mount: Mount) {
        self.mounts.push(mount);
    }

    /// Adds a mount at `index` of the search order (0: before all others).
    pub fn insert(&mut self, index: usize, mount: Mount) {
        self.mounts.insert(index.min(self.mounts.len()), mount);
    }

    /// Takes the mount at `index` out of the search order: the pakfile of a map that is left.
    pub fn remove(&mut self, index: usize) -> Option<Mount> {
        (index < self.mounts.len()).then(|| self.mounts.remove(index))
    }

    /// The mounts in search order.
    pub fn mounts(&self) -> &[Mount] {
        &self.mounts
    }

    /// Hides files of the addons' mounts: what `hide` says (`None`: shows them all again).
    pub fn hide_in_addons(&mut self, hide: Option<Hide>) {
        for mount in self.mounts.iter_mut().filter(|m| m.addon) {
            mount.hide(hide.clone());
        }
    }

    fn search<'a>(&'a self, path_id: &str) -> impl Iterator<Item = &'a Mount> {
        self.mounts.iter().filter(move |m| m.has_id(path_id))
    }

    /// The mount a file is read from: the first, in search order, that has it.
    pub fn locate(&self, path: &str, path_id: &str) -> Option<&Mount> {
        let path = normalize(path)?;
        self.search(path_id).find(|m| m.contains(&path) == Some(false))
    }

    /// Reads a file. Paths are looked up without regard to case; one that would leave the
    /// game file system (`../`) is an error.
    pub fn read(&self, path: &str, path_id: &str) -> io::Result<Vec<u8>> {
        let normalized =
            normalize(path).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("{path:?} is not a valid game path")))?;
        match self.search(path_id).find(|m| m.contains(&normalized) == Some(false)) {
            Some(mount) => mount.read(&normalized),
            None => Err(not_found(path, path_id)),
        }
    }

    /// Whether a file or folder is at `path`.
    pub fn exists(&self, path: &str, path_id: &str) -> bool {
        normalize(path).is_some_and(|p| self.search(path_id).any(|m| m.contains(&p).is_some()))
    }

    /// Whether a folder is at `path`.
    pub fn is_dir(&self, path: &str, path_id: &str) -> bool {
        normalize(path).is_some_and(|p| self.search(path_id).any(|m| m.contains(&p) == Some(true)))
    }

    /// What Lua's `file.Find` returns: the files and folders matching `pattern`, a path whose
    /// last component may hold `*` and `?`. Names appear once even when several mounts have
    /// them, sorted.
    pub fn find(&self, pattern: &str, path_id: &str) -> Found {
        let Some(pattern) = normalize(pattern) else { return Found::default() };
        let (dir, name_pattern) = path::split(&pattern);
        let mut files = BTreeSet::new();
        let mut dirs = BTreeSet::new();
        for mount in self.search(path_id) {
            for (name, is_dir) in mount.list(dir) {
                if path::wildcard_match(name_pattern, &name) {
                    if is_dir {
                        dirs.insert(name);
                    } else {
                        files.insert(name);
                    }
                }
            }
        }
        Found { files: files.into_iter().collect(), dirs: dirs.into_iter().collect() }
    }

    /// Every file visible under `path_id`, once each, with the index of the mount it is read
    /// from; sorted by path.
    pub fn files(&self, path_id: &str) -> Vec<(String, usize)> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for (index, mount) in self.mounts.iter().enumerate() {
            if !mount.has_id(path_id) {
                continue;
            }
            for file in mount.files() {
                if seen.insert(file.clone()) {
                    out.push((file, index));
                }
            }
        }
        out.sort();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use crate::{gma, vpk};

    fn stack(tmp: &TempDir) -> Vfs {
        let root = tmp.path();
        let loose = root.join("garrysmod");
        std::fs::create_dir_all(loose.join("Lua/AutoRun")).unwrap();
        std::fs::write(loose.join("Lua/AutoRun/loose.lua"), b"loose").unwrap();
        std::fs::write(loose.join("Lua/shared.lua"), b"from the folder").unwrap();

        let pack = root.join("garrysmod_dir.vpk");
        vpk::write_single(
            &pack,
            &[("lua/shared.lua", b"from the pack"), ("lua/autorun/packed.lua", b"packed"), ("materials/a.vmt", b"vmt")],
        )
        .unwrap();

        let addon = root.join("addon.gma");
        gma::write(
            &addon,
            &gma::GmaInfo { title: "Addon".into(), ..Default::default() },
            &[("lua/autorun/addon.lua", b"addon"), ("lua/shared.lua", b"from the addon")],
        )
        .unwrap();

        let mut vfs = Vfs::new();
        vfs.push(Mount::new(&["game", "mod"], Backing::Vpk(VpkArchive::open(&pack).unwrap())));
        vfs.push(Mount::new(&["GAME"], Backing::Dir(DirMount::new(&loose))));
        vfs.insert(0, Mount::new(&["GAME", "WORKSHOP"], Backing::Gma(GmaArchive::open(&addon).unwrap())).of_addon());
        vfs
    }

    #[test]
    fn addons_hide_what_they_are_told_to() {
        let tmp = TempDir::new();
        let mut vfs = stack(&tmp);
        // the addon's Lua out of sight: the game's own shows through, the addon's own is gone
        vfs.hide_in_addons(Some(Arc::new(|path: &str| path.starts_with("lua/"))));
        assert_eq!(vfs.read("lua/shared.lua", "GAME").unwrap(), b"from the pack");
        assert!(vfs.read("lua/autorun/addon.lua", "GAME").is_err());
        assert!(vfs.locate("lua/autorun/addon.lua", "GAME").is_none());
        assert_eq!(vfs.find("lua/autorun/*.lua", "GAME").files, ["loose.lua", "packed.lua"]);
        assert!(!vfs.files("WORKSHOP").iter().any(|(f, _)| f.starts_with("lua/")));
        assert!(vfs.is_dir("lua/autorun", "WORKSHOP"), "its folders stay");
        // and back
        vfs.hide_in_addons(None);
        assert_eq!(vfs.read("lua/autorun/addon.lua", "GAME").unwrap(), b"addon");
    }

    #[test]
    fn first_mount_wins() {
        let tmp = TempDir::new();
        let vfs = stack(&tmp);
        assert_eq!(vfs.read("lua/shared.lua", "GAME").unwrap(), b"from the addon");
        assert_eq!(vfs.read("LUA\\Shared.lua", "mod").unwrap(), b"from the pack");
        assert_eq!(vfs.read("lua/autorun/loose.lua", "game").unwrap(), b"loose");
        assert_eq!(vfs.locate("lua/autorun/loose.lua", "GAME").unwrap().kind(), "folder");
        assert!(vfs.read("lua/autorun/loose.lua", "MOD").is_err());
        assert!(vfs.read("lua/missing.lua", "GAME").unwrap_err().kind() == io::ErrorKind::NotFound);
        assert!(vfs.read("../secret.txt", "GAME").unwrap_err().kind() == io::ErrorKind::InvalidInput);
        assert!(vfs.exists("lua/autorun", "GAME"));
        assert!(vfs.is_dir("lua/autorun", "GAME"));
        assert!(!vfs.is_dir("lua/shared.lua", "GAME"));
    }

    #[test]
    fn find_merges_mounts() {
        let tmp = TempDir::new();
        let vfs = stack(&tmp);
        let found = vfs.find("lua/autorun/*.lua", "GAME");
        assert_eq!(found.files, ["addon.lua", "loose.lua", "packed.lua"]);
        assert!(found.dirs.is_empty());
        let found = vfs.find("lua/*", "GAME");
        assert_eq!(found.files, ["shared.lua"]);
        assert_eq!(found.dirs, ["autorun"]);
        assert_eq!(vfs.find("*", "MOD").dirs, ["lua", "materials"]);
        assert_eq!(vfs.find("lua/autorun/*.lua", "WORKSHOP").files, ["addon.lua"]);
        assert_eq!(vfs.find("../*", "GAME"), Found::default());
    }

    #[test]
    fn every_file_once() {
        let tmp = TempDir::new();
        let vfs = stack(&tmp);
        let files = vfs.files("GAME");
        let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            paths,
            ["lua/autorun/addon.lua", "lua/autorun/loose.lua", "lua/autorun/packed.lua", "lua/shared.lua", "materials/a.vmt"]
        );
        // lua/shared.lua comes from the addon, mount 0
        assert_eq!(files[3].1, 0);
    }
}
