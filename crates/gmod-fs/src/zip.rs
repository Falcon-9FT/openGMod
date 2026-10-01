//! Zip archives: above all a map's pakfile - the materials, models and sounds a map brings
//! along, a zip inside the BSP file - but any zip can be mounted.
//!
//! The central directory at the end of the archive lists the entries; each entry's bytes follow
//! its local header. Entries stored (method 0), deflated (8) or LZMA-compressed (14, what
//! Valve's `bspzip -repack` writes) can be read. Offsets are taken relative to where the
//! directory really is, so an archive embedded in another file (a pakfile lump) reads the same
//! whether its offsets count from the archive or from the file.

use std::fmt;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use crate::crc32::crc32;
use crate::index;
use crate::io_util::{MAX_FILE, invalid, read_exact_at};
use crate::{inflate, lzma, path};

const END_OF_DIRECTORY: u32 = 0x0605_4B50;
const DIRECTORY_ENTRY: u32 = 0x0201_4B50;
const LOCAL_HEADER: u32 = 0x0403_4B50;

/// One file in a zip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipEntry {
    /// Normalized path.
    pub path: String,
    pub method: u16,
    pub crc: u32,
    pub compressed: u64,
    pub size: u64,
    /// Offset of the local header from the start of the archive.
    header: u64,
}

/// An open zip archive, or a zip inside a file (a map's pakfile).
pub struct ZipArchive {
    path: PathBuf,
    file: File,
    /// Where the archive starts in the file, and its length.
    base: u64,
    len: u64,
    /// Sorted by path.
    entries: Vec<ZipEntry>,
}

impl fmt::Debug for ZipArchive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ZipArchive").field("path", &self.path).field("base", &self.base).field("entries", &self.entries.len()).finish()
    }
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

impl ZipArchive {
    /// Opens a zip file.
    pub fn open(path: &Path) -> io::Result<ZipArchive> {
        let len = std::fs::metadata(path)?.len();
        ZipArchive::open_range(path, 0, len)
    }

    /// Opens the zip that occupies `len` bytes from `offset` in the file at `path`.
    pub fn open_range(path: &Path, offset: u64, len: u64) -> io::Result<ZipArchive> {
        let context = |e: io::Error| io::Error::new(e.kind(), format!("{} (zip at {offset}): {e}", path.display()));
        let file = File::open(path).map_err(context)?;
        let file_len = file.metadata().map_err(context)?.len();
        if offset.checked_add(len).is_none_or(|end| end > file_len) {
            return Err(context(invalid("the archive runs past the end of the file")));
        }
        let entries = read_directory(&file, offset, len).map_err(context)?;
        Ok(ZipArchive { path: path.to_path_buf(), file, base: offset, len, entries })
    }

    /// The file the archive is in.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All entries, sorted by path.
    pub fn entries(&self) -> &[ZipEntry] {
        &self.entries
    }

    /// The entry at a normalized path.
    pub fn entry(&self, path: &str) -> Option<&ZipEntry> {
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
        let entry =
            self.entry(path).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{path} is not in {}", self.path.display())))?;
        self.read_entry(entry)
    }

    pub fn read_entry(&self, entry: &ZipEntry) -> io::Result<Vec<u8>> {
        let context = |e: io::Error| io::Error::new(e.kind(), format!("{}: {}: {e}", self.path.display(), entry.path));
        if entry.size > MAX_FILE || entry.compressed > MAX_FILE {
            return Err(context(invalid("too large to read")));
        }
        let mut header = [0u8; 30];
        if entry.header + 30 > self.len {
            return Err(context(invalid("local header past the end of the archive")));
        }
        read_exact_at(&self.file, &mut header, self.base + entry.header).map_err(context)?;
        if u32_at(&header, 0) != LOCAL_HEADER {
            return Err(context(invalid("no local header where the directory says")));
        }
        let start = entry.header + 30 + u16_at(&header, 26) as u64 + u16_at(&header, 28) as u64;
        if start + entry.compressed > self.len {
            return Err(context(invalid("data past the end of the archive")));
        }
        let mut data = vec![0u8; entry.compressed as usize];
        read_exact_at(&self.file, &mut data, self.base + start).map_err(context)?;
        let out = match entry.method {
            0 => data,
            8 => inflate::inflate(&data, entry.size as usize, entry.size).map_err(context)?,
            14 => {
                // LZMA in zip: `u16` version, `u16` size of the properties, the properties, the stream
                if data.len() < 4 {
                    return Err(context(invalid("LZMA entry too short")));
                }
                let properties_end = 4 + u16_at(&data, 2) as usize;
                if properties_end > data.len() || properties_end < 9 {
                    return Err(context(invalid("LZMA entry with bad properties")));
                }
                lzma::decompress_raw(&data[4..properties_end], &data[properties_end..], Some(entry.size), entry.size).map_err(context)?
            }
            other => return Err(context(invalid(format!("compression method {other} is not supported")))),
        };
        if out.len() as u64 != entry.size {
            return Err(context(invalid(format!("unpacked to {} bytes instead of {}", out.len(), entry.size))));
        }
        Ok(out)
    }

    /// Whether the entry's bytes match its CRC-32.
    pub fn verify(&self, entry: &ZipEntry) -> io::Result<bool> {
        Ok(crc32(&self.read_entry(entry)?) == entry.crc)
    }
}

fn read_directory(file: &File, base: u64, len: u64) -> io::Result<Vec<ZipEntry>> {
    // the end-of-directory record: 22 bytes and a comment of up to 65 535
    let tail_len = len.min(22 + 65_535);
    if tail_len < 22 {
        return Err(invalid("too short to be a zip archive"));
    }
    let mut tail = vec![0u8; tail_len as usize];
    read_exact_at(file, &mut tail, base + len - tail_len)?;
    let at = (0..=tail.len() - 22).rev().find(|&i| u32_at(&tail, i) == END_OF_DIRECTORY).ok_or_else(|| invalid("not a zip archive"))?;
    let record = &tail[at..];
    let count = u16_at(record, 10) as usize;
    let directory_size = u32_at(record, 12) as u64;
    let directory_offset = u32_at(record, 16) as u64;
    if directory_size == 0xFFFF_FFFF || directory_offset == 0xFFFF_FFFF || count == 0xFFFF {
        return Err(invalid("ZIP64 archives are not supported"));
    }
    let record_position = len - tail_len + at as u64;
    // where the directory really starts; offsets in the archive are shifted by as much
    let directory_start = record_position.checked_sub(directory_size).ok_or_else(|| invalid("the directory is larger than the archive"))?;
    let shift = directory_start as i64 - directory_offset as i64;
    let mut directory = vec![0u8; directory_size as usize];
    read_exact_at(file, &mut directory, base + directory_start)?;

    let mut entries = Vec::with_capacity(count);
    let mut pos = 0usize;
    for _ in 0..count {
        if pos + 46 > directory.len() || u32_at(&directory, pos) != DIRECTORY_ENTRY {
            return Err(invalid("damaged central directory"));
        }
        let e = &directory[pos..];
        let (name_len, extra_len, comment_len) = (u16_at(e, 28) as usize, u16_at(e, 30) as usize, u16_at(e, 32) as usize);
        let end = 46 + name_len + extra_len + comment_len;
        if pos + end > directory.len() {
            return Err(invalid("damaged central directory"));
        }
        let (compressed, size, header) = (u32_at(e, 20) as u64, u32_at(e, 24) as u64, u32_at(e, 42) as u64);
        if compressed == 0xFFFF_FFFF || size == 0xFFFF_FFFF || header == 0xFFFF_FFFF {
            return Err(invalid("ZIP64 entries are not supported"));
        }
        let name = String::from_utf8_lossy(&e[46..46 + name_len]);
        pos += end;
        if name.ends_with('/') {
            continue;
        }
        let Some(path) = path::normalize(&name).filter(|p| !p.is_empty()) else { continue };
        let header = u64::try_from(header as i64 + shift).map_err(|_| invalid("an entry starts before the archive"))?;
        entries.push(ZipEntry { path, method: u16_at(e, 10), crc: u32_at(e, 16), compressed, size, header });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    entries.dedup_by(|later, earlier| later.path == earlier.path);
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn unhex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// A zip made by Python's `zipfile` with a stored, a deflated and an LZMA entry.
    fn sample() -> Vec<u8> {
        unhex(include_str!("../testdata/zip/sample.hex"))
    }

    fn check(zip: &ZipArchive) {
        assert_eq!(zip.len(), 3);
        assert_eq!(zip.read("materials/stored.txt").unwrap(), b"stored entry");
        assert_eq!(zip.read("materials/maps/deflated.vmt").unwrap(), "\"LightmappedGeneric\" { }\n".repeat(20).as_bytes());
        assert_eq!(zip.read("models/lzma.mdl").unwrap(), "LZMA entry, as bspzip -repack writes. ".repeat(10).as_bytes());
        for entry in zip.entries() {
            assert!(zip.verify(entry).unwrap(), "{}", entry.path);
        }
        assert_eq!(zip.list("materials"), [("maps".to_string(), true), ("stored.txt".to_string(), false)]);
    }

    #[test]
    fn methods_and_embedding() {
        let tmp = TempDir::new();
        let alone = tmp.path().join("sample.zip");
        std::fs::write(&alone, sample()).unwrap();
        check(&ZipArchive::open(&alone).unwrap());

        // inside another file, as a map's pakfile is: offsets count from the archive
        let mut map = b"VBSP and 1000 bytes of lumps".repeat(40);
        let offset = map.len() as u64;
        map.extend_from_slice(&sample());
        map.extend_from_slice(b"trailing lumps");
        let embedded = tmp.path().join("map.bsp");
        std::fs::write(&embedded, &map).unwrap();
        check(&ZipArchive::open_range(&embedded, offset, sample().len() as u64).unwrap());

        // the same bytes opened as a whole file: offsets are off by the prefix, which is noticed
        let mut prefixed = b"prefix".repeat(10);
        prefixed.extend_from_slice(&sample());
        let file = tmp.path().join("prefixed.zip");
        std::fs::write(&file, &prefixed).unwrap();
        check(&ZipArchive::open(&file).unwrap());
    }

    #[test]
    fn damaged_archives() {
        let tmp = TempDir::new();
        let path = tmp.path().join("bad.zip");
        std::fs::write(&path, b"not a zip at all, but long enough to look for a directory").unwrap();
        assert!(ZipArchive::open(&path).is_err());
        let mut cut = sample();
        cut.drain(10..40);
        std::fs::write(&path, &cut).unwrap();
        if let Ok(zip) = ZipArchive::open(&path) {
            assert!(zip.entries().iter().any(|e| zip.read_entry(e).is_err()));
        }
        assert!(ZipArchive::open_range(&path, 5, 1 << 20).is_err(), "range past the end");
    }
}
