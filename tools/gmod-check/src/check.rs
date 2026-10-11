//! The coverage check: every file of the game file system that openGMOD already has a reader
//! for is read, and what fails is listed.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use gmod_bsp::{Bsp, mesh};
use gmod_fs::{Backing, Mount, Vfs, path};
use gmod_kv::Options;
use gmod_material::{Vtf, load_material_file, material_path, texture_path};

use crate::out::Out;

/// Texture parameters of materials whose textures are looked for.
const TEXTURE_PARAMS: [&str; 5] = ["$basetexture", "$basetexture2", "$bumpmap", "$normalmap", "$detail"];

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
    pub textures: Tally,
    pub materials: Tally,
    pub models: Tally,
    /// Physics models (`.phy`) of the models that read.
    pub physics: Tally,
    /// Models with animations whose every frame reads, and how many frames that was.
    pub animations: Tally,
    pub frames: usize,
    pub maps: Tally,
    /// Maps where a player put at the start stands: what is solid there is read right.
    pub collision: Tally,
    /// Gaps in the content rather than failures of openGMOD: textures that materials name,
    /// materials that models and maps name, models that maps name, which the game does not have.
    pub missing_textures: Vec<String>,
    pub missing_materials: Vec<String>,
    pub missing_models: Vec<String>,
    pub unreadable: Vec<String>,
    pub crc: Option<Tally>,
    pub seconds: f64,
}

impl Report {
    pub fn failures(&self) -> usize {
        [
            &self.keyvalues,
            &self.lua,
            &self.textures,
            &self.materials,
            &self.models,
            &self.physics,
            &self.animations,
            &self.maps,
            &self.collision,
        ]
        .iter()
        .map(|t| t.failed.len())
        .sum::<usize>()
            + self.unreadable.len()
            + self.crc.as_ref().map_or(0, |t| t.failed.len())
    }

    pub fn print(&self, out: &mut Out, all: bool) {
        out.line(format!("Checked {} files in {:.1} s:", self.files, self.seconds));
        out.line(format!(
            "  KeyValues  {:>8} read   {:>6} failed   (materials, scripts, VGUI resources, localisation)",
            self.keyvalues.ok,
            self.keyvalues.failed.len()
        ));
        out.line(format!(
            "  Lua        {:>8} read   {:>6} failed   ({} use GMod's additions to Lua)",
            self.lua.ok,
            self.lua.failed.len(),
            self.glua
        ));
        out.line(format!(
            "  Textures   {:>8} read   {:>6} failed   (VTF, map pakfiles included)",
            self.textures.ok,
            self.textures.failed.len()
        ));
        out.line(format!(
            "  Materials  {:>8} read   {:>6} failed   (VMT, patches followed)",
            self.materials.ok,
            self.materials.failed.len()
        ));
        out.line(format!(
            "  Models     {:>8} read   {:>6} failed   (MDL with VVD and VTX: the triangles of every model)",
            self.models.ok,
            self.models.failed.len()
        ));
        out.line(format!(
            "  Physics    {:>8} read   {:>6} failed   (PHY of those models: convex pieces, masses, ragdoll joints)",
            self.physics.ok,
            self.physics.failed.len()
        ));
        out.line(format!(
            "  Animations {:>8} read   {:>6} failed   (sequences of those models: {} frames, .ani blocks included)",
            self.animations.ok,
            self.animations.failed.len(),
            self.frames
        ));
        out.line(format!(
            "  Maps       {:>8} read   {:>6} failed   (BSP: world, lightmaps, displacements, static props, brushes)",
            self.maps.ok,
            self.maps.failed.len()
        ));
        out.line(format!(
            "  Collision  {:>8} stand  {:>6} failed   (a player put at each map's start stands there)",
            self.collision.ok,
            self.collision.failed.len()
        ));
        if let Some(crc) = &self.crc {
            out.line(format!("  CRC-32     {:>8} match  {:>6} differ   (archive entries)", crc.ok, crc.failed.len()));
        }
        if !self.unreadable.is_empty() {
            out.line(format!("  unreadable {:>8}", self.unreadable.len()));
        }
        let missing = self.missing_textures.len() + self.missing_materials.len() + self.missing_models.len();
        if missing > 0 {
            out.line(format!(
                "  Missing: {} textures named by materials, {} materials named by models and maps, {} models named by maps and models",
                self.missing_textures.len(),
                self.missing_materials.len(),
                self.missing_models.len()
            ));
            out.line("  (gaps in the content, not failures: the game shows these as errors too)");
        }
        let mut lists: Vec<(&str, &Vec<String>)> = vec![
            ("KeyValues failures", &self.keyvalues.failed),
            ("Lua failures", &self.lua.failed),
            ("Texture failures", &self.textures.failed),
            ("Material failures", &self.materials.failed),
            ("Model failures", &self.models.failed),
            ("Physics failures", &self.physics.failed),
            ("Animation failures", &self.animations.failed),
            ("Map failures", &self.maps.failed),
            ("Collision failures", &self.collision.failed),
            ("Unreadable", &self.unreadable),
        ];
        if let Some(crc) = &self.crc {
            lists.push(("CRC-32 failures", &crc.failed));
        }
        lists.push(("Missing textures", &self.missing_textures));
        lists.push(("Missing materials", &self.missing_materials));
        lists.push(("Missing models", &self.missing_models));
        for (what, failed) in lists {
            if failed.is_empty() {
                continue;
            }
            out.line("");
            out.line(format!("{what} ({}):", failed.len()));
            let shown = if all { failed.len() } else { failed.len().min(20) };
            for line in &failed[..shown] {
                out.line(format!("  {line}"));
            }
            if shown < failed.len() {
                out.line(format!("  ... {} more (--all shows them)", failed.len() - shown));
            }
        }
        out.line("");
    }
}

fn mount_name(vfs: &Vfs, index: usize) -> String {
    let mount = &vfs.mounts()[index];
    let origin = mount.origin();
    let name = origin.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| origin.display().to_string());
    format!("{} {name}", mount.kind())
}

/// Reads every file openGMOD has a reader for. A map's pakfile is mounted in front of the
/// others while its materials are looked up, as when the map is played.
pub fn run(vfs: &mut Vfs, crc: bool) -> Report {
    let started = Instant::now();
    let mut report = Report::default();
    let files = vfs.files("");
    report.files = files.len();
    for (path, index) in &files {
        let extension = path::extension(path);
        let kv = keyvalues_kind(path);
        let lua = extension == "lua";
        let texture = extension == "vtf";
        let map = extension == "bsp" && path.starts_with("maps/");
        if extension == "mdl" {
            check_model(vfs, path, &format!("{path} [{}]", mount_name(vfs, *index)), &mut report);
            continue;
        }
        if kv.is_none() && !lua && !texture && !map {
            continue;
        }
        let where_ = format!("{path} [{}]", mount_name(vfs, *index));
        let bytes = match vfs.mounts()[*index].read(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                report.unreadable.push(format!("{where_}: {e}"));
                continue;
            }
        };
        if texture {
            count(&mut report.textures, &where_, check_texture(bytes));
        } else if map {
            check_map(vfs, path, &where_, bytes, &mut report);
        } else if let Some(kind) = kv {
            let text = gmod_kv::decode_text(&bytes);
            let (_, errors) = gmod_kv::parse_lenient(&text, Options { escapes: kind == KeyValuesKind::Escaped });
            match errors.first() {
                None => report.keyvalues.ok += 1,
                Some(first) => {
                    let more = if errors.len() > 1 { format!(" (+{} more)", errors.len() - 1) } else { String::new() };
                    report.keyvalues.failed.push(format!("{where_}: {first}{more}"));
                }
            }
            if extension == "vmt" && path.starts_with("materials/") {
                check_material(vfs, path, &where_, &mut report);
            }
        } else {
            match gmod_glua::translate(&bytes) {
                Ok(translated) => {
                    report.lua.ok += 1;
                    if matches!(translated, std::borrow::Cow::Owned(_)) {
                        report.glua += 1;
                    }
                }
                Err(e) => report.lua.failed.push(format!("{where_}: {e}")),
            }
        }
    }
    if crc {
        report.crc = Some(verify_archives(vfs));
    }
    report.seconds = started.elapsed().as_secs_f64();
    report
}

fn count(tally: &mut Tally, where_: &str, result: Result<(), String>) {
    match result {
        Ok(()) => tally.ok += 1,
        Err(e) => tally.failed.push(format!("{where_}: {e}")),
    }
}

/// A texture reads when its sizes add up and its pixel format can be decoded.
fn check_texture(bytes: Vec<u8>) -> Result<(), String> {
    let vtf = Vtf::parse(bytes).map_err(|e| e.to_string())?;
    match vtf.rgba8(vtf.mipmaps - 1, 0, 0, 0) {
        Some(_) => Ok(()),
        None => Err(format!("{vtf:?}: {} pixels cannot be decoded yet", vtf.format.name())),
    }
}

/// A material reads when it and the materials it patches do; its textures should be there. A
/// patch of a material that is not there is a gap in the content, not a failure.
fn check_material(vfs: &Vfs, path: &str, where_: &str, report: &mut Report) {
    match load_material_file(vfs, path, 0) {
        Ok(material) => {
            report.materials.ok += 1;
            for param in TEXTURE_PARAMS {
                let Some(texture) = material.texture(param) else { continue };
                // render targets are made as the game runs
                if texture.starts_with("_rt_") {
                    continue;
                }
                if !texture_path(&texture).is_some_and(|file| vfs.exists(&file, "GAME")) {
                    report.missing_textures.push(format!("{where_}: {param} {texture}"));
                }
            }
        }
        Err(e) => {
            let message = e.to_string();
            let message = message.strip_prefix(&format!("{path}: ")).unwrap_or(&message);
            // a patch of a material the game does not have
            if e.kind() == std::io::ErrorKind::NotFound {
                report.missing_materials.push(format!("{where_}: {message}"));
            } else {
                report.materials.failed.push(format!("{where_}: {message}"));
            }
        }
    }
}

/// A model reads when its three files do and fit together into triangles; its materials should
/// be there. Its physics model, if it has one, should read and belong to it.
fn check_model(vfs: &Vfs, path: &str, where_: &str, report: &mut Report) {
    let model = match gmod_model::load(vfs, path).and_then(|model| model.mesh(0, 0).map(|_| model)) {
        Ok(model) => model,
        Err(e) => {
            let message = e.to_string();
            let message = message.strip_prefix(&format!("{path}: ")).unwrap_or(&message);
            return report.models.failed.push(format!("{where_}: {message}"));
        }
    };
    report.models.ok += 1;
    match gmod_model::load_physics(vfs, path, model.mdl.checksum) {
        Ok(Some(_)) => report.physics.ok += 1,
        Ok(None) => {}
        Err(e) => report.physics.failed.push(format!("{where_}: {e}")),
    }
    // its own animations, every frame (those of the models it includes are checked as those
    // models); the models it includes should be there
    if !model.mdl.animations.is_empty() {
        match gmod_model::Rig::load(vfs, path).and_then(|rig| rig.read_own()) {
            Ok(frames) => {
                report.animations.ok += 1;
                report.frames += frames;
            }
            Err(e) => report.animations.failed.push(format!("{where_}: {e}")),
        }
    }
    for include in &model.mdl.include_models {
        if !vfs.exists(&include.replace('\\', "/"), "GAME") {
            report.missing_models.push(format!("{where_}: includes {include}"));
        }
    }
    for texture in 0..model.mdl.textures.len() {
        let candidates = model.mdl.material_candidates(texture);
        if !candidates.iter().any(|name| material_path(name).is_some_and(|file| vfs.exists(&file, "GAME")))
            && let Some(first) = candidates.first()
        {
            report.missing_materials.push(format!("{where_}: {first}"));
        }
    }
}

/// A map reads when its lumps do and its world can be built; a player put at its start should
/// stand there. Its pakfile is checked as well: the textures, materials and models in it, and
/// whether the materials and models the map names are in the game with the pakfile in front.
fn check_map(vfs: &mut Vfs, path: &str, where_: &str, bytes: Vec<u8>, report: &mut Report) {
    let bsp = match Bsp::parse(PathBuf::from(path), Arc::from(bytes)) {
        Ok(bsp) => bsp,
        Err(e) => return report.maps.failed.push(format!("{where_}: {e}")),
    };
    let world = mesh::build(&bsp, mesh::Options::default());
    if world.broken_faces > 0 {
        return report.maps.failed.push(format!("{where_}: {} faces of the world cannot be built", world.broken_faces));
    }
    let pak = match bsp.pakfile() {
        Ok(pak) => pak,
        Err(e) => return report.maps.failed.push(format!("{where_}: pakfile: {e}")),
    };
    report.maps.ok += 1;
    let mounted = pak.is_some();
    if let Some(pak) = pak {
        let entries: Vec<String> = pak.entries().iter().map(|e| e.path.clone()).collect();
        vfs.insert(0, Mount::new(&["GAME", "BSP"], Backing::Zip(pak)));
        for entry in entries {
            let inner = format!("{path}: {entry}");
            match path::extension(&entry) {
                "vtf" => {
                    count(&mut report.textures, &inner, vfs.mounts()[0].read(&entry).map_err(|e| e.to_string()).and_then(check_texture))
                }
                "vmt" if entry.starts_with("materials/") => check_material(vfs, &entry, &inner, report),
                "mdl" => check_model(vfs, &entry, &inner, report),
                _ => {}
            }
        }
    }
    for name in &bsp.materials {
        if !material_path(name).is_some_and(|file| vfs.exists(&file, "GAME")) {
            report.missing_materials.push(format!("{where_}: {name}"));
        }
    }
    for model in &bsp.static_props.models {
        if !vfs.exists(model, "GAME") {
            report.missing_models.push(format!("{where_}: {model}"));
        }
    }
    // what is solid, the props of the pakfile's models among it
    let (collision, _) = gmod_world::collision(vfs, &bsp);
    if let Some(stands) = crate::trace::stand_at_start(&bsp, &collision) {
        count(&mut report.collision, where_, stands);
    }
    if mounted {
        vfs.remove(0);
    }
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
            Backing::Zip(zip) => zip.entries().iter().for_each(|e| check(&e.path, zip.verify(e))),
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

    /// A 1 x 1 BGRA texture, version 7.2.
    fn texture() -> Vec<u8> {
        let mut v = b"VTF\0".to_vec();
        for x in [7u32, 2, 80] {
            v.extend(x.to_le_bytes());
        }
        v.extend([1, 0, 1, 0]); // 1 x 1
        v.extend([0; 4]); // flags
        v.extend([1, 0, 0, 0]); // one frame, first frame 0
        v.resize(52, 0);
        v.extend(12i32.to_le_bytes()); // BGRA8888
        v.push(1); // one mipmap
        v.extend((-1i32).to_le_bytes()); // no thumbnail
        v.resize(80, 0);
        v.extend([1, 2, 3, 4]);
        v
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
                ("materials/concrete/floor.vtf", &texture()),
                ("lua/includes/init.lua", b"if a != b then end // GLua"),
                ("lua/plain.lua", b"local x = 1"),
                ("resource/language/x_english.txt", b"\"lang\" { \"Tokens\" { \"A\" \"say \\\"hi\\\"\" } }"),
            ],
        )
        .unwrap();
        write(&game.join("materials/broken.vmt"), b"\"VertexLitGeneric\" { \"$basetexture\" ");
        write(&game.join("materials/missing.vmt"), b"\"UnlitGeneric\" { \"$basetexture\" \"nothing/here\" }");
        write(&game.join("materials/broken.vtf"), b"VTF\0 cut short");
        write(&game.join("maps/broken.bsp"), b"VBSP cut short");
        write(&game.join("models/broken.mdl"), b"IDST cut short");
        write(&game.join("lua/broken.lua"), b"x = 1 /* never closed");
        write(&game.join("scripts/sentences.txt"), b"NOT KEYVALUES {");
        // a model with animations, one in a block of its .ani; one that includes a model not
        // there; one whose .ani is not there
        let mut rig = gmod_model::testmodel::TestRig {
            name: "anim.mdl".into(),
            checksum: 3,
            bones: vec![gmod_model::testmodel::TestBone { name: "root".into(), parent: -1, position: [0.0; 3], euler: [0.0; 3] }],
            animations: vec![gmod_model::testmodel::TestAnimation::turning("@a", 0, 5, 0.0, 1.0)],
            sequences: vec![gmod_model::testmodel::TestSequence::new("a", "ACT_IDLE", 0)],
            animation_file: "models/anim.ani".into(),
            ..Default::default()
        };
        rig.animations.push(gmod_model::testmodel::TestAnimation { block: 1, ..rig.animations[0].clone() });
        let (mdl, ani) = rig.files();
        let no_ani = gmod_model::testmodel::TestRig { animation_file: "models/gone.ani".into(), ..rig.clone() }.files().0;
        for (name, data) in [("anim", &mdl), ("noani", &no_ani)] {
            write(&game.join(format!("models/{name}.mdl")), data);
            write(&game.join(format!("models/{name}.vvd")), &gmod_model::testmodel::empty_vvd(3));
            write(&game.join(format!("models/{name}.dx90.vtx")), &gmod_model::testmodel::empty_vtx(3));
        }
        write(&game.join("models/anim.ani"), &ani.unwrap());
        rig.animations.pop();
        rig.include_models = vec!["models/m_anm.mdl".into()];
        write(&game.join("models/player.mdl"), &rig.files().0);
        write(&game.join("models/player.vvd"), &gmod_model::testmodel::empty_vvd(3));
        write(&game.join("models/player.dx90.vtx"), &gmod_model::testmodel::empty_vtx(3));
        let workshop = root.parent().unwrap().parent().unwrap().join("workshop/content/4000/42");
        std::fs::create_dir_all(&workshop).unwrap();
        gma::write(
            &workshop.join("addon.gma"),
            &GmaInfo { title: "Addon".into(), ..Default::default() },
            &[("lua/autorun/addon.lua", b"for i=1,2 do if i then continue end end")],
        )
        .unwrap();

        let install = Install::at(&root).unwrap();
        let (mut vfs, warnings) = mount_install(&install, &MountOptions::default());
        assert!(warnings.is_empty(), "{warnings:?}");
        let report = run(&mut vfs, true);
        assert_eq!(report.keyvalues.ok, 4, "{:?}", report.keyvalues.failed);
        assert_eq!(report.keyvalues.failed.len(), 1);
        assert!(report.keyvalues.failed[0].starts_with("materials/broken.vmt [folder garrysmod]"), "{}", report.keyvalues.failed[0]);
        assert_eq!(report.lua.ok, 3);
        assert_eq!(report.glua, 2);
        assert_eq!(report.lua.failed.len(), 1);
        assert!(report.lua.failed[0].contains("lua/broken.lua"));
        // a broken material still reads, as the engine reads what it can
        assert_eq!((report.materials.ok, report.materials.failed.len()), (3, 0), "{:?}", report.materials.failed);
        assert_eq!(report.missing_textures, ["materials/missing.vmt [folder garrysmod]: $basetexture nothing/here"]);
        assert_eq!(report.textures.ok, 1);
        assert!(report.textures.failed[0].starts_with("materials/broken.vtf [folder garrysmod]: "), "{:?}", report.textures.failed);
        assert_eq!((report.maps.ok, report.maps.failed.len()), (0, 1));
        assert_eq!((report.models.ok, report.models.failed.len()), (3, 1), "{:?}", report.models.failed);
        assert!(report.models.failed[0].starts_with("models/broken.mdl [folder garrysmod]: "), "{:?}", report.models.failed);
        // every frame of the animations, in the model and in its .ani
        assert_eq!((report.animations.ok, report.frames), (2, 5 + 5 + 5), "{:?}", report.animations.failed);
        assert_eq!(report.animations.failed.len(), 1);
        assert!(report.animations.failed[0].starts_with("models/noani.mdl [folder garrysmod]: "), "{:?}", report.animations.failed);
        assert_eq!(report.missing_models, ["models/player.mdl [folder garrysmod]: includes models/m_anm.mdl"]);
        let crc = report.crc.as_ref().unwrap();
        assert_eq!((crc.ok, crc.failed.len()), (6, 0));
        assert_eq!(report.failures(), 6);
    }
}
