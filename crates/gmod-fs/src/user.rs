//! openGMOD's own folder: settings, logs, Lua's `data` folder, caches, unpacked downloads.
//!
//! It is the only place openGMOD writes to. The Garry's Mod installation is only ever read,
//! so a user folder inside it is refused. Paths inside the folder come from game code (Lua's
//! `file.Write`), so they are checked component by component: no way out of the folder, and
//! no name Windows cannot store or treats as a device.

use std::io;
use std::path::{Path, PathBuf};

use crate::install::{Install, home};
use crate::path;

/// openGMOD's writable folder.
#[derive(Debug, Clone)]
pub struct UserDir {
    root: PathBuf,
}

fn invalid_input(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

impl UserDir {
    /// `$OPENGMOD_HOME`, else `.opengmod` in the home folder.
    pub fn default_root() -> Option<PathBuf> {
        if let Some(root) = std::env::var_os("OPENGMOD_HOME").filter(|v| !v.is_empty()) {
            return Some(PathBuf::from(root));
        }
        home().map(|h| h.join(".opengmod"))
    }

    /// Opens the folder at `root`, creating it if needed. Refused if it would lie inside the
    /// installation.
    pub fn open(root: impl Into<PathBuf>, install: Option<&Install>) -> io::Result<UserDir> {
        let root = root.into();
        let refuse = |at: &Path| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is inside the Garry's Mod installation; openGMOD never writes there - choose another folder", at.display()),
            )
        };
        if let Some(install) = install
            && is_inside(&root, &install.root)
        {
            return Err(refuse(&root));
        }
        std::fs::create_dir_all(&root)?;
        let real = root.canonicalize()?;
        // again with the real path: a symbolic link could lead into the installation
        if let Some(install) = install
            && is_inside(&real, &install.root)
        {
            return Err(refuse(&real));
        }
        Ok(UserDir { root: real })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The path on disk for a game-style path inside the folder (`data/mytool/save.txt`).
    /// Names are lower-cased, as GMod does when Lua writes files.
    pub fn path(&self, relative: &str) -> io::Result<PathBuf> {
        let normalized = path::normalize(relative)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| invalid_input(format!("{relative:?} is not a path inside the user folder")))?;
        let mut out = self.root.clone();
        for component in normalized.split('/') {
            if !portable_name(component) {
                return Err(invalid_input(format!("{relative:?}: {component:?} cannot be a file name on every system")));
            }
            out.push(component);
        }
        Ok(out)
    }

    /// A folder inside, created if needed.
    pub fn dir(&self, relative: &str) -> io::Result<PathBuf> {
        let dir = self.path(relative)?;
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    pub fn read(&self, relative: &str) -> io::Result<Vec<u8>> {
        std::fs::read(self.path(relative)?)
    }

    /// Writes a file whole: into a temporary file first, then renamed over the old one, so
    /// that a crash leaves either the old or the new contents.
    pub fn write(&self, relative: &str, data: &[u8]) -> io::Result<()> {
        let target = self.path(relative)?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut temporary = target.clone().into_os_string();
        temporary.push(".part");
        std::fs::write(&temporary, data)?;
        std::fs::rename(&temporary, &target)
    }
}

/// Whether `name` can be a file name on Windows, macOS and Linux alike.
fn portable_name(name: &str) -> bool {
    const RESERVED: [&str; 22] = [
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4",
        "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    let stem = name.split('.').next().unwrap_or(name).trim_end();
    !name.is_empty()
        && name.len() <= 255
        && !name.ends_with(['.', ' '])
        && !name.chars().any(|c| c < ' ' || "<>:\"|?*\\/".contains(c))
        && !RESERVED.contains(&stem.to_ascii_lowercase().as_str())
}

/// Whether `path` is `root` or lies inside it, comparing real locations where they exist (a
/// path that does not exist yet is taken from its nearest existing folder).
pub fn is_inside(path: &Path, root: &Path) -> bool {
    let path = real_location(path);
    let root = real_location(root);
    if cfg!(any(windows, target_os = "macos")) {
        // these systems compare names without regard to case
        let lower = |p: &Path| p.components().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).collect::<Vec<_>>();
        lower(&path).starts_with(&lower(&root))
    } else {
        path.starts_with(&root)
    }
}

fn real_location(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut rest = Vec::new();
    loop {
        if let Ok(real) = existing.canonicalize() {
            let mut out = real;
            out.extend(rest.iter().rev());
            return out;
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                existing = parent;
            }
            _ => return std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{TempDir, fake_install};

    fn install(tmp: &TempDir) -> Install {
        let root = tmp.path().join("GarrysMod");
        fake_install(&root);
        Install { root, library: None }
    }

    #[test]
    fn refuses_the_installation() {
        let tmp = TempDir::new();
        let install = install(&tmp);
        for inside in [install.root.clone(), install.game_dir(), install.game_dir().join("data/opengmod")] {
            let err = UserDir::open(&inside, Some(&install)).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{}", inside.display());
        }
        assert!(!install.game_dir().join("data").exists(), "nothing was created inside");

        #[cfg(unix)]
        {
            // a link from outside into the installation is followed and refused
            let link = tmp.path().join("link");
            std::os::unix::fs::symlink(install.game_dir(), &link).unwrap();
            assert!(UserDir::open(link.join("sub"), Some(&install)).is_err());
        }

        let user = UserDir::open(tmp.path().join("user"), Some(&install)).unwrap();
        assert!(user.root().ends_with("user"));
    }

    #[test]
    fn writes_stay_inside() {
        let tmp = TempDir::new();
        let user = UserDir::open(tmp.path().join("user"), None).unwrap();
        user.write("data/MyTool/Save.TXT", b"one").unwrap();
        user.write("data/mytool/save.txt", b"two").unwrap();
        assert_eq!(user.read("DATA/mytool/save.txt").unwrap(), b"two");
        assert!(user.root().join("data/mytool/save.txt").is_file());
        assert!(!user.root().join("data/mytool/save.txt.part").exists());

        for bad in [
            "../escape.txt",
            "data/../../escape.txt",
            "",
            "/",
            "c:/windows/x.txt",
            "data/con.txt",
            "data/aux",
            "data/name.",
            "data/a|b.txt",
            "data/a\u{1}b",
        ] {
            assert!(user.write(bad, b"x").is_err(), "{bad:?}");
        }
        assert!(!tmp.path().join("escape.txt").exists());
        assert!(user.dir("cache/workshop").unwrap().is_dir());
    }
}
