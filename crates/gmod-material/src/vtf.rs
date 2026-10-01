//! Valve texture files (`.vtf`), versions 7.0 to 7.5.
//!
//! The header:
//!
//! | Offset | Field |
//! |---|---|
//! | 0 | `"VTF\0"` |
//! | 4 | `u32` major version (7), `u32` minor version (0-5) |
//! | 12 | `u32` header size |
//! | 16 | `u16` width, `u16` height, `u32` flags, `u16` frames, `u16` first frame |
//! | 32 | `f32`×3 reflectivity (after 4 bytes of padding at 28), padding |
//! | 48 | `f32` bump map scale, `i32` image format, `u8` mipmap count |
//! | 57 | `i32` thumbnail format (-1: none), `u8` thumbnail width and height |
//! | 63 | 7.2 on: `u16` depth (volume textures) |
//! | 68 | 7.3 on: `u32` resource count; from 80, 8-byte resource entries |
//!
//! A resource entry is a 3-byte tag, a flags byte and a `u32`: the offset of the resource's
//! data in the file, or, with flag 2, the value itself. Tag `01 00 00` is the thumbnail,
//! `30 00 00` the image; `CRC`, `LOD`, `TSO` and `KVD` carry extra settings.
//!
//! Up to 7.2 the thumbnail (a small DXT1 image) follows the header and the image follows it.
//! The image runs from the smallest mipmap to the largest; within one, frame by frame, face by
//! face, slice by slice. A cube map (flag `ENVMAP`) has 6 faces; before 7.5 a seventh, a sphere
//! map, follows them unless the first frame is `0xFFFF`.

use std::fmt;
use std::io;

use crate::decode;
use crate::format::Format;

/// Texture flags.
pub mod flags {
    pub const POINT_SAMPLE: u32 = 0x1;
    pub const TRILINEAR: u32 = 0x2;
    pub const CLAMP_S: u32 = 0x4;
    pub const CLAMP_T: u32 = 0x8;
    pub const ANISOTROPIC: u32 = 0x10;
    pub const HINT_DXT5: u32 = 0x20;
    pub const NORMAL: u32 = 0x80;
    pub const NO_MIP: u32 = 0x100;
    pub const NO_LOD: u32 = 0x200;
    pub const ALL_MIPS: u32 = 0x400;
    pub const PROCEDURAL: u32 = 0x800;
    pub const ONE_BIT_ALPHA: u32 = 0x1000;
    pub const EIGHT_BIT_ALPHA: u32 = 0x2000;
    pub const ENVMAP: u32 = 0x4000;
    pub const RENDER_TARGET: u32 = 0x8000;
    pub const DEPTH_RENDER_TARGET: u32 = 0x10000;
    pub const NO_DEBUG_OVERRIDE: u32 = 0x20000;
    pub const SINGLE_COPY: u32 = 0x40000;
    pub const NO_DEPTH_BUFFER: u32 = 0x800000;
    pub const CLAMP_U: u32 = 0x2000000;
    pub const VERTEX_TEXTURE: u32 = 0x4000000;
    pub const SSBUMP: u32 = 0x8000000;
    pub const BORDER: u32 = 0x20000000;
}

/// Resource tags of 7.3 and later.
pub mod tag {
    pub const THUMBNAIL: [u8; 3] = [0x01, 0, 0];
    pub const IMAGE: [u8; 3] = [0x30, 0, 0];
    pub const SHEET: [u8; 3] = [0x10, 0, 0];
    pub const CRC: [u8; 3] = *b"CRC";
    pub const LOD: [u8; 3] = *b"LOD";
    pub const SETTINGS: [u8; 3] = *b"TSO";
    pub const KEY_VALUES: [u8; 3] = *b"KVD";
}

/// Resource flag: the entry holds the value, there is no data elsewhere in the file.
pub const RESOURCE_NO_DATA: u8 = 0x2;

/// One entry of the resource list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resource {
    pub tag: [u8; 3],
    pub flags: u8,
    /// An offset into the file, or the value itself with [`RESOURCE_NO_DATA`].
    pub data: u32,
}

/// The small image a texture carries for tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thumbnail {
    pub format: Format,
    pub width: u32,
    pub height: u32,
    pub offset: usize,
}

/// A decoded image: RGBA, 4 bytes a pixel, rows top to bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// A texture file.
#[derive(Clone)]
pub struct Vtf {
    /// Minor version: 7.`version`.
    pub version: u32,
    pub width: u32,
    pub height: u32,
    /// Slices of a volume texture; 1 for any other.
    pub depth: u32,
    /// [`flags`].
    pub flags: u32,
    /// Frames of an animated texture.
    pub frames: u32,
    pub first_frame: u16,
    pub reflectivity: [f32; 3],
    pub bump_scale: f32,
    pub format: Format,
    pub mipmaps: u32,
    /// 1, or 6 or 7 for a cube map.
    pub faces: u32,
    pub thumbnail: Option<Thumbnail>,
    pub resources: Vec<Resource>,
    /// Where the image data starts; `None` when the format is unknown and its size cannot be
    /// worked out.
    image_offset: Option<usize>,
    bytes: Vec<u8>,
}

impl fmt::Debug for Vtf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VTF 7.{} {} {}x{}", self.version, self.format.name(), self.width, self.height)?;
        if self.depth > 1 {
            write!(f, "x{}", self.depth)?;
        }
        write!(f, ", {} mipmaps, {} frames, {} faces, flags {:#x}", self.mipmaps, self.frames, self.faces, self.flags)
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// The most resources a file may list; the engine's own limit.
const MAX_RESOURCES: usize = 32;

impl Vtf {
    /// Reads a texture and checks that every image it declares is in the file.
    pub fn parse(bytes: Vec<u8>) -> io::Result<Vtf> {
        if bytes.len() < 64 || !bytes.starts_with(b"VTF\0") {
            return Err(invalid("not a VTF texture"));
        }
        let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let u16_at = |at: usize| u32::from(u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap()));
        let f32_at = |at: usize| f32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let (major, version) = (u32_at(4), u32_at(8));
        if major != 7 || version > 5 {
            return Err(invalid(format!("VTF version {major}.{version} is not known")));
        }
        let header_size = u32_at(12) as usize;
        let fixed = if version >= 2 { 80 } else { 64 };
        if header_size < fixed || header_size > bytes.len() {
            return Err(invalid(format!("header size {header_size} does not fit the file")));
        }
        let (width, height) = (u16_at(16), u16_at(18));
        if width == 0 || height == 0 {
            return Err(invalid("texture of size zero"));
        }
        let flags = u32_at(20);
        let frames = u16_at(24).max(1);
        let first_frame = u16_at(26) as u16;
        let reflectivity = [f32_at(32), f32_at(36), f32_at(40)];
        let bump_scale = f32_at(48);
        let format = Format::from_raw(u32_at(52) as i32).ok_or_else(|| invalid("texture without an image format"))?;
        let mipmaps = u32::from(bytes[56]).max(1);
        let thumbnail_format = Format::from_raw(u32_at(57) as i32);
        let (thumbnail_width, thumbnail_height) = (u32::from(bytes[61]), u32::from(bytes[62]));
        let depth = if version >= 2 && bytes.len() >= 65 { u16_at(63).max(1) } else { 1 };

        let mut resources = Vec::new();
        if version >= 3 {
            let count = u32_at(68) as usize;
            if count > MAX_RESOURCES || 80 + count * 8 > header_size {
                return Err(invalid(format!("{count} resources do not fit the header")));
            }
            for i in 0..count {
                let at = 80 + i * 8;
                resources.push(Resource { tag: bytes[at..at + 3].try_into().unwrap(), flags: bytes[at + 3], data: u32_at(at + 4) });
            }
        }
        let resource = |tag: [u8; 3]| resources.iter().find(|r| r.tag == tag && r.flags & RESOURCE_NO_DATA == 0).map(|r| r.data as usize);

        let thumbnail_size = match thumbnail_format {
            Some(format) => {
                format.image_size(thumbnail_width, thumbnail_height).ok_or_else(|| invalid("thumbnail in an unknown format"))?
            }
            None => 0,
        };
        let thumbnail_offset = resource(tag::THUMBNAIL).unwrap_or(header_size);
        let thumbnail = match thumbnail_format {
            Some(format) if thumbnail_size > 0 => {
                if thumbnail_offset.checked_add(thumbnail_size).is_none_or(|end| end > bytes.len()) {
                    return Err(invalid("thumbnail runs past the end of the file"));
                }
                Some(Thumbnail { format, width: thumbnail_width, height: thumbnail_height, offset: thumbnail_offset })
            }
            _ => None,
        };
        let image_offset = if version >= 3 { resource(tag::IMAGE) } else { None }.unwrap_or(header_size + thumbnail_size);

        let mut vtf = Vtf {
            version,
            width,
            height,
            depth,
            flags,
            frames,
            first_frame,
            reflectivity,
            bump_scale,
            format,
            mipmaps,
            faces: 1,
            thumbnail,
            resources,
            image_offset: None,
            bytes,
        };
        if flags & flags::ENVMAP != 0 {
            vtf.faces = 6;
            // the sphere map: present when the file holds it
            if version < 5 && first_frame != 0xFFFF {
                vtf.faces = 7;
                if vtf.data_size().is_some_and(|size| image_offset + size > vtf.bytes.len()) {
                    vtf.faces = 6;
                }
            }
        }
        if let Some(size) = vtf.data_size() {
            if image_offset.checked_add(size).is_none_or(|end| end > vtf.bytes.len()) {
                return Err(invalid(format!(
                    "{} {}x{}: {} bytes of image data from offset {image_offset}, but the file has {}",
                    format.name(),
                    width,
                    height,
                    size,
                    vtf.bytes.len()
                )));
            }
            vtf.image_offset = Some(image_offset);
        }
        Ok(vtf)
    }

    pub fn is_cubemap(&self) -> bool {
        self.flags & flags::ENVMAP != 0
    }

    /// Width, height and depth of mipmap `level` (0 is the full size).
    pub fn mip_size(&self, level: u32) -> (u32, u32, u32) {
        let at = |size: u32| (size >> level.min(31)).max(1);
        (at(self.width), at(self.height), at(self.depth))
    }

    /// Bytes of all images of one mipmap level; `None` if the format is unknown.
    fn level_size(&self, level: u32) -> Option<usize> {
        let (w, h, d) = self.mip_size(level);
        let images = d as usize * self.faces as usize * self.frames as usize;
        // a header can claim more than memory holds; such a texture fails the size check
        Some(self.format.image_size(w, h)?.saturating_mul(images))
    }

    /// Bytes of the image data, all levels.
    fn data_size(&self) -> Option<usize> {
        (0..self.mipmaps).try_fold(0usize, |sum, level| Some(sum.saturating_add(self.level_size(level)?)))
    }

    /// The stored bytes of one image: mipmap `level`, `frame`, `face` and depth `slice`.
    /// `None` if any is out of range or the format is unknown.
    pub fn image(&self, level: u32, frame: u32, face: u32, slice: u32) -> Option<&[u8]> {
        let (w, h, d) = self.mip_size(level);
        if level >= self.mipmaps || frame >= self.frames || face >= self.faces || slice >= d {
            return None;
        }
        let size = self.format.image_size(w, h)?;
        // smaller levels come first
        let before: usize = (level + 1..self.mipmaps).map(|l| self.level_size(l)).sum::<Option<usize>>()?;
        let index = (frame as usize * self.faces as usize + face as usize) * d as usize + slice as usize;
        let start = self.image_offset? + before + index * size;
        self.bytes.get(start..start + size)
    }

    /// One image decoded to RGBA; `None` as for [`Vtf::image`], or if the format cannot be shown.
    pub fn rgba8(&self, level: u32, frame: u32, face: u32, slice: u32) -> Option<Image> {
        let (width, height, _) = self.mip_size(level);
        let data = self.image(level, frame, face, slice)?;
        Some(Image { width, height, rgba: decode::to_rgba8(self.format, width, height, data)? })
    }

    /// The largest mipmap level no wider or taller than `max`, or the smallest there is.
    pub fn level_within(&self, max: u32) -> u32 {
        (0..self.mipmaps).find(|&level| self.mip_size(level).0 <= max && self.mip_size(level).1 <= max).unwrap_or(self.mipmaps - 1)
    }

    /// The thumbnail decoded to RGBA.
    pub fn thumbnail_rgba8(&self) -> Option<Image> {
        let t = self.thumbnail?;
        let data = self.bytes.get(t.offset..t.offset + t.format.image_size(t.width, t.height)?)?;
        Some(Image { width: t.width, height: t.height, rgba: decode::to_rgba8(t.format, t.width, t.height, data)? })
    }

    /// The value of a resource that has no data of its own (`CRC`, `LOD`, `TSO`).
    pub fn resource_value(&self, tag: [u8; 3]) -> Option<u32> {
        self.resources.iter().find(|r| r.tag == tag && r.flags & RESOURCE_NO_DATA != 0).map(|r| r.data)
    }

    /// The whole file.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
