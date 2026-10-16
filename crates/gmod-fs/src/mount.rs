//! The file system Garry's Mod builds when it starts: what is mounted, and in which order.
//!
//! 1. The `SearchPaths` of `garrysmod/gameinfo.txt`, in order. Its first line mounts every
//!    folder in `garrysmod/addons` (legacy addons); the game's own packs and folders follow.
//! 2. Addon files - subscribed workshop items and `.gma` files in `garrysmod/addons` - right
//!    after the legacy addon folders, in front of the game's own content, so that addons can
//!    replace it. (Their order among each other, and against legacy folders, is still to be
//!    checked against the original.)
//! 3. The game folders of `cfg/mount.cfg`, last.
//!
//! Search path IDs: what `gameinfo.txt` gives (`GAME`, `MOD`, `PLATFORM`, `DOWNLOAD`),
//! `WORKSHOP` for addon files, and the name from `mount.cfg` for a mounted game.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gmod_kv::{Document, Options};

use crate::dir::DirMount;
use crate::gameinfo::{self, Target};
use crate::gma::GmaArchive;
use crate::install::Install;
use crate::user::is_inside;
use crate::vfs::{Backing, Mount, Vfs};
use crate::vpk::VpkArchive;
use crate::workshop::{self, AddonFile, Origin};

/// What to mount besides the game itself.
#[derive(Debug, Clone)]
pub struct MountOptions {
    /// Subscribed workshop items and `.gma` files in `garrysmod/addons`.
    pub addons: bool,
    /// Workshop addons that servers made GMod download (`garrysmod/cache/workshop`). GMod
    /// mounts these only while playing on that server.
    pub server_downloads: bool,
    /// The game folders of `cfg/mount.cfg`.
    pub mount_cfg: bool,
    /// Where old compressed workshop downloads (`.bin`) are unpacked: a folder of openGMOD's
    /// own (never inside the installation). Without one they are not mounted.
    pub unpack_dir: Option<PathBuf>,
    /// Addons switched off, by [`workshop::addon_key`]: not mounted.
    pub disabled: HashSet<String>,
    /// The workshop items the player is subscribed to, as Steam lists them; the others of
    /// Steam's workshop folder are server downloads ([`workshop::addon_files`]). None: all.
    pub subscribed: Option<HashSet<u64>>,
}

impl Default for MountOptions {
    fn default() -> MountOptions {
        MountOptions {
            addons: true,
            server_downloads: false,
            mount_cfg: true,
            unpack_dir: None,
            disabled: HashSet::new(),
            subscribed: None,
        }
    }
}

/// The mount of an addon file: its `.gma`, an old compressed download unpacked first into
/// `unpack_dir` (none: it is not mounted). The addon's title is a search path of its own, as
/// in GMod.
pub fn addon_mount(addon: &AddonFile, unpack_dir: Option<&Path>) -> Result<Mount, String> {
    let path = if addon.packed {
        let dir =
            unpack_dir.ok_or_else(|| format!("{}: compressed download not mounted (no folder to unpack it to)", addon.path.display()))?;
        workshop::unpack_legacy(&addon.path, addon.workshop_id, dir).map_err(|e| e.to_string())?
    } else {
        addon.path.clone()
    };
    let gma = GmaArchive::open(&path).map_err(|e| e.to_string())?;
    let title = gma.info().title.clone();
    let ids: Vec<&str> = if title.is_empty() { vec!["GAME", "WORKSHOP"] } else { vec!["GAME", "WORKSHOP", &title] };
    Ok(Mount::new(&ids, Backing::Gma(gma)).of_addon())
}

/// The file an addon is read from once mounted: its own, or where its old compressed download
/// is unpacked to.
fn addon_origin(addon: &AddonFile, unpack_dir: Option<&Path>) -> Option<PathBuf> {
    if !addon.packed {
        return Some(addon.path.clone());
    }
    let name = match addon.workshop_id {
        Some(id) => format!("{id}.gma"),
        None => format!("{}.gma", addon.path.file_stem()?.to_string_lossy()),
    };
    Some(unpack_dir?.join(name))
}

/// What changed when a running game's addons were brought up to date ([`sync_addons`]).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AddonChanges {
    /// The titles of the addons mounted.
    pub mounted: Vec<String>,
    /// How many were taken out.
    pub unmounted: usize,
    /// What could not be mounted.
    pub warnings: Vec<String>,
}

/// Brings a running game's file system to the addons `options` want, as GMod does when one is
/// switched on or off or has downloaded: those wanted and not mounted are mounted, those
/// mounted and no longer wanted go. Only the player's own addons - subscribed items and
/// `.gma` files of `garrysmod/addons` - come and go; the rest stays as it was mounted.
pub fn sync_addons(vfs: &Vfs, install: &Install, options: &MountOptions) -> AddonChanges {
    let mut changes = AddonChanges::default();
    let unpack_dir = options.unpack_dir.as_deref().filter(|dir| !is_inside(dir, &install.root));
    let own: Vec<AddonFile> = workshop::addon_files(install, options.subscribed.as_ref())
        .into_iter()
        .filter(|a| matches!(a.origin, Origin::Subscribed | Origin::AddonsFolder))
        .collect();
    let wanted = |a: &AddonFile| options.addons && !options.disabled.contains(&workshop::addon_key(a));
    // every file of the player's addons there is, and those to stay
    let mut keep = HashSet::new();
    for addon in &own {
        if wanted(addon)
            && let Some(origin) = addon_origin(addon, unpack_dir)
        {
            keep.insert(origin);
        }
    }
    // addons that are no longer there at all (unsubscribed) go too: any `.gma` addon mount
    // under the workshop or addons folders not kept
    let managed = |origin: &Path| {
        install.workshop_dir().is_some_and(|dir| is_inside(origin, &dir))
            || is_inside(origin, &install.game_dir().join("addons"))
            || unpack_dir.is_some_and(|dir| is_inside(origin, dir))
    };
    for mount in vfs.mounts().iter() {
        let origin = mount.origin();
        if mount.addon && matches!(mount.backing, Backing::Gma(_)) && managed(origin) && !keep.contains(origin) && vfs.unmount(origin) {
            changes.unmounted += 1;
        }
    }
    for addon in own.iter().filter(|a| wanted(a)) {
        if addon_origin(addon, unpack_dir).is_some_and(|origin| vfs.is_mounted(&origin)) {
            continue;
        }
        match addon_mount(addon, unpack_dir) {
            Ok(mount) => {
                let title = match &mount.backing {
                    Backing::Gma(gma) => gma.info().title.clone(),
                    _ => String::new(),
                };
                vfs.mount_addon(mount);
                changes.mounted.push(title);
            }
            Err(e) => changes.warnings.push(e),
        }
    }
    changes
}

fn read_kv(path: &Path, warnings: &mut Vec<String>) -> Option<Document> {
    let bytes = std::fs::read(path).ok()?;
    let (doc, errors) = gmod_kv::parse_lenient(&gmod_kv::decode_text(&bytes), Options::default());
    warnings.extend(errors.into_iter().map(|e| format!("{}: {e}", path.display())));
    Some(doc)
}

/// Builds the file system of an installation. Returns it with a note for everything that
/// could not be mounted.
pub fn mount_install(install: &Install, options: &MountOptions) -> (Vfs, Vec<String>) {
    let mut warnings = Vec::new();
    let mut vfs = Vfs::new();
    let game_dir = install.game_dir();
    let gameinfo_path = game_dir.join("gameinfo.txt");

    let mut search_paths = match read_kv(&gameinfo_path, &mut warnings) {
        Some(doc) => gameinfo::search_paths(&doc, &install.root, &game_dir),
        None => {
            warnings.push(format!("{}: cannot be read", gameinfo_path.display()));
            Vec::new()
        }
    };
    if search_paths.is_empty() {
        warnings.push("gameinfo.txt has no SearchPaths: mounting the game's packs and folder only".into());
        search_paths = vec![
            gameinfo::SearchPath {
                ids: vec!["GAME".into(), "MOD".into()],
                target: Target::Vpk(game_dir.join("garrysmod_dir.vpk")),
                line: 0,
            },
            gameinfo::SearchPath { ids: vec!["GAME".into(), "MOD".into()], target: Target::Dir(game_dir.clone()), line: 0 },
        ];
    }

    // where addon files go: after the legacy addon folders at the top
    let mut addon_slot = None;
    for search_path in &search_paths {
        let ids: Vec<&str> = search_path.ids.iter().map(String::as_str).collect();
        if addon_slot.is_none() && !matches!(search_path.target, Target::EachDir(_)) {
            addon_slot = Some(vfs.mounts().len());
        }
        match &search_path.target {
            Target::EachDir(dir) => {
                let mut folders: Vec<PathBuf> =
                    std::fs::read_dir(dir).into_iter().flatten().filter_map(Result::ok).map(|e| e.path()).filter(|p| p.is_dir()).collect();
                folders.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
                for folder in folders {
                    vfs.push(Mount::new(&ids, Backing::Dir(DirMount::new(folder))).of_addon());
                }
            }
            Target::Vpk(path) => match VpkArchive::open(path) {
                Ok(vpk) => vfs.push(Mount::new(&ids, Backing::Vpk(vpk))),
                Err(e) => warnings.push(format!("gameinfo.txt line {}: {e}", search_path.line)),
            },
            Target::Dir(dir) => {
                if dir.is_dir() {
                    vfs.push(Mount::new(&ids, Backing::Dir(DirMount::new(dir))));
                } else if !dir.ends_with("download") {
                    // `garrysmod/download` only exists once a server sent something
                    warnings.push(format!("gameinfo.txt line {}: {} is not a folder", search_path.line, dir.display()));
                }
            }
        }
    }

    if options.addons || options.server_downloads {
        let unpack_dir = options.unpack_dir.as_deref().filter(|dir| {
            let inside = is_inside(dir, &install.root);
            if inside {
                warnings.push(format!("{} is inside the installation: old compressed downloads are not unpacked", dir.display()));
            }
            !inside
        });
        let mut slot = addon_slot.unwrap_or(vfs.mounts().len());
        for addon in workshop::addon_files(install, options.subscribed.as_ref()) {
            let wanted = match addon.origin {
                Origin::Subscribed | Origin::AddonsFolder => options.addons,
                Origin::ServerDownload => options.server_downloads,
            };
            if !wanted || options.disabled.contains(&workshop::addon_key(&addon)) {
                continue;
            }
            match addon_mount(&addon, unpack_dir) {
                Ok(mount) => {
                    vfs.insert(slot, mount);
                    slot += 1;
                }
                Err(e) => warnings.push(e),
            }
        }
    }

    if options.mount_cfg
        && let Some(doc) = read_kv(&game_dir.join("cfg/mount.cfg"), &mut warnings)
    {
        for (name, folder) in gameinfo::mount_cfg(&doc) {
            if !folder.is_dir() {
                warnings.push(format!("mount.cfg: {name}: {} is not a folder", folder.display()));
                continue;
            }
            let ids = ["GAME", name.as_str()];
            let mut packs: Vec<PathBuf> = std::fs::read_dir(&folder)
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().to_ascii_lowercase().ends_with("_dir.vpk")))
                .collect();
            packs.sort();
            for pack in packs {
                match VpkArchive::open(&pack) {
                    Ok(vpk) => vfs.push(Mount::new(&ids, Backing::Vpk(vpk))),
                    Err(e) => warnings.push(format!("mount.cfg: {e}")),
                }
            }
            vfs.push(Mount::new(&ids, Backing::Dir(DirMount::new(folder))));
        }
    }

    if let Some(slot) = addon_slot {
        vfs.set_addon_slot(slot);
    }
    (vfs, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gma::{self, GmaInfo};
    use crate::testutil::{TempDir, fake_install};
    use crate::vpk;

    const GAMEINFO: &str = r#""GameInfo"
{
	FileSystem
	{
		SearchPaths
		{
			game+mod			garrysmod/addons/*
			game+mod			garrysmod/garrysmod.vpk
			game				|all_source_engine_paths|sourceengine/hl2_misc.vpk
			mod+mod_write+default_write_path		|gameinfo_path|.
			game+game_write		garrysmod
			gamebin				garrysmod/bin
			game				|all_source_engine_paths|sourceengine
			game+download		garrysmod/download
		}
	}
}
"#;

    #[test]
    fn addons_come_and_go_while_the_game_runs() {
        let tmp = TempDir::new();
        let root = tmp.path().join("Library/steamapps/common/GarrysMod");
        fake_install(&root);
        let game = root.join("garrysmod");
        std::fs::write(game.join("gameinfo.txt"), GAMEINFO).unwrap();
        vpk::write_single(&game.join("garrysmod_dir.vpk"), &[("materials/x.vmt", b"the game's")]).unwrap();
        std::fs::create_dir_all(game.join("addons")).unwrap();
        let addon = game.join("addons/later.gma");
        gma::write(&addon, &GmaInfo { title: "Later".into(), ..Default::default() }, &[("materials/x.vmt", b"the addon's")]).unwrap();
        let install = Install::at(&root).unwrap();
        // switched off when the game started
        let off = MountOptions { disabled: HashSet::from(["later.gma".to_string()]), ..MountOptions::default() };
        let (vfs, _) = mount_install(&install, &off);
        assert_eq!(vfs.read("materials/x.vmt", "GAME").unwrap(), b"the game's");
        // switched on: there at once, in front of the game's own
        let changes = sync_addons(&vfs, &install, &MountOptions::default());
        assert_eq!(changes.mounted, ["Later"]);
        assert_eq!(vfs.read("materials/x.vmt", "GAME").unwrap(), b"the addon's");
        assert!(vfs.find("materials/*", "Later").files.contains(&"x.vmt".to_string()));
        assert_eq!(sync_addons(&vfs, &install, &MountOptions::default()), AddonChanges::default(), "once");
        // and off again
        let changes = sync_addons(&vfs, &install, &off);
        assert_eq!(changes.unmounted, 1);
        assert_eq!(vfs.read("materials/x.vmt", "GAME").unwrap(), b"the game's");
    }

    #[test]
    fn mounts_like_gmod() {
        let tmp = TempDir::new();
        let library = tmp.path().join("Library");
        let root = library.join("steamapps/common/GarrysMod");
        fake_install(&root);
        let game = root.join("garrysmod");
        std::fs::write(game.join("gameinfo.txt"), GAMEINFO).unwrap();
        vpk::write_single(
            &game.join("garrysmod_dir.vpk"),
            &[("lua/includes/init.lua", b"base"), ("materials/x.vmt", b"from garrysmod.vpk")],
        )
        .unwrap();
        std::fs::create_dir_all(root.join("sourceengine")).unwrap();
        vpk::write_single(&root.join("sourceengine/hl2_misc_dir.vpk"), &[("models/hl2.mdl", b"hl2"), ("materials/x.vmt", b"from hl2")])
            .unwrap();
        std::fs::create_dir_all(game.join("addons/LegacyAddon/lua/autorun")).unwrap();
        std::fs::write(game.join("addons/LegacyAddon/lua/autorun/legacy.lua"), b"legacy folder").unwrap();
        std::fs::create_dir_all(game.join("cfg")).unwrap();
        let other_game = tmp.path().join("cstrike");
        std::fs::create_dir_all(&other_game).unwrap();
        vpk::write_single(&other_game.join("cstrike_pak_dir.vpk"), &[("models/css.mdl", b"css")]).unwrap();
        std::fs::write(game.join("cfg/mount.cfg"), format!("\"mountcfg\" {{ \"cstrike\" \"{}\" }}", other_game.display())).unwrap();

        let install = Install::at(&root).unwrap();
        let workshop = install.workshop_dir().unwrap();
        std::fs::create_dir_all(workshop.join("123")).unwrap();
        gma::write(
            &workshop.join("123/addon.gma"),
            &GmaInfo { title: "Workshop Addon".into(), ..Default::default() },
            &[("materials/x.vmt", b"from the addon"), ("lua/autorun/ws.lua", b"ws")],
        )
        .unwrap();
        std::fs::write(workshop.join("456_legacy.bin"), b"ignored: not in an item folder").unwrap();

        let (vfs, warnings) = mount_install(&install, &MountOptions::default());
        assert!(warnings.is_empty(), "{warnings:?}");
        let order: Vec<String> =
            vfs.mounts().iter().map(|m| format!("{} {}", m.kind(), m.origin().file_name().unwrap().to_string_lossy())).collect();
        assert_eq!(
            order,
            [
                "folder LegacyAddon",
                "gma addon.gma",
                "vpk garrysmod_dir.vpk",
                "vpk hl2_misc_dir.vpk",
                "folder garrysmod",
                "folder garrysmod",
                "folder sourceengine",
                "vpk cstrike_pak_dir.vpk",
                "folder cstrike",
            ]
        );
        assert_eq!(vfs.mounts()[4].ids, ["MOD"], "write-only IDs are dropped");
        assert_eq!(vfs.read("materials/x.vmt", "GAME").unwrap(), b"from the addon");
        assert_eq!(vfs.read("materials/x.vmt", "MOD").unwrap(), b"from garrysmod.vpk");
        assert_eq!(vfs.read("lua/autorun/legacy.lua", "GAME").unwrap(), b"legacy folder");
        assert_eq!(vfs.read("models/css.mdl", "cstrike").unwrap(), b"css");
        assert_eq!(vfs.find("lua/autorun/*", "GAME").files, ["legacy.lua", "ws.lua"]);
        assert_eq!(vfs.find("lua/autorun/*", "WORKSHOP").files, ["ws.lua"]);
        // an addon's title is a search path too
        assert_eq!(vfs.find("lua/autorun/*", "Workshop Addon").files, ["ws.lua"]);

        // an addon switched off is not mounted
        let disabled = ["123".to_string()].into_iter().collect();
        let (vfs, _) = mount_install(&install, &MountOptions { disabled, ..MountOptions::default() });
        assert_eq!(vfs.read("materials/x.vmt", "GAME").unwrap(), b"from garrysmod.vpk");

        let (vfs, _) = mount_install(&install, &MountOptions { addons: false, mount_cfg: false, ..MountOptions::default() });
        assert_eq!(vfs.mounts().len(), 6);
        assert_eq!(vfs.read("materials/x.vmt", "GAME").unwrap(), b"from garrysmod.vpk");
    }

    #[test]
    fn packed_downloads_need_a_folder_outside() {
        let tmp = TempDir::new();
        let root = tmp.path().join("Library/steamapps/common/GarrysMod");
        fake_install(&root);
        let install = Install::at(&root).unwrap();
        let item = install.workshop_dir().unwrap().join("789");
        std::fs::create_dir_all(&item).unwrap();
        std::fs::write(item.join("old_legacy.bin"), b"").unwrap();

        let (_, warnings) = mount_install(&install, &MountOptions::default());
        assert!(warnings.iter().any(|w| w.contains("no folder to unpack")), "{warnings:?}");
        let inside = MountOptions { unpack_dir: Some(install.game_dir().join("cache/opengmod")), ..MountOptions::default() };
        let (_, warnings) = mount_install(&install, &inside);
        assert!(warnings.iter().any(|w| w.contains("inside the installation")), "{warnings:?}");
        assert!(!install.game_dir().join("cache").exists(), "nothing was written into the installation");
    }
}
