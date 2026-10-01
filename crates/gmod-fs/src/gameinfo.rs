//! Where the engine looks for files: the `SearchPaths` of `gameinfo.txt`, and the extra game
//! folders of `cfg/mount.cfg`.
//!
//! A search path line is `ids  location`: IDs joined with `+` (`game+mod`), and a location
//! relative to the installation folder, optionally starting with `|all_source_engine_paths|`
//! (the installation folder) or `|gameinfo_path|` (the folder of `gameinfo.txt`). A location
//! ending in `.vpk` names a pack by its base name (`garrysmod/garrysmod.vpk` is
//! `garrysmod/garrysmod_dir.vpk`); one ending in `/*` mounts every folder inside.

use std::path::{Path, PathBuf};

use gmod_kv::{Document, Value};

/// What a search path line points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A folder of loose files.
    Dir(PathBuf),
    /// A pack, by its directory file (`name_dir.vpk`, or `name.vpk` for a single-file pack).
    Vpk(PathBuf),
    /// Every folder inside this one (`garrysmod/addons/*`), each mounted on its own.
    EachDir(PathBuf),
}

/// One line of `SearchPaths`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPath {
    /// Upper case.
    pub ids: Vec<String>,
    pub target: Target,
    /// Line in `gameinfo.txt`.
    pub line: u32,
}

/// IDs that only say where the engine writes or where its programs are. openGMOD writes
/// nothing into the installation and loads no programs from it, so they are left out; a line
/// with nothing else is skipped.
const IGNORED_IDS: [&str; 5] = ["MOD_WRITE", "GAME_WRITE", "DEFAULT_WRITE_PATH", "GAMEBIN", "EXECUTABLE_PATH"];

/// Reads `GameInfo/FileSystem/SearchPaths`, in order. `base` is the installation folder
/// (`|all_source_engine_paths|` and plain relative locations), `gameinfo_dir` the folder of
/// `gameinfo.txt` (`|gameinfo_path|`).
pub fn search_paths(gameinfo: &Document, base: &Path, gameinfo_dir: &Path) -> Vec<SearchPath> {
    let Some(lines) = gameinfo.root.block("GameInfo").and_then(|g| g.block("FileSystem")).and_then(|f| f.block("SearchPaths")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for pair in lines {
        let Value::Str(location) = &pair.value else { continue };
        let ids: Vec<String> = pair
            .key
            .split('+')
            .map(|id| id.trim().to_ascii_uppercase())
            .filter(|id| !id.is_empty() && !IGNORED_IDS.contains(&id.as_str()))
            .collect();
        if ids.is_empty() {
            continue;
        }
        out.push(SearchPath { ids, target: resolve(location, base, gameinfo_dir), line: pair.line });
    }
    out
}

fn strip_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    (s.len() >= prefix.len() && s.is_char_boundary(prefix.len()) && s[..prefix.len()].eq_ignore_ascii_case(prefix))
        .then(|| &s[prefix.len()..])
}

/// `root` joined with a `/`-separated relative path, without `.` and empty components.
fn join(root: &Path, relative: &str) -> PathBuf {
    let mut out = root.to_path_buf();
    for part in relative.split('/').filter(|p| !p.is_empty() && *p != ".") {
        out.push(part);
    }
    out
}

fn resolve(location: &str, base: &Path, gameinfo_dir: &Path) -> Target {
    let location = location.trim().replace('\\', "/");
    let (root, rest) = if let Some(rest) = strip_prefix_ignore_case(&location, "|gameinfo_path|") {
        (gameinfo_dir, rest)
    } else if let Some(rest) = strip_prefix_ignore_case(&location, "|all_source_engine_paths|") {
        (base, rest)
    } else {
        (base, location.as_str())
    };
    if rest == "*" || rest.ends_with("/*") {
        return Target::EachDir(join(root, rest.trim_end_matches('*')));
    }
    if rest.to_ascii_lowercase().ends_with(".vpk") {
        let stem = &rest[..rest.len() - 4];
        let dir_file = join(root, &format!("{stem}_dir.vpk"));
        let single = join(root, rest);
        return Target::Vpk(if dir_file.exists() || !single.exists() { dir_file } else { single });
    }
    Target::Dir(join(root, rest))
}

/// The game folders of `cfg/mount.cfg` (`"mountcfg" { "cstrike" "C:\...\cstrike" }`): name
/// and folder, in file order.
pub fn mount_cfg(doc: &Document) -> Vec<(String, PathBuf)> {
    let Some(block) = doc.root.block("mountcfg") else { return Vec::new() };
    block
        .iter()
        .filter_map(|pair| Some((pair.key.clone(), PathBuf::from(pair.value.as_str()?.trim()))))
        .filter(|(_, p)| !p.as_os_str().is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn search_path_lines() {
        let tmp = TempDir::new();
        let base = tmp.path();
        let game = base.join("garrysmod");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::write(game.join("garrysmod_dir.vpk"), b"").unwrap();
        std::fs::create_dir_all(base.join("sourceengine")).unwrap();
        std::fs::write(base.join("sourceengine/single.vpk"), b"").unwrap();
        let text = r#""GameInfo"
{
	game	"Garry's Mod"
	FileSystem
	{
		SteamAppId	4000
		SearchPaths
		{
			game+mod			garrysmod/addons/*
			game+mod			garrysmod/garrysmod.vpk
			game				|all_source_engine_paths|sourceengine/single.vpk
			game				|ALL_SOURCE_ENGINE_PATHS|sourceengine\hl2_misc.vpk
			mod+mod_write+default_write_path		|gameinfo_path|.
			gamebin				garrysmod/bin
			game+download		garrysmod/download
		}
	}
}
"#;
        let doc = gmod_kv::parse(text).unwrap();
        let paths = search_paths(&doc, base, &game);
        let summary: Vec<(String, Target)> = paths.iter().map(|p| (p.ids.join("+"), p.target.clone())).collect();
        assert_eq!(
            summary,
            [
                ("GAME+MOD".to_string(), Target::EachDir(game.join("addons"))),
                ("GAME+MOD".to_string(), Target::Vpk(game.join("garrysmod_dir.vpk"))),
                ("GAME".to_string(), Target::Vpk(base.join("sourceengine/single.vpk"))),
                ("GAME".to_string(), Target::Vpk(base.join("sourceengine/hl2_misc_dir.vpk"))),
                ("MOD".to_string(), Target::Dir(game.clone())),
                ("GAME+DOWNLOAD".to_string(), Target::Dir(game.join("download"))),
            ]
        );
        assert_eq!(paths[1].line, 10);
    }

    #[test]
    fn mount_cfg_folders() {
        let text = "// comment\n\"mountcfg\"\n{\n//\t\"cstrike\"\t\"C:\\old\"\n\t\"cstrike\"\t\"C:\\steamcmd\\cstrike\"\n\t\"tf\" \"/srv/tf\"\n}\n";
        let doc = gmod_kv::parse(text).unwrap();
        assert_eq!(
            mount_cfg(&doc),
            [("cstrike".to_string(), PathBuf::from(r"C:\steamcmd\cstrike")), ("tf".to_string(), PathBuf::from("/srv/tf"))]
        );
    }
}
