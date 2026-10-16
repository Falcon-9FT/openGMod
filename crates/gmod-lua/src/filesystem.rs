//! The `file` library and `File` objects. (`file.Read`, `file.Write` and `file.Append` are
//! written in Lua, in the game's `includes/extensions/file.lua`, on top of `file.Open`.)
//!
//! Search paths are those of the game file system (`GAME`, `MOD`, `WORKSHOP`, a mounted
//! game's name...), plus `LUA` (and its realm names `lsv`, `lcl`, `LuaMenu`): the `lua/` and
//! `gamemodes/` folders, as `include` sees them. `DATA` is the `data/` folder of openGMOD's
//! own folder - the only place Lua can write to, and only files with the extensions GMod
//! allows. Without a data folder ([`crate::Runtime::set_data_dir`]) `DATA` is empty and
//! writes fail.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use gmod_fs::path::{normalize, split, wildcard_match};
use mlua::{Lua, MultiValue, Table, Value};

use crate::args::Args;
use crate::host;

/// What `file.Write` may create, as in GMod.
const WRITABLE: &[&str] = &["txt", "dat", "json", "xml", "csv", "jpg", "jpeg", "png", "vtf", "vmt", "mp3", "wav", "ogg"];

const LUA_ROOTS: [&str; 2] = ["lua", "gamemodes"];

enum Place {
    /// openGMOD's `data/` folder.
    Data,
    /// Folders inside the game file system, under a search path ID.
    Game { prefixes: &'static [&'static str], id: String },
}

fn place(path_id: &str) -> Place {
    match path_id.to_ascii_uppercase().as_str() {
        "DATA" => Place::Data,
        "LUA" | "LSV" | "LCL" | "LUAMENU" => Place::Game { prefixes: &LUA_ROOTS, id: "GAME".into() },
        "THIRDPARTY" => Place::Game { prefixes: &[""], id: "GAME".into() },
        other => Place::Game { prefixes: &[""], id: other.to_string() },
    }
}

fn join(prefix: &str, path: &str) -> String {
    if prefix.is_empty() { path.to_string() } else { format!("{prefix}/{path}") }
}

/// The file on disk in the data folder for a game-style path.
fn data_path(lua: &Lua, name: &str) -> Option<PathBuf> {
    let host = host(lua);
    let dir = host.data_dir.borrow();
    let dir = dir.as_ref()?;
    let normalized = normalize(name)?;
    let relative = if normalized.is_empty() { "data".to_string() } else { format!("data/{normalized}") };
    dir.path(&relative).ok()
}

/// In GMod the `data/` folder is in the game's folder: `GAME` and `MOD` see it as `data/...`.
/// Here it is openGMOD's own (`DATA`), not the one of the Garry's Mod whose files are mounted.
fn data_within(path_id: &str, name: &str) -> Option<String> {
    if !matches!(path_id.to_ascii_uppercase().as_str(), "GAME" | "MOD") {
        return None;
    }
    let normalized = normalize(name)?;
    if normalized == "data" {
        return Some(String::new());
    }
    normalized.strip_prefix("data/").map(str::to_string)
}

fn read(lua: &Lua, name: &str, path_id: &str) -> Option<Vec<u8>> {
    if let Some(rest) = data_within(path_id, name) {
        return read(lua, &rest, "DATA");
    }
    match place(path_id) {
        Place::Data => std::fs::read(data_path(lua, name)?).ok(),
        Place::Game { prefixes, id } => {
            let vfs = host(lua).vfs.clone();
            prefixes.iter().find_map(|prefix| vfs.read(&join(prefix, name), &id).ok())
        }
    }
}

/// `Some(is a folder)` when something is there.
fn stat(lua: &Lua, name: &str, path_id: &str) -> Option<bool> {
    if let Some(rest) = data_within(path_id, name) {
        return stat(lua, &rest, "DATA");
    }
    match place(path_id) {
        Place::Data => data_path(lua, name).and_then(|p| std::fs::metadata(p).ok()).map(|m| m.is_dir()),
        Place::Game { prefixes, id } => {
            let vfs = host(lua).vfs.clone();
            prefixes.iter().map(|prefix| join(prefix, name)).find(|path| vfs.exists(path, &id)).map(|path| vfs.is_dir(&path, &id))
        }
    }
}

/// `file.Find`: names of files and folders, unsorted.
fn find(lua: &Lua, pattern: &str, path_id: &str) -> (Vec<String>, Vec<String>) {
    if let Some(rest) = data_within(path_id, pattern).filter(|rest| !rest.is_empty()) {
        return find(lua, &rest, "DATA");
    }
    match place(path_id) {
        Place::Data => {
            let Some(normalized) = normalize(pattern) else { return Default::default() };
            let (dir, name_pattern) = split(&normalized);
            let Some(folder) = data_path(lua, dir) else { return Default::default() };
            let (mut files, mut dirs) = (Vec::new(), Vec::new());
            for entry in std::fs::read_dir(folder).into_iter().flatten().flatten() {
                let name = entry.file_name().to_string_lossy().to_lowercase();
                if name.ends_with(".part") || !wildcard_match(name_pattern, &name) {
                    continue;
                }
                if entry.file_type().is_ok_and(|t| t.is_dir()) { dirs.push(name) } else { files.push(name) }
            }
            (files, dirs)
        }
        Place::Game { prefixes, id } => {
            let vfs = host(lua).vfs.clone();
            let (mut files, mut dirs) = (Vec::new(), Vec::new());
            for prefix in prefixes {
                let found = vfs.find(&join(prefix, pattern), &id);
                files.extend(found.files.into_iter().filter(|f| !files.contains(f)).collect::<Vec<_>>());
                dirs.extend(found.dirs.into_iter().filter(|d| !dirs.contains(d)).collect::<Vec<_>>());
            }
            (files, dirs)
        }
    }
}

fn modified(lua: &Lua, name: &str, path_id: &str) -> u64 {
    if let Some(rest) = data_within(path_id, name) {
        return modified(lua, &rest, "DATA");
    }
    let file = match place(path_id) {
        Place::Data => data_path(lua, name),
        Place::Game { prefixes, id } => {
            let vfs = host(lua).vfs.clone();
            prefixes.iter().find_map(|prefix| {
                let path = join(prefix, name);
                vfs.locate(&path, &id).and_then(|mount| match &mount.backing {
                    gmod_fs::Backing::Dir(_) => Some(mount.origin().join(normalize(&path)?)),
                    _ => Some(mount.origin().to_path_buf()),
                })
            })
        }
    };
    file.and_then(|f| std::fs::metadata(f).ok())
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

fn one(value: Value) -> mlua::Result<MultiValue> {
    Ok(MultiValue::from_iter([value]))
}

fn writable(name: &str) -> bool {
    let extension = gmod_fs::path::extension(name).to_ascii_lowercase();
    WRITABLE.contains(&extension.as_str())
}

/// Saves what a `File` opened for writing holds.
fn save(path: &Path, data: &[u8], append: bool) -> bool {
    if let Some(parent) = path.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return false;
    }
    if append {
        use std::io::Write;
        std::fs::OpenOptions::new().create(true).append(true).open(path).and_then(|mut f| f.write_all(data)).is_ok()
    } else {
        let mut temporary = path.to_path_buf().into_os_string();
        temporary.push(".part");
        std::fs::write(&temporary, data).and_then(|_| std::fs::rename(&temporary, path)).is_ok()
    }
}

/// An open file. Files read are read whole; files written are kept in memory and saved on
/// `Flush`, `Close`, or when Lua lets go of them.
pub struct LuaFile {
    data: Vec<u8>,
    position: usize,
    /// Where a file opened for writing goes, and whether it is appended to.
    target: Option<(PathBuf, bool)>,
    /// Written since the last save.
    dirty: bool,
}

impl LuaFile {
    fn flush(&mut self) {
        if let Some((path, append)) = &self.target
            && self.dirty
        {
            save(path, &self.data, *append);
            if *append {
                self.data.clear();
                self.position = 0;
            }
            self.dirty = false;
        }
    }

    fn take(&mut self, count: usize) -> Option<&[u8]> {
        let end = self.position.checked_add(count)?;
        let bytes = self.data.get(self.position..end)?;
        self.position = end;
        Some(bytes)
    }

    fn put(&mut self, bytes: &[u8]) {
        if self.target.is_none() {
            return;
        }
        let end = self.position + bytes.len();
        if self.data.len() < end {
            self.data.resize(end, 0);
        }
        self.data[self.position..end].copy_from_slice(bytes);
        self.position = end;
        self.dirty = true;
    }
}

impl Drop for LuaFile {
    fn drop(&mut self) {
        self.flush();
    }
}

impl mlua::UserData for LuaFile {
    fn add_fields<F: mlua::UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("MetaName", "File");
        fields.add_meta_field("MetaID", crate::meta::type_id::FILE);
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function("__index", |lua, (ud, key): (mlua::AnyUserData, Value)| crate::meta::index(lua, "File", ud, key));
        methods.add_meta_function("__newindex", |lua, (ud, key, value): (mlua::AnyUserData, Value, Value)| {
            crate::meta::new_index(lua, "File", ud, key, value)
        });
        methods.add_meta_method("__tostring", |_, file, ()| Ok(format!("File [{} bytes]", file.data.len())));
    }
}

/// `file.Open(name, mode, path)`: a `File`, or nil.
fn open(a: &Args, name: &str, mode: &str, path_id: &str) -> mlua::Result<MultiValue> {
    let write = mode.contains('w') || mode.contains('a');
    let file = if write {
        let in_data = matches!(place(path_id), Place::Data);
        match data_path(a.lua, name) {
            Some(path) if in_data && writable(name) => {
                let append = mode.contains('a');
                LuaFile { data: Vec::new(), position: 0, target: Some((path, append)), dirty: !append }
            }
            _ => return one(Value::Nil),
        }
    } else {
        match read(a.lua, name, path_id) {
            Some(data) => LuaFile { data, position: 0, target: None, dirty: false },
            None => return one(Value::Nil),
        }
    };
    one(Value::UserData(a.lua.create_userdata(file)?))
}

fn file_methods(lua: &Lua) -> mlua::Result<()> {
    let meta = crate::meta::meta_table(lua, "File", crate::meta::type_id::FILE)?;
    fn with<R>(a: &Args, f: impl FnOnce(&mut LuaFile) -> R) -> mlua::Result<R> {
        match a.get(1) {
            Some(Value::UserData(ud)) => Ok(f(&mut *ud.borrow_mut::<LuaFile>().map_err(|_| a.bad(1, "File"))?)),
            _ => Err(a.bad(1, "File")),
        }
    }
    let def = |name: &'static str, f: fn(&Args) -> mlua::Result<Value>| -> mlua::Result<()> {
        meta.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
    };
    def("Close", |a| {
        with(a, LuaFile::flush)?;
        Ok(Value::Nil)
    })?;
    def("Flush", |a| {
        with(a, LuaFile::flush)?;
        Ok(Value::Nil)
    })?;
    def("Size", |a| Ok(Value::Integer(with(a, |f| f.data.len())? as i64)))?;
    def("Tell", |a| Ok(Value::Integer(with(a, |f| f.position)? as i64)))?;
    def("EndOfFile", |a| Ok(Value::Boolean(with(a, |f| f.position >= f.data.len())?)))?;
    def("Seek", |a| {
        let to = a.number(2)?.max(0.0) as usize;
        with(a, |f| f.position = to.min(f.data.len()))?;
        Ok(Value::Nil)
    })?;
    def("Skip", |a| {
        let by = a.number(2)? as i64;
        let at = with(a, |f| {
            f.position = (f.position as i64 + by).clamp(0, f.data.len() as i64) as usize;
            f.position
        })?;
        Ok(Value::Integer(at as i64))
    })?;
    def("Read", |a| {
        let length = a.opt_number(2)?;
        let bytes = with(a, |f| {
            let left = f.data.len().saturating_sub(f.position);
            let count = length.map_or(left, |n| (n.max(0.0) as usize).min(left));
            if left == 0 { None } else { f.take(count).map(<[u8]>::to_vec) }
        })?;
        Ok(match bytes {
            Some(bytes) => Value::String(a.lua.create_string(bytes)?),
            None => Value::Nil,
        })
    })?;
    def("ReadLine", |a| {
        let line = with(a, |f| {
            let rest = f.data.get(f.position..)?;
            if rest.is_empty() {
                return None;
            }
            let length = rest.iter().position(|&b| b == b'\n').map_or(rest.len(), |i| i + 1);
            f.take(length).map(<[u8]>::to_vec)
        })?;
        Ok(match line {
            Some(line) => Value::String(a.lua.create_string(line)?),
            None => Value::Nil,
        })
    })?;
    def("ReadBool", |a| Ok(with(a, |f| f.take(1).map(|b| b[0] != 0))?.map_or(Value::Nil, Value::Boolean)))?;
    def("ReadByte", |a| Ok(number(with(a, |f| f.take(1).map(|b| b[0] as f64))?)))?;
    def("ReadShort", |a| Ok(number(with(a, |f| f.take(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f64))?)))?;
    def("ReadUShort", |a| Ok(number(with(a, |f| f.take(2).map(|b| u16::from_le_bytes([b[0], b[1]]) as f64))?)))?;
    def("ReadLong", |a| Ok(number(with(a, |f| f.take(4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64))?)))?;
    def("ReadULong", |a| Ok(number(with(a, |f| f.take(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64))?)))?;
    def("ReadFloat", |a| Ok(number(with(a, |f| f.take(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64))?)))?;
    def("ReadDouble", |a| Ok(number(with(a, |f| f.take(8).map(|b| f64::from_le_bytes(b.try_into().unwrap_or_default())))?)))?;
    def("ReadUInt64", |a| {
        let value = with(a, |f| f.take(8).map(|b| u64::from_le_bytes(b.try_into().unwrap_or_default())))?;
        Ok(match value {
            Some(value) => Value::String(a.lua.create_string(value.to_string())?),
            None => Value::Nil,
        })
    })?;
    def("Write", |a| {
        let data = match a.get(2) {
            Some(Value::String(s)) => s.as_bytes().to_vec(),
            _ => a.string(2)?.into_bytes(),
        };
        with(a, |f| f.put(&data))?;
        Ok(Value::Nil)
    })?;
    def("WriteBool", |a| {
        let value = a.boolean(2);
        with(a, |f| f.put(&[value as u8]))?;
        Ok(Value::Nil)
    })?;
    def("WriteByte", |a| {
        let value = a.number(2)? as i64 as u8;
        with(a, |f| f.put(&[value]))?;
        Ok(Value::Nil)
    })?;
    def("WriteShort", |a| {
        let value = a.number(2)? as i64 as i16;
        with(a, |f| f.put(&value.to_le_bytes()))?;
        Ok(Value::Nil)
    })?;
    def("WriteUShort", |a| {
        let value = a.number(2)? as i64 as u16;
        with(a, |f| f.put(&value.to_le_bytes()))?;
        Ok(Value::Nil)
    })?;
    def("WriteLong", |a| {
        let value = a.number(2)? as i64 as i32;
        with(a, |f| f.put(&value.to_le_bytes()))?;
        Ok(Value::Nil)
    })?;
    def("WriteULong", |a| {
        let value = a.number(2)? as i64 as u32;
        with(a, |f| f.put(&value.to_le_bytes()))?;
        Ok(Value::Nil)
    })?;
    def("WriteFloat", |a| {
        let value = a.number(2)? as f32;
        with(a, |f| f.put(&value.to_le_bytes()))?;
        Ok(Value::Nil)
    })?;
    def("WriteDouble", |a| {
        let value = a.number(2)?;
        with(a, |f| f.put(&value.to_le_bytes()))?;
        Ok(Value::Nil)
    })?;
    def("WriteUInt64", |a| {
        let value = a.string(2)?.trim().parse::<u64>().unwrap_or(0);
        with(a, |f| f.put(&value.to_le_bytes()))?;
        Ok(Value::Nil)
    })?;
    Ok(())
}

fn number(value: Option<f64>) -> Value {
    value.map_or(Value::Nil, Value::Number)
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let file = lua.create_table()?;
    let def = |name: &'static str, f: fn(&Args) -> mlua::Result<MultiValue>| -> mlua::Result<()> {
        file.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
    };
    def("Open", |a| {
        let (name, mode) = (a.string(1)?, a.string(2)?);
        let path_id = a.opt_string(3)?.unwrap_or_else(|| "GAME".into());
        open(a, &name, &mode, &path_id)
    })?;
    def("Exists", |a| one(Value::Boolean(stat(a.lua, &a.string(1)?, &a.string(2)?).is_some())))?;
    def("IsDir", |a| one(Value::Boolean(stat(a.lua, &a.string(1)?, &a.string(2)?) == Some(true))))?;
    def("Size", |a| {
        let size = read(a.lua, &a.string(1)?, &a.string(2)?).map_or(-1, |b| b.len() as i64);
        one(Value::Integer(size))
    })?;
    def("Time", |a| one(Value::Integer(modified(a.lua, &a.string(1)?, &a.string(2)?) as i64)))?;
    def("Find", |a| {
        let pattern = a.string(1)?;
        let path_id = a.string(2)?;
        let sorting = a.opt_string(3)?.unwrap_or_else(|| "nameasc".into());
        let (mut files, mut dirs) = find(a.lua, &pattern, &path_id);
        match sorting.as_str() {
            "namedesc" => {
                files.sort_by(|x, y| y.cmp(x));
                dirs.sort_by(|x, y| y.cmp(x));
            }
            "dateasc" | "datedesc" => {
                let (dir, _) = split(&pattern);
                let dir = dir.to_string();
                let time = |name: &String| modified(a.lua, &format!("{dir}/{name}"), &path_id);
                files.sort_by_key(time);
                if sorting == "datedesc" {
                    files.reverse();
                }
                dirs.sort();
            }
            _ => {
                files.sort();
                dirs.sort();
            }
        }
        let files: Table = a.lua.create_sequence_from(files)?;
        let dirs: Table = a.lua.create_sequence_from(dirs)?;
        Ok(MultiValue::from_iter([Value::Table(files), Value::Table(dirs)]))
    })?;
    def("CreateDir", |a| {
        if let Some(path) = data_path(a.lua, &a.string(1)?) {
            let _ = std::fs::create_dir_all(path);
        }
        one(Value::Nil)
    })?;
    def("Delete", |a| {
        let deleted = match (a.opt_string(2)?.as_deref().map(place).unwrap_or(Place::Data), data_path(a.lua, &a.string(1)?)) {
            (Place::Data, Some(path)) if path.is_dir() => std::fs::remove_dir(path).is_ok(),
            (Place::Data, Some(path)) => std::fs::remove_file(path).is_ok(),
            _ => false,
        };
        one(Value::Boolean(deleted))
    })?;
    def("Rename", |a| {
        let (from, to) = (a.string(1)?, a.string(2)?);
        let renamed = match (data_path(a.lua, &from), data_path(a.lua, &to)) {
            (Some(from), Some(to)) if writable(&to.to_string_lossy()) || from.is_dir() => std::fs::rename(from, to).is_ok(),
            _ => false,
        };
        one(Value::Boolean(renamed))
    })?;
    lua.globals().raw_set("file", file)?;
    file_methods(lua)?;
    Ok(())
}
