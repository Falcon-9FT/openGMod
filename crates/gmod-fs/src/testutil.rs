//! Helpers for tests: a temporary folder that removes itself, a fake installation.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

pub(crate) struct TempDir(PathBuf);

impl TempDir {
    pub(crate) fn new() -> TempDir {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
        let name = format!("gmod-fs-test-{}-{}-{nanos}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed));
        let path = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The least a folder needs to pass for a Garry's Mod installation.
pub(crate) fn fake_install(root: &Path) {
    std::fs::create_dir_all(root.join("garrysmod")).unwrap();
    std::fs::write(root.join("garrysmod/gameinfo.txt"), b"\"GameInfo\" {}").unwrap();
    std::fs::write(root.join("garrysmod/garrysmod_dir.vpk"), b"").unwrap();
    std::fs::write(root.join("gmod.exe"), b"").unwrap();
}
