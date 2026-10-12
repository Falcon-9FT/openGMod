//! The records of the lumps a map is drawn from, as they are laid out in the file.

use std::io;

use gmod_fs::lzma;

/// Indices of the lumps used here (of 64).
pub mod index {
    pub const ENTITIES: usize = 0;
    pub const PLANES: usize = 1;
    pub const TEXDATA: usize = 2;
    pub const VERTEXES: usize = 3;
    pub const NODES: usize = 5;
    pub const TEXINFO: usize = 6;
    pub const FACES: usize = 7;
    pub const LIGHTING: usize = 8;
    pub const LEAFS: usize = 10;
    pub const EDGES: usize = 12;
    pub const SURFEDGES: usize = 13;
    pub const MODELS: usize = 14;
    pub const LEAF_BRUSHES: usize = 17;
    pub const BRUSHES: usize = 18;
    pub const BRUSH_SIDES: usize = 19;
    pub const DISPINFO: usize = 26;
    pub const DISP_VERTS: usize = 33;
    pub const GAME_LUMP: usize = 35;
    pub const PAKFILE: usize = 40;
    pub const CUBEMAPS: usize = 42;
    pub const TEXDATA_STRING_DATA: usize = 43;
    pub const TEXDATA_STRING_TABLE: usize = 44;
    pub const OVERLAYS: usize = 45;
    pub const LIGHTING_HDR: usize = 53;
    pub const FACES_HDR: usize = 58;
}

/// `texinfo` flags.
pub mod surf {
    pub const LIGHT: i32 = 0x0001;
    pub const SKY2D: i32 = 0x0002;
    pub const SKY: i32 = 0x0004;
    pub const WARP: i32 = 0x0008;
    pub const TRANS: i32 = 0x0010;
    pub const TRIGGER: i32 = 0x0040;
    pub const NODRAW: i32 = 0x0080;
    pub const HINT: i32 = 0x0100;
    pub const SKIP: i32 = 0x0200;
    pub const NOLIGHT: i32 = 0x0400;
    pub const BUMPLIGHT: i32 = 0x0800;
}

/// What a brush is made of, as flags (`CONTENTS_` in GMod's Lua); a brush can be several
/// things at once, water and a ladder say.
pub mod contents {
    pub const EMPTY: u32 = 0;
    pub const SOLID: u32 = 0x1;
    pub const WINDOW: u32 = 0x2;
    pub const AUX: u32 = 0x4;
    pub const GRATE: u32 = 0x8;
    pub const SLIME: u32 = 0x10;
    pub const WATER: u32 = 0x20;
    pub const BLOCKLOS: u32 = 0x40;
    pub const OPAQUE: u32 = 0x80;
    pub const TESTFOGVOLUME: u32 = 0x100;
    pub const IGNORE_NODRAW_OPAQUE: u32 = 0x2000;
    pub const MOVEABLE: u32 = 0x4000;
    pub const AREAPORTAL: u32 = 0x8000;
    pub const PLAYERCLIP: u32 = 0x1_0000;
    pub const MONSTERCLIP: u32 = 0x2_0000;
    pub const CURRENT_0: u32 = 0x4_0000;
    pub const CURRENT_90: u32 = 0x8_0000;
    pub const CURRENT_180: u32 = 0x10_0000;
    pub const CURRENT_270: u32 = 0x20_0000;
    pub const CURRENT_UP: u32 = 0x40_0000;
    pub const CURRENT_DOWN: u32 = 0x80_0000;
    pub const ORIGIN: u32 = 0x100_0000;
    pub const MONSTER: u32 = 0x200_0000;
    pub const DEBRIS: u32 = 0x400_0000;
    pub const DETAIL: u32 = 0x800_0000;
    pub const TRANSLUCENT: u32 = 0x1000_0000;
    pub const LADDER: u32 = 0x2000_0000;
    pub const HITBOX: u32 = 0x4000_0000;
}

pub(crate) fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// Where a lump is in the file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LumpEntry {
    pub offset: u32,
    pub length: u32,
    pub version: u32,
    /// The unpacked size of an LZMA-compressed lump (0 otherwise).
    pub fourcc: u32,
}

/// The most a compressed lump may unpack to. The engine's own limits are far lower; a map from
/// a server is untrusted, and a small LZMA stream can claim gigabytes.
pub(crate) const MAX_UNPACKED: u64 = 256 << 20;

/// Unpacks an LZMA block of a map: `"LZMA"`, `u32` unpacked size, `u32` packed size, the 5
/// property bytes and the stream. Returns the bytes and the size of the block.
pub(crate) fn unpack_lzma(block: &[u8], what: &str) -> io::Result<(Vec<u8>, usize)> {
    let header = block.get(..17).filter(|h| h.starts_with(b"LZMA")).ok_or_else(|| invalid(format!("{what}: not an LZMA block")))?;
    let size = u32::from_le_bytes(header[4..8].try_into().unwrap()) as u64;
    let packed = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
    if size > MAX_UNPACKED {
        return Err(invalid(format!("{what}: unpacks to {size} bytes, more than {MAX_UNPACKED}")));
    }
    let stream = block.get(17..17 + packed).ok_or_else(|| invalid(format!("{what}: runs past its end")))?;
    let bytes = lzma::decompress_raw(&header[12..17], stream, Some(size), size).map_err(|e| invalid(format!("{what}: {e}")))?;
    Ok((bytes, 17 + packed))
}

/// The bytes of a lump, unpacked if it is LZMA-compressed.
pub(crate) fn lump_bytes<'a>(file: &'a [u8], entry: &LumpEntry, what: &str) -> io::Result<std::borrow::Cow<'a, [u8]>> {
    let start = entry.offset as usize;
    let end = start.checked_add(entry.length as usize).filter(|&end| end <= file.len());
    let Some(end) = end else {
        return Err(invalid(format!("{what} lump runs past the end of the file")));
    };
    let raw = &file[start..end];
    if raw.len() >= 17 && raw.starts_with(b"LZMA") {
        return Ok(std::borrow::Cow::Owned(unpack_lzma(raw, &format!("{what} lump"))?.0));
    }
    Ok(std::borrow::Cow::Borrowed(raw))
}

/// Little-endian fields of one fixed-size record.
pub(crate) struct Fields<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Fields<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Fields<'a> {
        Fields { bytes, pos: 0 }
    }

    fn take<const N: usize>(&mut self) -> [u8; N] {
        let out = self.bytes[self.pos..self.pos + N].try_into().unwrap();
        self.pos += N;
        out
    }

    pub(crate) fn skip(&mut self, n: usize) {
        self.pos += n;
    }

    pub(crate) fn u8(&mut self) -> u8 {
        self.take::<1>()[0]
    }

    pub(crate) fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.take())
    }

    pub(crate) fn i16(&mut self) -> i16 {
        i16::from_le_bytes(self.take())
    }

    pub(crate) fn i16x3(&mut self) -> [i16; 3] {
        [self.i16(), self.i16(), self.i16()]
    }

    pub(crate) fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take())
    }

    pub(crate) fn i32(&mut self) -> i32 {
        i32::from_le_bytes(self.take())
    }

    pub(crate) fn f32(&mut self) -> f32 {
        f32::from_le_bytes(self.take())
    }

    pub(crate) fn vec3(&mut self) -> [f32; 3] {
        [self.f32(), self.f32(), self.f32()]
    }

    pub(crate) fn vec4(&mut self) -> [f32; 4] {
        [self.f32(), self.f32(), self.f32(), self.f32()]
    }
}

/// A lump of `size`-byte records. Like the engine, a lump that is not a whole number of
/// records is refused.
pub(crate) fn records<T>(bytes: &[u8], size: usize, what: &str, parse: impl Fn(&mut Fields) -> T) -> io::Result<Vec<T>> {
    if !bytes.len().is_multiple_of(size) {
        return Err(invalid(format!("{what} lump: {} bytes is not a whole number of {size}-byte records", bytes.len())));
    }
    Ok(bytes.chunks(size).map(|chunk| parse(&mut Fields::new(chunk))).collect())
}

/// A plane: points `p` with `dot(normal, p) == dist`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plane {
    pub normal: [f32; 3],
    pub dist: f32,
    pub kind: i32,
}

impl Plane {
    pub(crate) const SIZE: usize = 20;

    pub(crate) fn parse(f: &mut Fields) -> Plane {
        Plane { normal: f.vec3(), dist: f.f32(), kind: f.i32() }
    }
}

/// A face: a convex polygon of the world or of a brush model, or the base of a displacement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Face {
    /// The plane, already facing the way the face does: front faces are wound clockwise.
    pub plane: u16,
    /// 1 when the plane is the flipped one of its pair (the plane index is odd). Nothing to
    /// turn around: [`Face::plane`] faces the right way either way.
    pub side: u8,
    pub on_node: u8,
    pub first_edge: i32,
    pub num_edges: i16,
    pub texinfo: i16,
    /// Index of the displacement built on this face, or -1.
    pub dispinfo: i16,
    pub fog_volume: i16,
    pub styles: [u8; 4],
    /// Byte offset of the face's lightmap samples in the lighting lump, or -1.
    pub light_offset: i32,
    pub area: f32,
    pub lightmap_mins: [i32; 2],
    /// Lightmap size in luxels, less one in each direction.
    pub lightmap_size: [i32; 2],
    pub original_face: i32,
    pub num_primitives: u16,
    pub first_primitive: u16,
    pub smoothing_groups: u32,
}

impl Face {
    pub(crate) const SIZE: usize = 56;

    pub(crate) fn parse(f: &mut Fields) -> Face {
        Face {
            plane: f.u16(),
            side: f.u8(),
            on_node: f.u8(),
            first_edge: f.i32(),
            num_edges: f.i16(),
            texinfo: f.i16(),
            dispinfo: f.i16(),
            fog_volume: f.i16(),
            styles: [f.u8(), f.u8(), f.u8(), f.u8()],
            light_offset: f.i32(),
            area: f.f32(),
            lightmap_mins: [f.i32(), f.i32()],
            lightmap_size: [f.i32(), f.i32()],
            original_face: f.i32(),
            num_primitives: f.u16(),
            first_primitive: f.u16(),
            smoothing_groups: f.u32(),
        }
    }
}

/// A node of a BSP tree: its plane cuts space in two, and each half goes on in a child. A
/// child `>= 0` is another node, one `< 0` a leaf (`-1 - child`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node {
    pub plane: i32,
    /// In front of the plane, then behind it.
    pub children: [i32; 2],
    pub mins: [i16; 3],
    pub maxs: [i16; 3],
    pub first_face: u16,
    pub num_faces: u16,
    pub area: i16,
}

impl Node {
    pub(crate) const SIZE: usize = 32;

    pub(crate) fn parse(f: &mut Fields) -> Node {
        Node {
            plane: f.i32(),
            children: [f.i32(), f.i32()],
            mins: f.i16x3(),
            maxs: f.i16x3(),
            first_face: f.u16(),
            num_faces: f.u16(),
            area: f.i16(),
        }
    }
}

/// A leaf of a BSP tree: a convex cell of space, with the brushes that reach into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Leaf {
    /// [`contents`] flags.
    pub contents: u32,
    /// The visibility cluster; -1 outside the world.
    pub cluster: i16,
    pub area: u16,
    pub flags: u16,
    pub mins: [i16; 3],
    pub maxs: [i16; 3],
    pub first_leaf_face: u16,
    pub num_leaf_faces: u16,
    /// The leaf's brushes: a range of the leaf brush lump.
    pub first_leaf_brush: u16,
    pub num_leaf_brushes: u16,
    pub water_data: i16,
}

impl Leaf {
    pub(crate) const SIZE: usize = 32;
    /// Version 0 of the lump (the maps of BSP version 19) keeps an ambient light cube in every
    /// leaf: six ColorRGBExp32 samples before the padding.
    pub(crate) const SIZE_WITH_LIGHT: usize = 56;

    pub(crate) fn parse(f: &mut Fields) -> Leaf {
        let contents = f.u32();
        let cluster = f.i16();
        // a 9-bit area and 7 bits of flags in one short
        let area_flags = f.u16();
        Leaf {
            contents,
            cluster,
            area: area_flags & 0x1FF,
            flags: area_flags >> 9,
            mins: f.i16x3(),
            maxs: f.i16x3(),
            first_leaf_face: f.u16(),
            num_leaf_faces: f.u16(),
            first_leaf_brush: f.u16(),
            num_leaf_brushes: f.u16(),
            water_data: f.i16(),
        }
    }
}

/// A brush: a convex solid, the space behind the planes of all its sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Brush {
    pub first_side: i32,
    pub num_sides: i32,
    /// [`contents`] flags.
    pub contents: u32,
}

impl Brush {
    pub(crate) const SIZE: usize = 12;

    pub(crate) fn parse(f: &mut Fields) -> Brush {
        Brush { first_side: f.i32(), num_sides: f.i32(), contents: f.u32() }
    }
}

/// A side of a brush.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrushSide {
    /// Its plane faces out of the brush.
    pub plane: u16,
    /// The surface, -1 for none.
    pub texinfo: i16,
    pub dispinfo: i16,
    /// A bevel: a plane the compiler adds along an edge or a corner so that a box swept
    /// against the brush stops at its edges as it does at its faces. It is no face of its own.
    pub bevel: bool,
}

impl BrushSide {
    pub(crate) const SIZE: usize = 8;

    pub(crate) fn parse(f: &mut Fields) -> BrushSide {
        // the bevel is a short in the maps of GMod, a byte followed by another flag in later
        // games: its low byte either way
        BrushSide { plane: f.u16(), texinfo: f.i16(), dispinfo: f.i16(), bevel: f.u8() != 0 }
    }
}

/// How a texture and the lightmap are laid on faces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TexInfo {
    /// `u = dot(p, s.xyz) + s.w` in texels, likewise `v`.
    pub texture_vecs: [[f32; 4]; 2],
    /// The same in luxels.
    pub lightmap_vecs: [[f32; 4]; 2],
    /// [`surf`] flags.
    pub flags: i32,
    pub texdata: i32,
}

impl TexInfo {
    pub(crate) const SIZE: usize = 72;

    pub(crate) fn parse(f: &mut Fields) -> TexInfo {
        TexInfo { texture_vecs: [f.vec4(), f.vec4()], lightmap_vecs: [f.vec4(), f.vec4()], flags: f.i32(), texdata: f.i32() }
    }
}

/// A material used by the map, with the size of its base texture when the map was compiled.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TexData {
    pub reflectivity: [f32; 3],
    /// Index into the texture name table.
    pub name_index: i32,
    pub width: i32,
    pub height: i32,
    pub view_width: i32,
    pub view_height: i32,
}

impl TexData {
    pub(crate) const SIZE: usize = 32;

    pub(crate) fn parse(f: &mut Fields) -> TexData {
        TexData { reflectivity: f.vec3(), name_index: f.i32(), width: f.i32(), height: f.i32(), view_width: f.i32(), view_height: f.i32() }
    }
}

/// A brush model: model 0 is the world, the others belong to brush entities (`*1`, `*2` ...).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Model {
    pub mins: [f32; 3],
    pub maxs: [f32; 3],
    pub origin: [f32; 3],
    pub head_node: i32,
    pub first_face: i32,
    pub num_faces: i32,
}

impl Model {
    pub(crate) const SIZE: usize = 48;

    pub(crate) fn parse(f: &mut Fields) -> Model {
        Model { mins: f.vec3(), maxs: f.vec3(), origin: f.vec3(), head_node: f.i32(), first_face: f.i32(), num_faces: f.i32() }
    }
}

/// A displacement: a grid of `(2^power + 1)²` vertices raised over a four-sided face.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DispInfo {
    /// The corner of the base face the grid starts at.
    pub start_position: [f32; 3],
    pub first_vert: i32,
    pub first_tri: i32,
    pub power: i32,
    pub min_tess: i32,
    pub smoothing_angle: f32,
    pub contents: i32,
    pub map_face: u16,
    pub lightmap_alpha_start: i32,
    pub lightmap_sample_start: i32,
}

impl DispInfo {
    /// The neighbour tables and allowed vertices that follow are not needed to draw it.
    pub(crate) const SIZE: usize = 176;

    pub(crate) fn parse(f: &mut Fields) -> DispInfo {
        let start_position = f.vec3();
        let (first_vert, first_tri, power, min_tess) = (f.i32(), f.i32(), f.i32(), f.i32());
        let smoothing_angle = f.f32();
        let contents = f.i32();
        let map_face = f.u16();
        f.skip(2);
        DispInfo {
            start_position,
            first_vert,
            first_tri,
            power,
            min_tess,
            smoothing_angle,
            contents,
            map_face,
            lightmap_alpha_start: f.i32(),
            lightmap_sample_start: f.i32(),
        }
    }

    /// Vertices along one side of the grid.
    pub fn side(&self) -> usize {
        (1usize << self.power.clamp(0, 4)) + 1
    }
}

/// One vertex of a displacement: how far it is raised, along which direction, and its blend
/// alpha (0-255, for two-texture blends).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DispVert {
    pub direction: [f32; 3],
    pub distance: f32,
    pub alpha: f32,
}

impl DispVert {
    pub(crate) const SIZE: usize = 20;

    pub(crate) fn parse(f: &mut Fields) -> DispVert {
        DispVert { direction: f.vec3(), distance: f.f32(), alpha: f.f32() }
    }
}

/// Where an `env_cubemap` was placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cubemap {
    pub origin: [i32; 3],
    pub size: i32,
}

impl Cubemap {
    pub(crate) const SIZE: usize = 16;

    pub(crate) fn parse(f: &mut Fields) -> Cubemap {
        Cubemap { origin: [f.i32(), f.i32(), f.i32()], size: f.i32() }
    }
}

/// An `info_overlay` as the compiler leaves it: a picture laid over faces of the world.
#[derive(Debug, Clone, PartialEq)]
pub struct Overlay {
    pub id: i32,
    /// Its material, through a texinfo.
    pub texinfo: i16,
    /// Of overlays on the same faces, those of a higher order are drawn over (0 to 3).
    pub render_order: u8,
    /// The faces it lies on (at most 64).
    pub faces: Vec<i32>,
    /// Texture coordinates from the first corner to the last: start and end along U and V.
    pub u: [f32; 2],
    pub v: [f32; 2],
    /// The four corners along its U and V axes from the origin; the compiler keeps the U axis
    /// in the third numbers of the first three, which lie in the plane and need none.
    pub points: [[f32; 3]; 4],
    pub origin: [f32; 3],
    /// Its normal: the faces' it was placed on.
    pub normal: [f32; 3],
}

impl Overlay {
    pub(crate) const SIZE: usize = 352;
    /// How many faces an overlay names at most.
    pub const MAX_FACES: usize = 64;

    pub(crate) fn parse(f: &mut Fields) -> Overlay {
        let id = f.i32();
        let texinfo = f.i16();
        let count = f.u16();
        let all: Vec<i32> = (0..Overlay::MAX_FACES).map(|_| f.i32()).collect();
        let faces = all[..usize::from(count & 0x3fff).min(Overlay::MAX_FACES)].to_vec();
        let (u, v) = ([f.f32(), f.f32()], [f.f32(), f.f32()]);
        let points = [f.vec3(), f.vec3(), f.vec3(), f.vec3()];
        Overlay { id, texinfo, render_order: (count >> 14) as u8, faces, u, v, points, origin: f.vec3(), normal: f.vec3() }
    }

    /// The record as the compiler writes it.
    pub fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Overlay::SIZE);
        out.extend(self.id.to_le_bytes());
        out.extend(self.texinfo.to_le_bytes());
        let count = self.faces.len().min(Overlay::MAX_FACES) as u16 | u16::from(self.render_order.min(3)) << 14;
        out.extend(count.to_le_bytes());
        for i in 0..Overlay::MAX_FACES {
            out.extend(self.faces.get(i).copied().unwrap_or(0).to_le_bytes());
        }
        let floats = [self.u.as_slice(), &self.v, self.points.as_flattened(), &self.origin, &self.normal].concat();
        out.extend(floats.iter().flat_map(|v| v.to_le_bytes()));
        out
    }
}
