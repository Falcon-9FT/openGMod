//! The addons a player has: the workshop items they subscribed to (kept by Steam), `.gma`
//! files put into `garrysmod/addons` by hand, and the workshop addons servers made GMod
//! download (`garrysmod/cache/workshop`).
//!
//! Steam keeps each subscribed item in `steamapps/workshop/content/4000/<id>/`: a `.gma`, or
//! for items published before the current workshop a `<name>_legacy.bin` - an LZMA stream
//! around a GMA. Those are unpacked once into a folder of openGMOD's own and read from
//! there; the downloads themselves are not touched. Steam's list of the items it installed
//! (`appworkshop_4000.acf`) leaves out the folders of items no longer subscribed to; it is
//! written late, so a folder that changed since it was written is kept all the same.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use gmod_kv::Options;

use crate::install::Install;
use crate::io_util::invalid;
use crate::lzma;

/// The largest compressed download read, and the largest addon unpacked from it.
const MAX_PACKED: u64 = 4 << 30;
const MAX_UNPACKED: u64 = 8 << 30;

/// Where an addon file comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Subscribed on the workshop; Steam keeps it.
    Subscribed,
    /// A `.gma` in `garrysmod/addons`.
    AddonsFolder,
    /// Downloaded from a server (`garrysmod/cache/workshop`).
    ServerDownload,
}

/// One addon file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddonFile {
    pub path: PathBuf,
    pub origin: Origin,
    pub workshop_id: Option<u64>,
    /// An old compressed download (`.bin`) that must be unpacked before it can be mounted.
    pub packed: bool,
}

fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case(extension))
}

/// Files in `dir` with one of `extensions`, sorted by name.
fn files_with(dir: &Path, extensions: &[&str]) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && extensions.iter().any(|x| has_extension(p, x)))
        .collect();
    out.sort();
    out
}

/// The addon files of a subscribed item's folder: `.gma` files, old compressed downloads
/// (`.bin`), and files of another name that are GMA files all the same.
fn item_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let is_gma = |path: &Path| {
        let mut magic = [0u8; 4];
        std::fs::File::open(path).and_then(|mut f| io::Read::read_exact(&mut f, &mut magic)).is_ok() && &magic == b"GMAD"
    };
    let mut out: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && (has_extension(p, "gma") || has_extension(p, "bin") || is_gma(p)))
        .collect();
    out.sort();
    out
}

/// The IDs Steam lists as installed in `appworkshop_4000.acf`, if it has that file, and when
/// it was written.
fn installed_ids(library: &Path) -> Option<(HashSet<u64>, SystemTime)> {
    let manifest = library.join(format!("steamapps/workshop/appworkshop_{}.acf", crate::install::APP_ID));
    let written = std::fs::metadata(&manifest).and_then(|m| m.modified()).ok()?;
    let bytes = std::fs::read(manifest).ok()?;
    let (doc, _) = gmod_kv::parse_lenient(&gmod_kv::decode_text(&bytes), Options { escapes: true });
    let installed = doc.root.block("AppWorkshop")?.block("WorkshopItemsInstalled")?;
    Some((installed.iter().filter_map(|pair| pair.key.parse().ok()).collect(), written))
}

/// When a folder or a file in it last changed.
fn last_change(dir: &Path) -> Option<SystemTime> {
    let own = std::fs::metadata(dir).and_then(|m| m.modified()).ok();
    let files = std::fs::read_dir(dir).into_iter().flatten().filter_map(Result::ok).filter_map(|e| e.metadata().ok()?.modified().ok());
    own.into_iter().chain(files).max()
}

/// Every addon file of an installation: subscribed items first (by ID), then the ones in
/// `garrysmod/addons`, then server downloads that are not subscribed too.
pub fn addon_files(install: &Install) -> Vec<AddonFile> {
    let mut out = Vec::new();
    let mut ids = HashSet::new();

    if let (Some(dir), Some(library)) = (install.workshop_dir(), install.library.as_deref()) {
        let listed = installed_ids(library);
        let mut items: Vec<(u64, PathBuf)> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter_map(|e| Some((e.file_name().to_str()?.parse::<u64>().ok()?, e.path())))
            .filter(|(id, path)| {
                // Steam writes its list when it likes, so an item it downloaded since is not
                // in it yet: only one it left out when it last wrote the list is gone
                path.is_dir()
                    && listed.as_ref().is_none_or(|(ids, written)| ids.contains(id) || last_change(path).is_none_or(|t| t >= *written))
            })
            .collect();
        items.sort();
        for (id, item) in items {
            for path in item_files(&item) {
                let packed = has_extension(&path, "bin");
                out.push(AddonFile { path, origin: Origin::Subscribed, workshop_id: Some(id), packed });
                ids.insert(id);
            }
        }
    }

    let game = install.game_dir();
    for path in files_with(&game.join("addons"), &["gma"]) {
        out.push(AddonFile { path, origin: Origin::AddonsFolder, workshop_id: None, packed: false });
    }
    for path in files_with(&game.join("cache/workshop"), &["gma"]) {
        let id = path.file_stem().and_then(|s| s.to_str()).and_then(|s| s.parse::<u64>().ok());
        if id.is_some_and(|id| ids.contains(&id)) {
            continue;
        }
        out.push(AddonFile { path, origin: Origin::ServerDownload, workshop_id: id, packed: false });
    }
    out
}

/// What an addon is known by when it is switched off: its workshop item, or for a `.gma` put
/// into `garrysmod/addons` by hand, its file's name.
pub fn addon_key(addon: &AddonFile) -> String {
    match addon.workshop_id {
        Some(id) if addon.origin != Origin::AddonsFolder => id.to_string(),
        _ => addon.path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default(),
    }
}

/// The addons switched off: a file with one [`addon_key`] a line. None when it is missing.
pub fn read_disabled(path: &Path) -> HashSet<String> {
    std::fs::read_to_string(path)
        .map(|text| text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("//")).map(str::to_string).collect())
        .unwrap_or_default()
}

/// Writes the addons switched off, sorted.
pub fn write_disabled(path: &Path, disabled: &HashSet<String>) -> io::Result<()> {
    let mut keys: Vec<&String> = disabled.iter().collect();
    keys.sort();
    let mut text = String::from("// addons openGMOD does not mount: workshop items, or files of garrysmod/addons\n");
    for key in keys {
        text.push_str(key);
        text.push('\n');
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, text)
}

/// Unpacks an old compressed workshop download into `cache_dir` (unless an unpacked copy at
/// least as new is already there) and returns the unpacked `.gma`.
pub fn unpack_legacy(packed: &Path, workshop_id: Option<u64>, cache_dir: &Path) -> io::Result<PathBuf> {
    let context = |e: io::Error| io::Error::new(e.kind(), format!("{}: {e}", packed.display()));
    let name = match workshop_id {
        Some(id) => format!("{id}.gma"),
        None => format!("{}.gma", packed.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()),
    };
    let target = cache_dir.join(name);
    let source = std::fs::metadata(packed).map_err(context)?;
    if let Ok(unpacked) = std::fs::metadata(&target)
        && unpacked.len() > 0
        && let (Ok(newer), Ok(older)) = (unpacked.modified(), source.modified())
        && newer >= older
    {
        return Ok(target);
    }
    if source.len() > MAX_PACKED {
        return Err(context(invalid(format!("{} bytes is too large", source.len()))));
    }
    let data = lzma::decompress_alone(&std::fs::read(packed).map_err(context)?, MAX_UNPACKED).map_err(context)?;
    if !data.starts_with(b"GMAD") {
        return Err(context(invalid("the unpacked data is not a GMA file")));
    }
    std::fs::create_dir_all(cache_dir)?;
    let mut temporary = target.clone().into_os_string();
    temporary.push(".part");
    std::fs::write(&temporary, &data)?;
    std::fs::rename(&temporary, &target)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::gma::{self, GmaArchive, GmaInfo};
    use crate::testutil::{TempDir, fake_install};

    #[test]
    fn finds_every_kind_of_addon() {
        let tmp = TempDir::new();
        let library = tmp.path().join("Library");
        let root = library.join("steamapps/common/GarrysMod");
        fake_install(&root);
        let install = Install::at(&root).unwrap();
        let workshop = install.workshop_dir().unwrap();
        let info = GmaInfo { title: "t".into(), ..Default::default() };
        for id in ["222", "111", "333"] {
            std::fs::create_dir_all(workshop.join(id)).unwrap();
        }
        gma::write(&workshop.join("111/item.gma"), &info, &[]).unwrap();
        std::fs::write(workshop.join("222/old_legacy.bin"), b"").unwrap();
        gma::write(&workshop.join("333/gone.gma"), &info, &[]).unwrap();
        // a GMA by another name, and a file that is no addon
        gma::write(&workshop.join("111/temp"), &info, &[]).unwrap();
        std::fs::write(workshop.join("111/notes.txt"), b"not an addon").unwrap();
        std::fs::create_dir_all(install.game_dir().join("addons")).unwrap();
        gma::write(&install.game_dir().join("addons/by_hand.gma"), &info, &[]).unwrap();
        std::fs::create_dir_all(install.game_dir().join("cache/workshop")).unwrap();
        gma::write(&install.game_dir().join("cache/workshop/111.gma"), &info, &[]).unwrap();
        gma::write(&install.game_dir().join("cache/workshop/444.gma"), &info, &[]).unwrap();

        let summary = |files: Vec<AddonFile>| -> Vec<(String, Origin, Option<u64>, bool)> {
            files
                .into_iter()
                .map(|f| (f.path.file_name().unwrap().to_string_lossy().into_owned(), f.origin, f.workshop_id, f.packed))
                .collect()
        };
        let all = [
            ("item.gma".to_string(), Origin::Subscribed, Some(111), false),
            ("temp".to_string(), Origin::Subscribed, Some(111), false),
            ("old_legacy.bin".to_string(), Origin::Subscribed, Some(222), true),
            ("gone.gma".to_string(), Origin::Subscribed, Some(333), false),
            ("by_hand.gma".to_string(), Origin::AddonsFolder, None, false),
            ("444.gma".to_string(), Origin::ServerDownload, Some(444), false),
        ];
        assert_eq!(summary(addon_files(&install)), all);

        // with Steam's list, items no longer subscribed (333) are left out
        let acf = "\"AppWorkshop\"\n{\n\t\"appid\" \"4000\"\n\t\"WorkshopItemsInstalled\"\n\t{\n\t\t\"111\" { \"size\" \"1\" }\n\t\t\"222\" { \"size\" \"1\" }\n\t}\n}\n";
        let manifest = library.join("steamapps/workshop/appworkshop_4000.acf");
        std::fs::write(&manifest, acf).unwrap();
        let written_at = |time: SystemTime| std::fs::File::options().write(true).open(&manifest).unwrap().set_modified(time).unwrap();
        written_at(SystemTime::now() + Duration::from_secs(3600));
        let mut expected = all.to_vec();
        expected.remove(3);
        assert_eq!(summary(addon_files(&install)), expected);
        // ...but not an item downloaded after Steam last wrote its list
        written_at(SystemTime::now() - Duration::from_secs(3600));
        assert_eq!(summary(addon_files(&install)), all);
        written_at(SystemTime::now() + Duration::from_secs(3600));

        // what each is switched off by: the item, or the file's name
        let keys: Vec<String> = addon_files(&install).iter().map(addon_key).collect();
        assert_eq!(keys, ["111", "111", "222", "by_hand.gma", "444"]);
        let file = tmp.path().join("settings/disabled.txt");
        let disabled: HashSet<String> = ["111".to_string(), "by_hand.gma".to_string()].into_iter().collect();
        write_disabled(&file, &disabled).unwrap();
        assert_eq!(read_disabled(&file), disabled);
        assert!(read_disabled(&tmp.path().join("none.txt")).is_empty());
    }

    #[test]
    fn unpacks_legacy_downloads_once() {
        let tmp = TempDir::new();
        // an LZMA stream around a small GMA (made with Python's lzma module, FORMAT_ALONE)
        let gma_bytes = {
            let path = tmp.path().join("plain.gma");
            gma::write(&path, &GmaInfo { title: "Legacy".into(), ..Default::default() }, &[("lua/autorun/legacy.lua", b"print(1)")])
                .unwrap();
            std::fs::read(&path).unwrap()
        };
        let packed_path = tmp.path().join("item_legacy.bin");
        std::fs::write(&packed_path, store_only_lzma(&gma_bytes)).unwrap();

        let cache = tmp.path().join("cache");
        let unpacked = unpack_legacy(&packed_path, Some(42), &cache).unwrap();
        assert_eq!(unpacked, cache.join("42.gma"));
        let addon = GmaArchive::open(&unpacked).unwrap();
        assert_eq!(addon.info().title, "Legacy");
        assert_eq!(addon.read("lua/autorun/legacy.lua").unwrap(), b"print(1)");

        // a second call keeps the unpacked file
        std::fs::write(&unpacked, b"GMAD-marker").unwrap();
        assert_eq!(std::fs::read(unpack_legacy(&packed_path, Some(42), &cache).unwrap()).unwrap(), b"GMAD-marker");

        std::fs::write(&packed_path, b"not lzma at all").unwrap();
        assert!(unpack_legacy(&packed_path, Some(43), &cache).is_err());
    }

    /// An LZMA "alone" stream of `data` that uses only literals - enough to test the
    /// container handling without an encoder. Built by running the range coder's encoding
    /// side for "literal" decisions.
    fn store_only_lzma(data: &[u8]) -> Vec<u8> {
        let mut enc = RangeEncoder::default();
        let (lc, lp, pb) = (3u32, 0u32, 2u32);
        let mut literal = vec![1024u16; 0x300 << (lc + lp)];
        let mut is_match = [1024u16; 12 * 16];
        let mut prev = 0u8;
        for (pos, &byte) in data.iter().enumerate() {
            let pos_state = pos & ((1 << pb) - 1);
            // state stays 0: only literals
            enc.bit(&mut is_match[pos_state], 0);
            let lit_state = (prev as usize) >> (8 - lc);
            let probs = &mut literal[lit_state * 0x300..(lit_state + 1) * 0x300];
            let mut symbol = 1usize;
            for i in (0..8).rev() {
                let bit = ((byte >> i) & 1) as u32;
                enc.bit(&mut probs[symbol], bit);
                symbol = (symbol << 1) | bit as usize;
            }
            prev = byte;
        }
        let mut out = vec![((pb * 5 + lp) * 9 + lc) as u8];
        out.extend_from_slice(&(1u32 << 16).to_le_bytes());
        out.extend_from_slice(&(data.len() as u64).to_le_bytes());
        out.extend(enc.finish());
        out
    }

    /// The encoding half of LZMA's range coder, for the test above.
    struct RangeEncoder {
        low: u64,
        range: u32,
        cache: u8,
        cache_size: u64,
        out: Vec<u8>,
    }

    impl Default for RangeEncoder {
        fn default() -> Self {
            RangeEncoder { low: 0, range: u32::MAX, cache: 0, cache_size: 1, out: Vec::new() }
        }
    }

    impl RangeEncoder {
        fn shift_low(&mut self) {
            if self.low < 0xFF00_0000 || self.low > 0xFFFF_FFFF {
                let carry = (self.low >> 32) as u8;
                let mut temp = self.cache;
                loop {
                    self.out.push(temp.wrapping_add(carry));
                    temp = 0xFF;
                    self.cache_size -= 1;
                    if self.cache_size == 0 {
                        break;
                    }
                }
                self.cache = ((self.low >> 24) & 0xFF) as u8;
            }
            self.cache_size += 1;
            self.low = (self.low & 0x00FF_FFFF) << 8;
        }

        fn bit(&mut self, prob: &mut u16, bit: u32) {
            let bound = (self.range >> 11) * *prob as u32;
            if bit == 0 {
                self.range = bound;
                *prob += (2048 - *prob) >> 5;
            } else {
                self.low += bound as u64;
                self.range -= bound;
                *prob -= *prob >> 5;
            }
            while self.range < (1 << 24) {
                self.range <<= 8;
                self.shift_low();
            }
        }

        fn finish(mut self) -> Vec<u8> {
            for _ in 0..5 {
                self.shift_low();
            }
            self.out
        }
    }
}
