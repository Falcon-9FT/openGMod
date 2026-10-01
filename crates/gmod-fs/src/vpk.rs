//! Valve packs (`.vpk`), the archives Garry's Mod and the Source engine ship their content in.
//!
//! A pack is a directory file (`garrysmod_dir.vpk`) holding a tree of all entries, and
//! numbered data files next to it (`garrysmod_000.vpk`, `garrysmod_001.vpk` ...) holding the
//! bytes. A small pack can keep the bytes in the directory file itself (a single-file pack).
//! Layout, little endian:
//!
//! | part | contents |
//! |---|---|
//! | header, version 1 | `u32` signature `0x55AA1234`, `u32` version, `u32` size of the tree |
//! | header, version 2 | version 1, then `u32` sizes of: embedded file data, archive MD5s, other MD5s, signature |
//! | tree | extensions, each followed by its folders, each followed by its file names and entries |
//! | file data | bytes of the entries kept in the directory file |
//!
//! The tree is three nested lists of NUL-terminated strings (extension, folder, file name),
//! each list ended by an empty string. A single space stands for "none": a file without an
//! extension, or one in the root folder. Each file name is followed by its entry: `u32`
//! CRC-32 of the whole file, `u16` count of preload bytes stored right after the entry,
//! `u16` archive index (`0x7FFF`: the directory file, counted from the end of the tree),
//! `u32` offset, `u32` length, `u16` terminator `0xFFFF`. A file is its preload bytes
//! followed by `length` bytes at `offset` of its archive.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use crate::crc32::crc32;
use crate::index;
use crate::io_util::{Bytes, invalid, read_exact_at};
use crate::path;

const SIGNATURE: u32 = 0x55AA_1234;
/// Archive index of the entries whose bytes are in the directory file itself.
pub const EMBEDDED: u16 = 0x7FFF;
const TERMINATOR: u16 = 0xFFFF;

/// One file in a pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VpkEntry {
    /// Normalized path.
    pub path: String,
    pub crc: u32,
    /// Which numbered data file holds the bytes ([`EMBEDDED`]: the directory file).
    pub archive: u16,
    pub offset: u32,
    pub length: u32,
    /// Start and length of the preload bytes in [`VpkArchive`]'s preload buffer.
    preload: (usize, u16),
}

impl VpkEntry {
    /// Size of the whole file.
    pub fn size(&self) -> u64 {
        self.preload.1 as u64 + self.length as u64
    }
}

/// An open pack: its tree in memory, its data files opened as they are needed.
pub struct VpkArchive {
    dir_path: PathBuf,
    /// The directory file's path without `_dir.vpk` (or `.vpk`); data files append `_NNN.vpk`.
    stem: PathBuf,
    version: u32,
    /// Where the bytes of [`EMBEDDED`] entries are counted from: the end of the tree.
    embedded_base: u64,
    /// Sorted by path.
    entries: Vec<VpkEntry>,
    preload: Vec<u8>,
    /// Open data files with their lengths, by archive index.
    files: Mutex<HashMap<u16, Arc<(File, u64)>>>,
}

impl fmt::Debug for VpkArchive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VpkArchive")
            .field("path", &self.dir_path)
            .field("version", &self.version)
            .field("entries", &self.entries.len())
            .finish()
    }
}

impl VpkArchive {
    /// Opens a pack by its directory file: `name_dir.vpk`, or `name.vpk` for a single-file pack.
    pub fn open(dir_path: &Path) -> io::Result<VpkArchive> {
        let context = |e: io::Error| io::Error::new(e.kind(), format!("{}: {e}", dir_path.display()));
        let mut file = File::open(dir_path).map_err(context)?;
        let file_len = file.metadata().map_err(context)?.len();
        let mut head = [0u8; 12];
        file.read_exact(&mut head).map_err(|_| context(invalid("too short to be a VPK")))?;
        let mut h = Bytes::new(&head);
        let (signature, version, tree_size) = (h.u32()?, h.u32()?, h.u32()? as u64);
        if signature != SIGNATURE {
            return Err(context(invalid(format!("not a VPK (signature {signature:#010x})"))));
        }
        let header_len = match version {
            1 => 12,
            2 => {
                let mut rest = [0u8; 16];
                file.read_exact(&mut rest).map_err(context)?;
                28
            }
            other => return Err(context(invalid(format!("VPK version {other} is not supported")))),
        };
        if header_len + tree_size > file_len {
            return Err(context(invalid(format!("the tree ({tree_size} bytes) runs past the end of the file"))));
        }
        let mut tree = vec![0u8; tree_size as usize];
        file.read_exact(&mut tree).map_err(context)?;
        let (entries, preload) = parse_tree(&tree).map_err(context)?;
        let stem = stem_of(dir_path);
        let files = Mutex::new(HashMap::from([(EMBEDDED, Arc::new((file, file_len)))]));
        Ok(VpkArchive { dir_path: dir_path.to_path_buf(), stem, version, embedded_base: header_len + tree_size, entries, preload, files })
    }

    /// The directory file.
    pub fn path(&self) -> &Path {
        &self.dir_path
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All entries, sorted by path.
    pub fn entries(&self) -> &[VpkEntry] {
        &self.entries
    }

    /// The entry at a normalized path.
    pub fn entry(&self, path: &str) -> Option<&VpkEntry> {
        debug_assert!(path::is_normalized(path), "{path:?}");
        index::find(&self.entries, |e| &e.path, path)
    }

    /// `Some(is a folder)` if something is at the normalized `path`.
    pub fn contains(&self, path: &str) -> Option<bool> {
        if self.entry(path).is_some() { Some(false) } else { index::has_dir(&self.entries, |e| &e.path, path).then_some(true) }
    }

    /// The names directly inside the normalized folder `dir`, each with whether it is a folder.
    pub fn list(&self, dir: &str) -> Vec<(String, bool)> {
        index::children(&self.entries, |e| &e.path, dir)
    }

    /// Reads the file at a normalized path.
    pub fn read(&self, path: &str) -> io::Result<Vec<u8>> {
        let entry = self
            .entry(path)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{path} is not in {}", self.dir_path.display())))?;
        self.read_entry(entry)
    }

    /// Reads one entry of this pack.
    pub fn read_entry(&self, entry: &VpkEntry) -> io::Result<Vec<u8>> {
        let (start, len) = entry.preload;
        let preload = &self.preload[start..start + len as usize];
        if entry.length == 0 {
            return Ok(preload.to_vec());
        }
        let file = self.data_file(entry.archive)?;
        let base = if entry.archive == EMBEDDED { self.embedded_base } else { 0 };
        let begin = base + entry.offset as u64;
        if begin + entry.length as u64 > file.1 {
            return Err(invalid(format!("{}: {} runs past the end of archive {}", self.dir_path.display(), entry.path, entry.archive)));
        }
        let mut out = Vec::with_capacity(preload.len() + entry.length as usize);
        out.extend_from_slice(preload);
        out.resize(preload.len() + entry.length as usize, 0);
        read_exact_at(&file.0, &mut out[preload.len()..], begin)?;
        Ok(out)
    }

    /// Whether the entry's bytes match its CRC-32.
    pub fn verify(&self, entry: &VpkEntry) -> io::Result<bool> {
        Ok(crc32(&self.read_entry(entry)?) == entry.crc)
    }

    /// The path of numbered data file `index`.
    pub fn data_file_path(&self, index: u16) -> PathBuf {
        if index == EMBEDDED {
            return self.dir_path.clone();
        }
        let mut name = self.stem.clone().into_os_string();
        name.push(format!("_{index:03}.vpk"));
        PathBuf::from(name)
    }

    fn data_file(&self, index: u16) -> io::Result<Arc<(File, u64)>> {
        let mut files = self.files.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(file) = files.get(&index) {
            return Ok(file.clone());
        }
        let path = self.data_file_path(index);
        let context = |e: io::Error| io::Error::new(e.kind(), format!("{}: {e}", path.display()));
        let file = File::open(&path).map_err(context)?;
        let len = file.metadata().map_err(context)?.len();
        let file = Arc::new((file, len));
        files.insert(index, file.clone());
        Ok(file)
    }
}

/// `name_dir.vpk` → `name`, `name.vpk` → `name` (keeping the folder).
fn stem_of(dir_path: &Path) -> PathBuf {
    let name = dir_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let lower = name.to_ascii_lowercase();
    let cut = if lower.ends_with("_dir.vpk") {
        name.len() - "_dir.vpk".len()
    } else if lower.ends_with(".vpk") {
        name.len() - ".vpk".len()
    } else {
        name.len()
    };
    dir_path.with_file_name(&name[..cut])
}

fn parse_tree(tree: &[u8]) -> io::Result<(Vec<VpkEntry>, Vec<u8>)> {
    let mut r = Bytes::new(tree);
    let mut entries = Vec::new();
    let mut preload = Vec::new();
    loop {
        let ext = r.cstr()?;
        if ext.is_empty() {
            break;
        }
        loop {
            let dir = r.cstr()?;
            if dir.is_empty() {
                break;
            }
            loop {
                let name = r.cstr()?;
                if name.is_empty() {
                    break;
                }
                let crc = r.u32()?;
                let preload_len = r.u16()?;
                let archive = r.u16()?;
                let offset = r.u32()?;
                let length = r.u32()?;
                let terminator = r.u16()?;
                let full = entry_path(dir, name, ext);
                if terminator != TERMINATOR {
                    return Err(invalid(format!("entry {full}: terminator {terminator:#06x} instead of 0xffff")));
                }
                let start = preload.len();
                preload.extend_from_slice(r.take(preload_len as usize)?);
                // a name the file system could never ask for is skipped, not an error
                let Some(path) = path::normalize(&full) else { continue };
                entries.push(VpkEntry { path, crc, archive, offset, length, preload: (start, preload_len) });
            }
        }
    }
    // stable: of two entries with one path the first in the file stays
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    entries.dedup_by(|later, earlier| later.path == earlier.path);
    Ok((entries, preload))
}

fn entry_path(dir: &[u8], name: &[u8], ext: &[u8]) -> String {
    let mut out = String::new();
    if dir != b" " {
        out.push_str(&String::from_utf8_lossy(dir));
        out.push('/');
    }
    out.push_str(&String::from_utf8_lossy(name));
    if ext != b" " {
        out.push('.');
        out.push_str(&String::from_utf8_lossy(ext));
    }
    out
}

/// Writes a version 1 single-file pack holding `files` (path, bytes): for tests and tools.
pub fn write_single(out: &Path, files: &[(&str, &[u8])]) -> io::Result<()> {
    // extension → folder → [(name, file index)]
    let mut tree: BTreeMap<String, BTreeMap<String, Vec<(String, usize)>>> = BTreeMap::new();
    for (i, (file_path, _)) in files.iter().enumerate() {
        let normalized =
            path::normalize(file_path).filter(|p| !p.is_empty()).ok_or_else(|| invalid(format!("{file_path:?} is not a file path")))?;
        let (dir, file_name) = path::split(&normalized);
        let (stem, ext) = match file_name.rfind('.') {
            Some(dot) if dot > 0 => (&file_name[..dot], &file_name[dot + 1..]),
            _ => (file_name, " "),
        };
        let dir = if dir.is_empty() { " " } else { dir };
        tree.entry(ext.to_string()).or_default().entry(dir.to_string()).or_default().push((stem.to_string(), i));
    }
    let mut tree_bytes = Vec::new();
    let mut data = Vec::new();
    for (ext, dirs) in &tree {
        push_cstr(&mut tree_bytes, ext);
        for (dir, names) in dirs {
            push_cstr(&mut tree_bytes, dir);
            for (name, i) in names {
                let bytes = files[*i].1;
                let offset = u32::try_from(data.len()).map_err(|_| invalid("more than 4 GiB of data"))?;
                let length = u32::try_from(bytes.len()).map_err(|_| invalid("a file of more than 4 GiB"))?;
                push_cstr(&mut tree_bytes, name);
                tree_bytes.extend_from_slice(&crc32(bytes).to_le_bytes());
                tree_bytes.extend_from_slice(&0u16.to_le_bytes());
                tree_bytes.extend_from_slice(&EMBEDDED.to_le_bytes());
                tree_bytes.extend_from_slice(&offset.to_le_bytes());
                tree_bytes.extend_from_slice(&length.to_le_bytes());
                tree_bytes.extend_from_slice(&TERMINATOR.to_le_bytes());
                data.extend_from_slice(bytes);
            }
            tree_bytes.push(0);
        }
        tree_bytes.push(0);
    }
    tree_bytes.push(0);
    let mut file = File::create(out)?;
    file.write_all(&SIGNATURE.to_le_bytes())?;
    file.write_all(&1u32.to_le_bytes())?;
    file.write_all(&(tree_bytes.len() as u32).to_le_bytes())?;
    file.write_all(&tree_bytes)?;
    file.write_all(&data)?;
    file.flush()
}

fn push_cstr(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
    out.push(0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn single_file_round_trip() {
        let tmp = TempDir::new();
        let path = tmp.path().join("pak01_dir.vpk");
        let files: [(&str, &[u8]); 5] = [
            ("materials/models/chair.vmt", b"\"VertexLitGeneric\" {}"),
            ("Materials\\Models\\Table.VTF", &[1, 2, 3, 4]),
            ("readme", b"no extension"),
            ("root.txt", b"in the root"),
            ("empty.dat", b""),
        ];
        write_single(&path, &files).unwrap();

        let vpk = VpkArchive::open(&path).unwrap();
        assert_eq!(vpk.version(), 1);
        assert_eq!(vpk.len(), 5);
        assert_eq!(vpk.read("materials/models/chair.vmt").unwrap(), files[0].1);
        assert_eq!(vpk.read("materials/models/table.vtf").unwrap(), [1, 2, 3, 4]);
        assert_eq!(vpk.read("readme").unwrap(), b"no extension");
        assert_eq!(vpk.read("root.txt").unwrap(), b"in the root");
        assert_eq!(vpk.read("empty.dat").unwrap(), b"");
        assert!(vpk.read("missing.txt").is_err());
        for entry in vpk.entries() {
            assert!(vpk.verify(entry).unwrap(), "{}", entry.path);
        }
        assert_eq!(vpk.contains("materials/models"), Some(true));
        assert_eq!(vpk.contains("materials/models/chair.vmt"), Some(false));
        assert_eq!(vpk.contains("materials/mod"), None);
        assert_eq!(vpk.list("materials/models"), [("chair.vmt".to_string(), false), ("table.vtf".to_string(), false)]);
        assert_eq!(vpk.data_file_path(3), tmp.path().join("pak01_003.vpk"));
    }

    /// A directory file whose entries live in a numbered data file, with preload bytes, in
    /// the version 2 layout.
    #[test]
    fn numbered_archives_and_preload() {
        let tmp = TempDir::new();
        let data = b"0123456789abcdef";
        std::fs::write(tmp.path().join("test_000.vpk"), data).unwrap();

        let whole = [b"PRE".as_slice(), &data[4..10]].concat();
        let mut tree = Vec::new();
        push_cstr(&mut tree, "txt");
        push_cstr(&mut tree, "docs");
        push_cstr(&mut tree, "notes");
        tree.extend_from_slice(&crc32(&whole).to_le_bytes());
        tree.extend_from_slice(&3u16.to_le_bytes());
        tree.extend_from_slice(&0u16.to_le_bytes());
        tree.extend_from_slice(&4u32.to_le_bytes());
        tree.extend_from_slice(&6u32.to_le_bytes());
        tree.extend_from_slice(&TERMINATOR.to_le_bytes());
        tree.extend_from_slice(b"PRE");
        tree.extend_from_slice(&[0, 0, 0]);
        let mut dir_file = Vec::new();
        dir_file.extend_from_slice(&SIGNATURE.to_le_bytes());
        dir_file.extend_from_slice(&2u32.to_le_bytes());
        dir_file.extend_from_slice(&(tree.len() as u32).to_le_bytes());
        dir_file.extend_from_slice(&[0u8; 16]);
        dir_file.extend_from_slice(&tree);
        let dir_path = tmp.path().join("test_dir.vpk");
        std::fs::write(&dir_path, &dir_file).unwrap();

        let vpk = VpkArchive::open(&dir_path).unwrap();
        assert_eq!(vpk.version(), 2);
        let entry = vpk.entry("docs/notes.txt").unwrap();
        assert_eq!(entry.size(), 9);
        assert_eq!(vpk.read_entry(entry).unwrap(), whole);
        assert!(vpk.verify(entry).unwrap());

        // A damaged entry is reported, not trusted.
        std::fs::write(tmp.path().join("test_000.vpk"), b"short").unwrap();
        let vpk = VpkArchive::open(&dir_path).unwrap();
        assert!(vpk.read("docs/notes.txt").is_err());
    }

    #[test]
    fn rejects_what_is_not_a_pack() {
        let tmp = TempDir::new();
        let path = tmp.path().join("bad_dir.vpk");
        std::fs::write(&path, b"GMAD\x03 definitely not a pack").unwrap();
        assert!(VpkArchive::open(&path).is_err());
        std::fs::write(&path, [0x34, 0x12, 0xAA, 0x55, 1, 0, 0, 0, 0xFF, 0, 0, 0]).unwrap();
        assert!(VpkArchive::open(&path).is_err(), "tree larger than the file");
        assert!(VpkArchive::open(&tmp.path().join("missing_dir.vpk")).is_err());
    }
}
