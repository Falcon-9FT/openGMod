//! Finding the player's Garry's Mod.
//!
//! openGMOD brings no game content: it plays on the files of an installed, licensed Garry's
//! Mod and on the workshop addons its owner subscribed to. This module finds that
//! installation - a folder named by the player, `GMOD_ROOT`, or Steam's libraries - and the
//! Steam library around it, where the subscribed workshop items are.

use std::fmt;
use std::path::{Path, PathBuf};

use gmod_kv::{Options, Value};

/// Steam's app ID of Garry's Mod.
pub const APP_ID: u32 = 4000;
/// The folder Steam installs it into, under `steamapps/common`.
const INSTALL_DIR: &str = "GarrysMod";

/// What a folder must have to be used as Garry's Mod.
const ESSENTIALS: [&str; 2] = ["garrysmod/gameinfo.txt", "garrysmod/garrysmod_dir.vpk"];

/// A Garry's Mod installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    /// The folder with `garrysmod/`, `sourceengine/`, `platform/` and the game's programs.
    pub root: PathBuf,
    /// The Steam library it is in, when known.
    pub library: Option<PathBuf>,
}

/// Garry's Mod was not found; what was looked at and what it lacked.
#[derive(Debug, Clone, Default)]
pub struct LocateError {
    pub tried: Vec<(PathBuf, Vec<&'static str>)>,
}

impl fmt::Display for LocateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Garry's Mod was not found.")?;
        for (path, missing) in &self.tried {
            writeln!(f, "  {}: lacks {}", path.display(), missing.join(", "))?;
        }
        write!(f, "Name the Garry's Mod folder (the one with garrysmod/ and sourceengine/ in it), or set GMOD_ROOT to it.")
    }
}

impl std::error::Error for LocateError {}

/// What `root` lacks to be used as Garry's Mod (nothing: it is fine).
pub fn missing(root: &Path) -> Vec<&'static str> {
    ESSENTIALS.into_iter().filter(|essential| !root.join(essential).is_file()).collect()
}

impl Install {
    /// `garrysmod/`: `gameinfo.txt`, the game's packs, addons, Lua, maps.
    pub fn game_dir(&self) -> PathBuf {
        self.root.join("garrysmod")
    }

    /// Where Steam keeps the workshop items the player subscribed to.
    pub fn workshop_dir(&self) -> Option<PathBuf> {
        self.library.as_ref().map(|library| library.join("steamapps/workshop/content").join(APP_ID.to_string()))
    }

    /// The installation at `path`, which may name the installation folder, its `garrysmod`
    /// folder, or a file in the installation folder (`gmod.exe`).
    pub fn at(path: &Path) -> Result<Install, LocateError> {
        Install::at_among(path, &steam_roots())
    }

    /// [`Install::at`], with the Steam installations to look for its library in when the
    /// folder is not where Steam puts games (`<library>/steamapps/common/<folder>`).
    fn at_among(path: &Path, steams: &[PathBuf]) -> Result<Install, LocateError> {
        let mut candidates = Vec::new();
        if path.is_file() {
            candidates.extend(path.parent().map(Path::to_path_buf));
        } else {
            candidates.push(path.to_path_buf());
            if path.file_name().is_some_and(|name| name.eq_ignore_ascii_case("garrysmod")) {
                candidates.extend(path.parent().map(Path::to_path_buf));
            }
        }
        let mut error = LocateError::default();
        for candidate in candidates {
            let missing = missing(&candidate);
            if missing.is_empty() {
                let library = library_of(&candidate).or_else(|| library_for(&candidate, steams));
                return Ok(Install { root: candidate, library });
            }
            error.tried.push((candidate, missing));
        }
        Err(error)
    }

    /// Finds Garry's Mod. A folder the player named is the only one tried; otherwise
    /// `GMOD_ROOT`, then the Steam libraries of every Steam installation on the computer.
    pub fn locate(named: Option<&Path>) -> Result<Install, LocateError> {
        if let Some(path) = named {
            return Install::at(path);
        }
        let mut error = LocateError::default();
        if let Some(root) = std::env::var_os("GMOD_ROOT").filter(|v| !v.is_empty()) {
            match Install::at(Path::new(&root)) {
                Ok(install) => return Ok(install),
                Err(e) => error.tried.extend(e.tried),
            }
        }
        let mut seen = Vec::new();
        for steam in steam_roots() {
            for library in libraries(&steam) {
                let key = library.canonicalize().unwrap_or_else(|_| library.clone());
                if seen.contains(&key) {
                    continue;
                }
                seen.push(key);
                let dir = app_dir(&library);
                if !dir.exists() {
                    continue;
                }
                let missing = missing(&dir);
                if missing.is_empty() {
                    return Ok(Install { root: dir, library: Some(library) });
                }
                error.tried.push((dir, missing));
            }
        }
        Err(error)
    }
}

/// `<library>/steamapps/common/<folder>` → `<library>`.
fn library_of(root: &Path) -> Option<PathBuf> {
    let common = root.parent()?;
    let steamapps = common.parent()?;
    let fits = common.file_name()?.eq_ignore_ascii_case("common") && steamapps.file_name()?.eq_ignore_ascii_case("steamapps");
    if fits { steamapps.parent().map(Path::to_path_buf) } else { None }
}

/// The library of a game folder that is not where Steam puts games - one reached by a link,
/// or a copy: the library Steam installed it into, else the first with subscribed workshop
/// items of Garry's Mod, as those are what the library is needed for.
fn library_for(root: &Path, steams: &[PathBuf]) -> Option<PathBuf> {
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    let libraries: Vec<PathBuf> = steams.iter().flat_map(|steam| libraries(steam)).collect();
    let workshop = |library: &PathBuf| library.join("steamapps/workshop/content").join(APP_ID.to_string()).is_dir();
    libraries.iter().find(|library| same(&app_dir(library), root)).or_else(|| libraries.iter().find(|l| workshop(l))).cloned()
}

pub(crate) fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).filter(|h| !h.is_empty()).map(PathBuf::from)
}

/// Every place a Steam installation may be on this computer, most likely first.
fn steam_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    #[cfg(windows)]
    roots.extend(windows_registry_steam());
    if let Some(home) = home() {
        for relative in [
            ".steam/steam",
            ".steam/root",
            ".local/share/Steam",
            ".var/app/com.valvesoftware.Steam/data/Steam",
            "snap/steam/common/.local/share/Steam",
            "Library/Application Support/Steam",
        ] {
            roots.push(home.join(relative));
        }
    }
    if cfg!(windows) {
        for drive in b'C'..=b'Z' {
            let drive = PathBuf::from(format!("{}:\\", drive as char));
            if drive.exists() {
                roots.push(drive.join("Program Files (x86)").join("Steam"));
                roots.push(drive.join("Program Files").join("Steam"));
                roots.push(drive.join("Steam"));
            }
        }
    }
    roots.retain(|root| root.is_dir());
    roots
}

/// Steam's folder from the registry (`reg query`, so that no Windows API binding is needed).
#[cfg(windows)]
fn windows_registry_steam() -> Vec<PathBuf> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut out = Vec::new();
    for (key, value) in [
        (r"HKCU\Software\Valve\Steam", "SteamPath"),
        (r"HKLM\SOFTWARE\WOW6432Node\Valve\Steam", "InstallPath"),
        (r"HKLM\SOFTWARE\Valve\Steam", "InstallPath"),
    ] {
        let Ok(output) = std::process::Command::new("reg").args(["query", key, "/v", value]).creation_flags(CREATE_NO_WINDOW).output()
        else {
            continue;
        };
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if let Some(at) = line.find("REG_SZ") {
                let path = line[at + "REG_SZ".len()..].trim();
                if !path.is_empty() {
                    out.push(PathBuf::from(path));
                }
            }
        }
    }
    out
}

fn read_steam_file(path: &Path) -> Option<gmod_kv::Document> {
    let bytes = std::fs::read(path).ok()?;
    let (doc, _) = gmod_kv::parse_lenient(&gmod_kv::decode_text(&bytes), Options { escapes: true });
    Some(doc)
}

/// The libraries of a Steam installation, its own folder first.
pub(crate) fn libraries(steam: &Path) -> Vec<PathBuf> {
    let mut out = vec![steam.to_path_buf()];
    for vdf in [steam.join("steamapps/libraryfolders.vdf"), steam.join("config/libraryfolders.vdf")] {
        let Some(doc) = read_steam_file(&vdf) else { continue };
        let Some(folders) = doc.root.block("libraryfolders") else { continue };
        for pair in folders {
            if pair.key.is_empty() || !pair.key.bytes().all(|c| c.is_ascii_digit()) {
                continue;
            }
            // current files: "0" { "path" "..." ... }; old ones: "1" "D:\\SteamLibrary"
            let path = match &pair.value {
                Value::Block(block) => block.str("path"),
                Value::Str(s) => Some(s.as_str()),
            };
            if let Some(path) = path.filter(|p| !p.is_empty()) {
                let path = PathBuf::from(path);
                if !out.contains(&path) {
                    out.push(path);
                }
            }
        }
    }
    out
}

/// Where Garry's Mod is (or would be) installed in a library.
pub(crate) fn app_dir(library: &Path) -> PathBuf {
    let manifest = library.join(format!("steamapps/appmanifest_{APP_ID}.acf"));
    let folder = read_steam_file(&manifest)
        .and_then(|doc| doc.root.block("AppState").and_then(|state| state.str("installdir")).map(str::to_string))
        .filter(|f| !f.is_empty() && !f.contains(['/', '\\', ':']) && f != "..")
        .unwrap_or_else(|| INSTALL_DIR.to_string());
    library.join("steamapps").join("common").join(folder)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{TempDir, fake_install};

    #[test]
    fn named_folder() {
        let tmp = TempDir::new();
        let root = tmp.path().join("SteamLibrary/steamapps/common/GarrysMod");
        fake_install(&root);
        let library = tmp.path().join("SteamLibrary");
        let expected = Install { root: root.clone(), library: Some(library.clone()) };
        assert_eq!(Install::at(&root).unwrap(), expected);
        assert_eq!(Install::at(&root.join("garrysmod")).unwrap(), expected);
        assert_eq!(Install::at(&root.join("gmod.exe")).unwrap(), expected);
        assert_eq!(expected.workshop_dir().unwrap(), library.join("steamapps/workshop/content/4000"));

        let elsewhere = tmp.path().join("Games/GMod");
        fake_install(&elsewhere);
        assert_eq!(Install::at_among(&elsewhere, &[]).unwrap().library, None);
        // a folder elsewhere still finds the subscribed items of the Steam on the computer
        let steam = tmp.path().join("Steam");
        std::fs::create_dir_all(steam.join("steamapps")).unwrap();
        assert_eq!(Install::at_among(&elsewhere, std::slice::from_ref(&steam)).unwrap().library, None);
        std::fs::create_dir_all(steam.join("steamapps/workshop/content/4000")).unwrap();
        assert_eq!(Install::at_among(&elsewhere, std::slice::from_ref(&steam)).unwrap().library, Some(steam));

        let err = Install::at(&tmp.path().join("Games")).unwrap_err();
        assert_eq!(err.tried[0].1, ESSENTIALS);
        assert!(err.to_string().contains("GMOD_ROOT"));
    }

    #[test]
    fn steam_libraries() {
        let tmp = TempDir::new();
        let steam = tmp.path().join("Steam");
        let second = tmp.path().join("Second Library");
        std::fs::create_dir_all(steam.join("steamapps")).unwrap();
        std::fs::create_dir_all(second.join("steamapps")).unwrap();
        let second_escaped = second.display().to_string().replace('\\', "\\\\");
        let vdf = format!(
            "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t\t\"apps\" {{ \"4000\" \"1\" }}\n\t}}\n\t\"1\"\n\t{{\n\t\t\"path\"\t\t\"{second_escaped}\"\n\t}}\n}}\n",
            steam.display().to_string().replace('\\', "\\\\"),
        );
        std::fs::write(steam.join("steamapps/libraryfolders.vdf"), vdf).unwrap();
        assert_eq!(libraries(&steam), [steam.clone(), second.clone()]);

        // the folder name comes from the app manifest
        std::fs::write(
            second.join("steamapps/appmanifest_4000.acf"),
            "\"AppState\"\n{\n\t\"appid\"\t\t\"4000\"\n\t\"installdir\"\t\t\"Garrys Mod Custom\"\n}\n",
        )
        .unwrap();
        assert_eq!(app_dir(&second), second.join("steamapps/common/Garrys Mod Custom"));
        assert_eq!(app_dir(&steam), steam.join("steamapps/common/GarrysMod"));

        // the old format: numbered keys with the path as value
        std::fs::write(
            steam.join("steamapps/libraryfolders.vdf"),
            format!("\"LibraryFolders\" {{ \"TimeNextStatsReport\" \"1\" \"1\" \"{second_escaped}\" }}"),
        )
        .unwrap();
        assert_eq!(libraries(&steam), [steam.clone(), second]);
    }
}
