//! The game lump - a directory of further lumps, each with a four-character ID - and the
//! static props (`sprp`) in it: the models placed in the map that do not move.
//!
//! A directory entry is `u32` ID, `u16` flags, `u16` version, `i32` offset (from the start of
//! the file) and `i32` length. Flag 1 marks an LZMA-compressed lump: the length is then the
//! unpacked size, and the packed size is in the LZMA block's own header (a last, empty entry
//! closes the directory, but its offset cannot be relied on: maps have it 0).
//!
//! `sprp`: `i32` count and that many 128-byte model names; `i32` count and that many `u16`
//! leaves; `i32` count and the props. A prop's first 56 bytes are the same in every version;
//! later versions add fields after them, so the record size is taken from the version.

use std::io;

use crate::lumps::{Fields, invalid, unpack_lzma};

/// One lump of the game lump directory, unpacked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameLump {
    /// The ID as written (`b"sprp"`, `b"dprp"` ...).
    pub id: [u8; 4],
    pub flags: u16,
    pub version: u16,
    pub data: Vec<u8>,
}

/// The game lumps of a map; `directory` is the game lump itself, `file` the whole map.
pub fn game_lumps(directory: &[u8], file: &[u8]) -> io::Result<Vec<GameLump>> {
    if directory.is_empty() {
        return Ok(Vec::new());
    }
    if directory.len() < 4 {
        return Err(invalid("game lump: too short"));
    }
    let count = i32::from_le_bytes(directory[0..4].try_into().unwrap()).max(0) as usize;
    if directory.len() < 4 + count * 16 {
        return Err(invalid("game lump: directory runs past the lump"));
    }
    let entries: Vec<(u32, u16, u16, usize, usize)> = (0..count)
        .map(|i| {
            let mut f = Fields::new(&directory[4 + i * 16..4 + i * 16 + 16]);
            (f.u32(), f.u16(), f.u16(), f.i32().max(0) as usize, f.i32().max(0) as usize)
        })
        .collect();
    let mut out = Vec::new();
    for &(id, flags, version, offset, length) in &entries {
        if id == 0 && length == 0 {
            continue;
        }
        let name = String::from_utf8_lossy(&id.to_be_bytes()).into_owned();
        let data = if flags & 1 != 0 && length > 0 {
            unpack_lzma(file.get(offset..).unwrap_or_default(), &format!("game lump {name}"))?.0
        } else {
            file.get(offset..offset + length).ok_or_else(|| invalid(format!("game lump {name}: runs past the end of the file")))?.to_vec()
        };
        out.push(GameLump { id: id.to_be_bytes(), flags, version, data });
    }
    Ok(out)
}

/// A model placed in the map.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StaticProp {
    pub origin: [f32; 3],
    /// Pitch, yaw, roll in degrees.
    pub angles: [f32; 3],
    /// Index into [`StaticProps::models`].
    pub model: u16,
    pub first_leaf: u16,
    pub leaf_count: u16,
    pub solid: u8,
    pub flags: u8,
    pub skin: i32,
    pub fade_min: f32,
    pub fade_max: f32,
    pub lighting_origin: [f32; 3],
}

/// The static props of a map.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StaticProps {
    pub version: u16,
    /// Model paths (`models/props_c17/bench01a.mdl`).
    pub models: Vec<String>,
    pub leaves: Vec<u16>,
    pub props: Vec<StaticProp>,
}

/// Record sizes of the `sprp` versions known to be in use.
fn prop_size(version: u16) -> Option<usize> {
    match version {
        4 => Some(56),
        5 => Some(60),
        6 => Some(64),
        7 | 8 => Some(68),
        9 => Some(72),
        10 => Some(76),
        11 | 12 => Some(80),
        _ => None,
    }
}

fn read_i32(data: &[u8], pos: &mut usize, what: &str) -> io::Result<usize> {
    let bytes = data.get(*pos..*pos + 4).ok_or_else(|| invalid(format!("static props: {what} missing")))?;
    *pos += 4;
    let value = i32::from_le_bytes(bytes.try_into().unwrap());
    usize::try_from(value).map_err(|_| invalid(format!("static props: negative {what}")))
}

/// Reads an `sprp` game lump.
pub fn static_props(lump: &GameLump) -> io::Result<StaticProps> {
    let data = &lump.data;
    let mut pos = 0;
    let model_count = read_i32(data, &mut pos, "model count")?;
    let names = data.get(pos..pos + model_count * 128).ok_or_else(|| invalid("static props: model names run past the lump"))?;
    let models = names
        .chunks(128)
        .map(|name| {
            let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
            String::from_utf8_lossy(&name[..end]).into_owned()
        })
        .collect();
    pos += model_count * 128;
    let leaf_count = read_i32(data, &mut pos, "leaf count")?;
    let leaves_bytes = data.get(pos..pos + leaf_count * 2).ok_or_else(|| invalid("static props: leaves run past the lump"))?;
    let leaves = leaves_bytes.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    pos += leaf_count * 2;
    let prop_count = read_i32(data, &mut pos, "prop count")?;
    let rest = data.len() - pos;
    let size = match prop_size(lump.version) {
        Some(size) if size * prop_count <= rest => size,
        // an unknown version: the records fill the rest of the lump
        _ if prop_count > 0 && rest.is_multiple_of(prop_count) && rest / prop_count >= 56 => rest / prop_count,
        _ if prop_count == 0 => 56,
        _ => return Err(invalid(format!("static props version {}: {prop_count} props do not fit {rest} bytes", lump.version))),
    };
    let props = data[pos..pos + prop_count * size]
        .chunks(size)
        .map(|record| {
            let mut f = Fields::new(record);
            StaticProp {
                origin: f.vec3(),
                angles: f.vec3(),
                model: f.u16(),
                first_leaf: f.u16(),
                leaf_count: f.u16(),
                solid: f.u8(),
                flags: f.u8(),
                skin: f.i32(),
                fade_min: f.f32(),
                fade_max: f.f32(),
                lighting_origin: f.vec3(),
            }
        })
        .collect();
    Ok(StaticProps { version: lump.version, models, leaves, props })
}
