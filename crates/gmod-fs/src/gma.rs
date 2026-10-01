//! Garry's Mod addon files (`.gma`): workshop addons, and addons packed by hand.
//!
//! Layout, little endian, strings NUL-terminated:
//!
//! | part | contents |
//! |---|---|
//! | header | `"GMAD"`, `u8` format version (1-3) |
//! | | `u64` SteamID of the author (unused, 0), `u64` time of packing (Unix seconds) |
//! | | version 2 and later: names of required content, ended by an empty string (always empty) |
//! | | title, description (a JSON object in current addons), author name, `i32` addon version |
//! | file list | `u32` file number (1, 2, ...; 0 ends the list), path, `i64` size, `u32` CRC-32 |
//! | file data | the files' bytes one after another, in list order |
//! | trailer | `u32` CRC-32 of everything before it (0 when the packer did not compute it) |

use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, Seek, Write};
use std::path::{Path, PathBuf};

use crate::crc32::crc32;
use crate::index;
use crate::io_util::{MAX_FILE, invalid, read_array, read_cstr, read_exact_at};
use crate::path;

const MAGIC: &[u8; 4] = b"GMAD";
/// The format version written by current tools.
pub const FORMAT_VERSION: u8 = 3;
/// Bounds that keep a damaged file from making us read or allocate without end.
const MAX_STRING: usize = 16 << 20;
const MAX_FILES: usize = 1 << 20;

/// What an addon says about itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GmaInfo {
    pub format_version: u8,
    pub steam_id: u64,
    /// When it was packed, in Unix seconds.
    pub timestamp: u64,
    pub required_content: Vec<String>,
    pub title: String,
    /// Free text, or in current addons a JSON object with `description`, `type` and `tags`.
    pub description: String,
    pub author: String,
    pub addon_version: i32,
}

/// One file in an addon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GmaEntry {
    /// Normalized path.
    pub path: String,
    pub size: u64,
    pub crc: u32,
    /// Where the bytes start in the addon file.
    offset: u64,
}

/// An open addon file.
pub struct GmaArchive {
    path: PathBuf,
    file: File,
    info: GmaInfo,
    /// Sorted by path.
    entries: Vec<GmaEntry>,
}

impl fmt::Debug for GmaArchive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GmaArchive")
            .field("path", &self.path)
            .field("title", &self.info.title)
            .field("entries", &self.entries.len())
            .finish()
    }
}

impl GmaArchive {
    pub fn open(path: &Path) -> io::Result<GmaArchive> {
        let context = |e: io::Error| io::Error::new(e.kind(), format!("{}: {e}", path.display()));
        let file = File::open(path).map_err(context)?;
        let file_len = file.metadata().map_err(context)?.len();
        let (info, mut entries) = read_header(&file, file_len).map_err(context)?;
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        entries.dedup_by(|later, earlier| later.path == earlier.path);
        Ok(GmaArchive { path: path.to_path_buf(), file, info, entries })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn info(&self) -> &GmaInfo {
        &self.info
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All entries, sorted by path.
    pub fn entries(&self) -> &[GmaEntry] {
        &self.entries
    }

    /// The entry at a normalized path.
    pub fn entry(&self, path: &str) -> Option<&GmaEntry> {
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

    pub fn read_entry(&self, entry: &GmaEntry) -> io::Result<Vec<u8>> {
        // sizes were checked against the file length when it was opened
        if entry.size > MAX_FILE {
            return Err(invalid(format!("{}: {} is {} bytes, too large to read", self.path.display(), entry.path, entry.size)));
        }
        let mut out = vec![0u8; entry.size as usize];
        read_exact_at(&self.file, &mut out, entry.offset)?;
        Ok(out)
    }

    /// Whether the entry's bytes match its CRC-32.
    pub fn verify(&self, entry: &GmaEntry) -> io::Result<bool> {
        Ok(crc32(&self.read_entry(entry)?) == entry.crc)
    }
}

fn string(r: &mut impl io::BufRead) -> io::Result<String> {
    Ok(String::from_utf8_lossy(&read_cstr(r, MAX_STRING)?).into_owned())
}

fn read_header(file: &File, file_len: u64) -> io::Result<(GmaInfo, Vec<GmaEntry>)> {
    let mut r = BufReader::new(file);
    let magic: [u8; 4] = read_array(&mut r).map_err(|_| invalid("too short to be an addon"))?;
    if &magic != MAGIC {
        // Old workshop downloads (`.bin`) are LZMA streams of a GMA: properties byte 0x5D.
        return Err(invalid(if magic[0] == 0x5D && magic[1..3] == [0, 0] {
            "an LZMA-compressed workshop download (.bin); unpacking these is not supported yet"
        } else {
            "not a GMA file"
        }));
    }
    let [format_version] = read_array(&mut r)?;
    if !(1..=FORMAT_VERSION).contains(&format_version) {
        return Err(invalid(format!("GMA format version {format_version} is not supported")));
    }
    let steam_id = u64::from_le_bytes(read_array(&mut r)?);
    let timestamp = u64::from_le_bytes(read_array(&mut r)?);
    let mut required_content = Vec::new();
    if format_version > 1 {
        loop {
            let s = string(&mut r)?;
            if s.is_empty() {
                break;
            }
            required_content.push(s);
        }
    }
    let title = string(&mut r)?;
    let description = string(&mut r)?;
    let author = string(&mut r)?;
    let addon_version = i32::from_le_bytes(read_array(&mut r)?);

    let mut listed = Vec::new();
    loop {
        let number = u32::from_le_bytes(read_array(&mut r)?);
        if number == 0 {
            break;
        }
        if listed.len() == MAX_FILES {
            return Err(invalid(format!("more than {MAX_FILES} files")));
        }
        let name = string(&mut r)?;
        let size = i64::from_le_bytes(read_array(&mut r)?);
        let crc = u32::from_le_bytes(read_array(&mut r)?);
        let size = u64::try_from(size).map_err(|_| invalid(format!("{name}: negative size {size}")))?;
        listed.push((name, size, crc));
    }

    let mut offset = r.stream_position()?;
    let mut entries = Vec::with_capacity(listed.len());
    for (name, size, crc) in listed {
        let end = offset.checked_add(size).filter(|&end| end <= file_len);
        let Some(end) = end else {
            return Err(invalid(format!("{name} ({size} bytes) runs past the end of the file: the addon is cut short")));
        };
        if let Some(path) = path::normalize(&name).filter(|p| !p.is_empty()) {
            entries.push(GmaEntry { path, size, crc, offset });
        }
        offset = end;
    }
    let info = GmaInfo { format_version, steam_id, timestamp, required_content, title, description, author, addon_version };
    Ok((info, entries))
}

/// Writes an addon file holding `files` (path, bytes), the way current tools do: for tests
/// and tools.
pub fn write(out: &Path, info: &GmaInfo, files: &[(&str, &[u8])]) -> io::Result<()> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(MAGIC);
    bytes.push(FORMAT_VERSION);
    bytes.extend_from_slice(&info.steam_id.to_le_bytes());
    bytes.extend_from_slice(&info.timestamp.to_le_bytes());
    for required in &info.required_content {
        push_cstr(&mut bytes, required);
    }
    bytes.push(0);
    push_cstr(&mut bytes, &info.title);
    push_cstr(&mut bytes, &info.description);
    push_cstr(&mut bytes, &info.author);
    bytes.extend_from_slice(&info.addon_version.to_le_bytes());
    for (i, (name, data)) in files.iter().enumerate() {
        let number = u32::try_from(i + 1).map_err(|_| invalid("too many files"))?;
        bytes.extend_from_slice(&number.to_le_bytes());
        push_cstr(&mut bytes, &name.replace('\\', "/").to_lowercase());
        bytes.extend_from_slice(&(data.len() as i64).to_le_bytes());
        bytes.extend_from_slice(&crc32(data).to_le_bytes());
    }
    bytes.extend_from_slice(&0u32.to_le_bytes());
    for (_, data) in files {
        bytes.extend_from_slice(data);
    }
    let trailer = crc32(&bytes);
    bytes.extend_from_slice(&trailer.to_le_bytes());
    let mut file = File::create(out)?;
    file.write_all(&bytes)?;
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

    fn info() -> GmaInfo {
        GmaInfo {
            format_version: FORMAT_VERSION,
            steam_id: 0,
            timestamp: 1_700_000_000,
            required_content: Vec::new(),
            title: "Test Addon".into(),
            description: r#"{"description":"A test","type":"tool","tags":["fun","build"]}"#.into(),
            author: "Author Name".into(),
            addon_version: 1,
        }
    }

    #[test]
    fn round_trip() {
        let tmp = TempDir::new();
        let path = tmp.path().join("addon.gma");
        let files: [(&str, &[u8]); 3] =
            [("lua/autorun/test.lua", b"print(\"hello\")"), ("Materials/Test/Logo.vmt", b"\"UnlitGeneric\" {}"), ("models/empty.mdl", b"")];
        write(&path, &info(), &files).unwrap();

        let gma = GmaArchive::open(&path).unwrap();
        assert_eq!(gma.info(), &info());
        assert_eq!(gma.len(), 3);
        assert_eq!(gma.read("lua/autorun/test.lua").unwrap(), files[0].1);
        assert_eq!(gma.read("materials/test/logo.vmt").unwrap(), files[1].1);
        assert_eq!(gma.read("models/empty.mdl").unwrap(), b"");
        for entry in gma.entries() {
            assert!(gma.verify(entry).unwrap(), "{}", entry.path);
        }
        assert_eq!(gma.contains("lua/autorun"), Some(true));
        assert_eq!(gma.list(""), [("lua".to_string(), true), ("materials".to_string(), true), ("models".to_string(), true)]);
    }

    #[test]
    fn cut_short_and_wrong_files() {
        let tmp = TempDir::new();
        let path = tmp.path().join("addon.gma");
        write(&path, &info(), &[("lua/a.lua", b"0123456789")]).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        // without the trailer and the last bytes of the data: the download stopped early
        std::fs::write(&path, &bytes[..bytes.len() - 8]).unwrap();
        let err = GmaArchive::open(&path).unwrap_err();
        assert!(err.to_string().contains("cut short"), "{err}");

        std::fs::write(&path, b"GMAD\x09").unwrap();
        assert!(GmaArchive::open(&path).unwrap_err().to_string().contains("version 9"));
        std::fs::write(&path, [0x5D, 0, 0, 0x80, 0]).unwrap();
        assert!(GmaArchive::open(&path).unwrap_err().to_string().contains("LZMA"));
        std::fs::write(&path, b"GMAD\x03\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0title-without-end").unwrap();
        assert!(GmaArchive::open(&path).is_err());
    }
}
