//! The coverage check: every file of the game file system that openGMOD already has a reader
//! for is read, and what fails is listed.

use std::time::Instant;

use gmod_fs::{Backing, Vfs, path};
use gmod_kv::Options;

/// How a text file is read as KeyValues, if it is one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyValuesKind {
    Plain,
    /// Localisation files write quotes as `\"`.
    Escaped,
}

/// Which files are KeyValues. Some `.txt` files in `scripts/` have formats of their own
/// (HUD animations, sentences, response rules, DSP presets) and are left out.
fn keyvalues_kind(path: &str) -> Option<KeyValuesKind> {
    let (dir, name) = path::split(path);
    match path::extension(path) {
        "vmt" | "res" | "vdf" | "acf" => Some(KeyValuesKind::Plain),
        "txt" if path == "gameinfo.txt" => Some(KeyValuesKind::Plain),
        "txt" if dir == "resource" || dir.starts_with("resource/") => Some(KeyValuesKind::Escaped),
        "txt" if dir == "scripts" || dir.starts_with("scripts/") => {
            let own_format = name.starts_with("sentences")
                || (name.starts_with("hudanimations") && name != "hudanimations_manifest.txt")
                || name == "dsp_presets.txt"
                || dir.starts_with("scripts/talker");
            (!own_format).then_some(KeyValuesKind::Plain)
        }
        "txt" if dir.starts_with("settings/") => Some(KeyValuesKind::Plain),
        _ => None,
    }
}

/// Successes and failures of one kind of check.
#[derive(Debug, Default)]
pub struct Tally {
    pub ok: usize,
    pub failed: Vec<String>,
}

#[derive(Debug, Default)]
pub struct Report {
    pub files: usize,
    pub keyvalues: Tally,
    pub lua: Tally,
    /// Lua files that use GMod's additions to the language.
    pub glua: usize,
    pub unreadable: Vec<String>,
    pub crc: Option<Tally>,
    pub seconds: f64,
}

impl Report {
    pub fn failures(&self) -> usize {
        self.keyvalues.failed.len() + self.lua.failed.len() + self.unreadable.len() + self.crc.as_ref().map_or(0, |t| t.failed.len())
    }

    pub fn print(&self, all: bool) {
        println!("Checked {} files in {:.1} s:", self.files, self.seconds);
        println!(
            "  KeyValues  {:>8} read   {:>6} failed   (materials, scripts, VGUI resources, localisation)",
            self.keyvalues.ok,
            self.keyvalues.failed.len()
        );
        println!(
            "  Lua        {:>8} read   {:>6} failed   ({} use GMod's additions to Lua)",
            self.lua.ok,
            self.lua.failed.len(),
            self.glua
        );
        if let Some(crc) = &self.crc {
            println!("  CRC-32     {:>8} match  {:>6} differ   (archive entries)", crc.ok, crc.failed.len());
        }
        if !self.unreadable.is_empty() {
            println!("  unreadable {:>8}", self.unreadable.len());
        }
        let mut lists: Vec<(&str, &Vec<String>)> =
            vec![("KeyValues", &self.keyvalues.failed), ("Lua", &self.lua.failed), ("Unreadable", &self.unreadable)];
        if let Some(crc) = &self.crc {
            lists.push(("CRC-32", &crc.failed));
        }
        for (what, failed) in lists {
            if failed.is_empty() {
                continue;
            }
            println!();
            println!("{what} failures ({}):", failed.len());
            let shown = if all { failed.len() } else { failed.len().min(20) };
            for line in &failed[..shown] {
                println!("  {line}");
            }
            if shown < failed.len() {
                println!("  ... {} more (--all shows them)", failed.len() - shown);
            }
        }
        println!();
    }
}

fn mount_name(vfs: &Vfs, index: usize) -> String {
    let mount = &vfs.mounts()[index];
    let origin = mount.origin();
    let name = origin.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| origin.display().to_string());
    format!("{} {name}", mount.kind())
}

/// Reads every file openGMOD has a reader for.
pub fn run(vfs: &Vfs, crc: bool) -> Report {
    let started = Instant::now();
    let mut report = Report::default();
    let files = vfs.files("");
    report.files = files.len();
    for (path, index) in &files {
        let kv = keyvalues_kind(path);
        let lua = path::extension(path) == "lua";
        if kv.is_none() && !lua {
            continue;
        }
        let where_ = || format!("{path} [{}]", mount_name(vfs, *index));
        let bytes = match vfs.mounts()[*index].read(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                report.unreadable.push(format!("{}: {e}", where_()));
                continue;
            }
        };
        if let Some(kind) = kv {
            let text = gmod_kv::decode_text(&bytes);
            let (_, errors) = gmod_kv::parse_lenient(&text, Options { escapes: kind == KeyValuesKind::Escaped });
            match errors.first() {
                None => report.keyvalues.ok += 1,
                Some(first) => {
                    let more = if errors.len() > 1 { format!(" (+{} more)", errors.len() - 1) } else { String::new() };
                    report.keyvalues.failed.push(format!("{}: {first}{more}", where_()));
                }
            }
        } else {
            match gmod_glua::translate(&bytes) {
                Ok(translated) => {
                    report.lua.ok += 1;
                    if matches!(translated, std::borrow::Cow::Owned(_)) {
                        report.glua += 1;
                    }
                }
                Err(e) => report.lua.failed.push(format!("{}: {e}", where_())),
            }
        }
    }
    if crc {
        report.crc = Some(verify_archives(vfs));
    }
    report.seconds = started.elapsed().as_secs_f64();
    report
}

fn verify_archives(vfs: &Vfs) -> Tally {
    let mut tally = Tally::default();
    for (index, mount) in vfs.mounts().iter().enumerate() {
        let mut check = |path: &str, result: std::io::Result<bool>| match result {
            Ok(true) => tally.ok += 1,
            Ok(false) => tally.failed.push(format!("{path} [{}]: CRC-32 differs", mount_name(vfs, index))),
            Err(e) => tally.failed.push(format!("{path} [{}]: {e}", mount_name(vfs, index))),
        };
        match &mount.backing {
            Backing::Dir(_) => {}
            Backing::Vpk(vpk) => vpk.entries().iter().for_each(|e| check(&e.path, vpk.verify(e))),
            Backing::Gma(gma) => gma.entries().iter().for_each(|e| check(&e.path, gma.verify(e))),
        }
    }
    tally
}

#[cfg(test)]
mod tests {
    use super::*;
    use gmod_fs::gma::{self, GmaInfo};
    use gmod_fs::install::Install;
    use gmod_fs::mount::{MountOptions, mount_install};
    use gmod_fs::vpk;
    use std::path::{Path, PathBuf};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> TempDir {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
            let path = std::env::temp_dir().join(format!("gmod-check-test-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(path: &Path, data: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, data).unwrap();
    }

    #[test]
    fn classifies_text_files() {
        assert_eq!(keyvalues_kind("materials/models/a.vmt"), Some(KeyValuesKind::Plain));
        assert_eq!(keyvalues_kind("resource/ui/menu.res"), Some(KeyValuesKind::Plain));
        assert_eq!(keyvalues_kind("resource/language/hl2_english.txt"), Some(KeyValuesKind::Escaped));
        assert_eq!(keyvalues_kind("scripts/weapons/weapon_pistol.txt"), Some(KeyValuesKind::Plain));
        assert_eq!(keyvalues_kind("scripts/hudanimations_manifest.txt"), Some(KeyValuesKind::Plain));
        assert_eq!(keyvalues_kind("scripts/hudanimations.txt"), None);
        assert_eq!(keyvalues_kind("scripts/sentences.txt"), None);
        assert_eq!(keyvalues_kind("scripts/talker/npc_alyx.txt"), None);
        assert_eq!(keyvalues_kind("gameinfo.txt"), Some(KeyValuesKind::Plain));
        assert_eq!(keyvalues_kind("data/save.txt"), None);
        assert_eq!(keyvalues_kind("lua/autorun/a.lua"), None);
    }

    /// A small installation with good and broken files in a pack, a folder and an addon.
    #[test]
    fn checks_an_installation() {
        let tmp = TempDir::new();
        let root = tmp.0.join("Library/steamapps/common/GarrysMod");
        let game = root.join("garrysmod");
        write(
            &game.join("gameinfo.txt"),
            b"\"GameInfo\" { FileSystem { SearchPaths { game+mod garrysmod/addons/* \n game+mod garrysmod/garrysmod.vpk \n game garrysmod } } }",
        );
        vpk::write_single(
            &game.join("garrysmod_dir.vpk"),
            &[
                ("materials/ok.vmt", b"\"LightmappedGeneric\" { \"$basetexture\" \"concrete\\floor\" }"),
                ("lua/includes/init.lua", b"if a != b then end // GLua"),
                ("lua/plain.lua", b"local x = 1"),
                ("resource/language/x_english.txt", b"\"lang\" { \"Tokens\" { \"A\" \"say \\\"hi\\\"\" } }"),
            ],
        )
        .unwrap();
        write(&game.join("materials/broken.vmt"), b"\"VertexLitGeneric\" { \"$basetexture\" ");
        write(&game.join("lua/broken.lua"), b"x = 1 /* never closed");
        write(&game.join("scripts/sentences.txt"), b"NOT KEYVALUES {");
        let workshop = root.parent().unwrap().parent().unwrap().join("workshop/content/4000/42");
        std::fs::create_dir_all(&workshop).unwrap();
        gma::write(
            &workshop.join("addon.gma"),
            &GmaInfo { title: "Addon".into(), ..Default::default() },
            &[("lua/autorun/addon.lua", b"for i=1,2 do if i then continue end end")],
        )
        .unwrap();

        let install = Install::at(&root).unwrap();
        let (vfs, warnings) = mount_install(&install, &MountOptions::default());
        assert!(warnings.is_empty(), "{warnings:?}");
        let report = run(&vfs, true);
        assert_eq!(report.keyvalues.ok, 3, "{:?}", report.keyvalues.failed);
        assert_eq!(report.keyvalues.failed.len(), 1);
        assert!(report.keyvalues.failed[0].starts_with("materials/broken.vmt [folder garrysmod]"), "{}", report.keyvalues.failed[0]);
        assert_eq!(report.lua.ok, 3);
        assert_eq!(report.glua, 2);
        assert_eq!(report.lua.failed.len(), 1);
        assert!(report.lua.failed[0].contains("lua/broken.lua"));
        let crc = report.crc.as_ref().unwrap();
        assert_eq!((crc.ok, crc.failed.len()), (5, 0));
        assert_eq!(report.failures(), 2);
    }
}
