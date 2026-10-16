//! The world laid out for a GPU: the bytes to upload and what to draw with them. Nothing here
//! knows a graphics API, so all of it is tested without one.
//!
//! * All pieces go into one vertex and one index buffer; a [`Draw`] is a surface's range.
//! * Textures stay block-compressed when the GPU takes BC formats: DXT1, DXT3 and DXT5 are
//!   BC1, BC2 and BC3. Everything else becomes 8-bit RGBA. The frames of an animated texture
//!   go along, to be layers of one texture; a surface's parameters say which one shows.
//! * Light maps become half floats (RGBA, 8 bytes a texel): light can be brighter than 1.
//! * The camera looks the way Source counts angles and projects with depth reversed: 1 at
//!   the near plane, 0 at infinity, which keeps float depth precise far away.

use gmod_bsp::mesh::Lightmap;
use gmod_material::decode::to_half;
use gmod_material::vtf::flags;
use gmod_material::{Format, Vtf};

use crate::sky::Fog;
use crate::{Look, Piece, SUN, Surface, Vec3, World, add, axes, dot, normalize, scale, sub};

/// Bytes of one vertex: position (3 `f32`), normal (3), texture coordinates (2), light map
/// coordinates (2), blend alpha (1) and vertex light (3).
pub const VERTEX_SIZE: usize = 56;

/// Bytes of the bones of one vertex of a model: three bone numbers and a byte of nothing, then
/// their three weights (`f32`).
pub const SKIN_SIZE: usize = 16;

/// The bones of every vertex, in the order of [`geometry`]'s vertices; vertices of pieces
/// that do not move have their first bone's whole weight.
pub fn skin(pieces: &[Piece]) -> Vec<u8> {
    let mut out = Vec::new();
    for piece in pieces {
        for i in 0..piece.vertices.len() {
            let w = piece.skin.get(i).copied().unwrap_or(crate::Weights { bones: [0; 3], weights: [1.0, 0.0, 0.0] });
            out.extend([w.bones[0], w.bones[1], w.bones[2], 0]);
            out.extend(w.weights.iter().flat_map(|x| x.to_le_bytes()));
        }
    }
    out
}

/// How many bones a world's vertices need: one past the highest they name; 0 for one that
/// does not move.
pub fn bone_count(pieces: &[Piece]) -> usize {
    let named = pieces.iter().flat_map(|p| &p.skin).flat_map(|w| (0..3).filter(|&i| w.weights[i] > 0.0).map(|i| w.bones[i]));
    named.max().map_or(0, |b| usize::from(b) + 1)
}

/// One draw call: a surface of a piece.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Draw {
    pub piece: usize,
    pub surface: usize,
    /// The range in the index buffer.
    pub first_index: u32,
    pub index_count: u32,
    /// The middle of the surface, to sort translucent ones by distance.
    pub center: Vec3,
    /// An overlay ([`crate::Surface::overlay`]): drawn in order, not by distance.
    pub overlay: bool,
}

/// Every piece in one vertex buffer and one index buffer (`u32`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Geometry {
    pub vertices: Vec<u8>,
    pub indices: Vec<u8>,
    pub draws: Vec<Draw>,
}

/// Lays out the world's triangles.
pub fn geometry(pieces: &[Piece]) -> Geometry {
    let mut out = Geometry::default();
    let (mut vertex_count, mut index_count) = (0u32, 0u32);
    for (p, piece) in pieces.iter().enumerate() {
        for v in &piece.vertices {
            let values = [
                v.position[0],
                v.position[1],
                v.position[2],
                v.normal[0],
                v.normal[1],
                v.normal[2],
                v.uv[0],
                v.uv[1],
                v.lightmap_uv[0],
                v.lightmap_uv[1],
                v.alpha,
                v.light[0],
                v.light[1],
                v.light[2],
            ];
            out.vertices.extend(values.iter().flat_map(|x| x.to_le_bytes()));
        }
        out.indices.extend(piece.indices.iter().flat_map(|&i| (vertex_count + i).to_le_bytes()));
        for (s, surface) in piece.surfaces.iter().enumerate() {
            let indices = &piece.indices[surface.first..surface.first + surface.count];
            let sum = indices.iter().fold([0.0; 3], |acc, &i| add(acc, piece.vertices[i as usize].position));
            out.draws.push(Draw {
                piece: p,
                surface: s,
                first_index: index_count + surface.first as u32,
                index_count: surface.count as u32,
                center: scale(sum, 1.0 / indices.len().max(1) as f32),
                overlay: surface.overlay,
            });
        }
        vertex_count += piece.vertices.len() as u32;
        index_count += piece.indices.len() as u32;
    }
    out
}

/// How texels are stored for the GPU; all colour formats are sRGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Bc1,
    Bc2,
    Bc3,
    Bgra8,
    Rgba8,
}

impl PixelFormat {
    /// Side of a block in texels (1 for uncompressed formats) and its bytes.
    pub fn block(self) -> (u32, u32) {
        match self {
            PixelFormat::Bc1 => (4, 8),
            PixelFormat::Bc2 | PixelFormat::Bc3 => (4, 16),
            PixelFormat::Bgra8 | PixelFormat::Rgba8 => (1, 4),
        }
    }

    /// Bytes in a row of blocks and rows of blocks, for a level of `width` x `height`.
    pub fn layout(self, width: u32, height: u32) -> (u32, u32) {
        let (side, bytes) = self.block();
        (width.div_ceil(side) * bytes, height.div_ceil(side))
    }
}

/// A texture's pixels, ready to upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextureData {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    /// The frames of an animated texture; 1 for any other.
    pub frames: u32,
    /// Mipmap levels, the largest first; each as wide and high as `width >> level` (at least
    /// 1), compressed levels in whole blocks; each level's frames one after another.
    pub levels: Vec<Vec<u8>>,
    pub clamp_s: bool,
    pub clamp_t: bool,
}

/// The most frames of an animated texture uploaded: the layers a GPU surely has.
pub const MAX_FRAMES: u32 = 256;

/// A texture's pixels from its largest level no bigger than `max_size` down, every frame. DXT stays
/// compressed when `compressed` (the GPU takes BC) and the size is whole blocks; anything else
/// is decoded. `None` when the pixels cannot be decoded.
pub fn texture_data(vtf: &Vtf, compressed: bool, max_size: u32) -> Option<TextureData> {
    let first = vtf.level_within(max_size);
    let (width, height, _) = vtf.mip_size(first);
    let whole_blocks = width.is_multiple_of(4) && height.is_multiple_of(4);
    let kept = match vtf.format {
        Format::Dxt1 | Format::Dxt1OneBitAlpha if compressed && whole_blocks => Some(PixelFormat::Bc1),
        Format::Dxt3 if compressed && whole_blocks => Some(PixelFormat::Bc2),
        Format::Dxt5 if compressed && whole_blocks => Some(PixelFormat::Bc3),
        Format::Bgra8888 => Some(PixelFormat::Bgra8),
        Format::Rgba8888 => Some(PixelFormat::Rgba8),
        _ => None,
    };
    let frames = vtf.frames.clamp(1, MAX_FRAMES);
    let levels: Option<Vec<Vec<u8>>> = (first..vtf.mipmaps)
        .map(|level| {
            let frame = |frame: u32| match kept {
                Some(_) => vtf.image(level, frame, 0, 0).map(<[u8]>::to_vec),
                None => vtf.rgba8(level, frame, 0, 0).map(|image| image.rgba),
            };
            (0..frames).map(frame).collect::<Option<Vec<Vec<u8>>>>().map(|frames| frames.concat())
        })
        .collect();
    Some(TextureData {
        width,
        height,
        format: kept.unwrap_or(PixelFormat::Rgba8),
        frames,
        levels: levels?,
        clamp_s: vtf.flags & flags::CLAMP_S != 0,
        clamp_t: vtf.flags & flags::CLAMP_T != 0,
    })
}

/// The purple and black squares the game shows for a texture it does not have: 4 x 4 squares
/// across the texture, 64 texels wide, with its mipmaps.
pub fn missing_texture() -> TextureData {
    let levels = (0..7)
        .map(|level| {
            let side = 64u32 >> level;
            let cell = (side / 4).max(1);
            (0..side * side)
                .flat_map(|i| {
                    let (x, y) = (i % side, i / side);
                    if (x / cell + y / cell).is_multiple_of(2) { [255, 0, 255, 255] } else { [0, 0, 0, 255] }
                })
                .collect()
        })
        .collect();
    TextureData { width: 64, height: 64, format: PixelFormat::Rgba8, frames: 1, levels, clamp_s: false, clamp_t: false }
}

/// A light map as RGBA half floats.
pub fn lightmap_data(lightmap: &Lightmap) -> Vec<u8> {
    lightmap.texels.iter().flat_map(|&[r, g, b]| [r, g, b, 1.0]).flat_map(|v| to_half(v).to_le_bytes()).collect()
}

/// How a surface is lit, for the shader.
pub const LIGHT_NONE: f32 = 0.0;
pub const LIGHT_MAP: f32 = 1.0;
pub const LIGHT_VERTEX: f32 = 2.0;

/// How fog covers a surface: towards the fog's colour; towards nothing, for what adds its
/// light; not at all (`$nofog`).
pub const FOG_COLOR: f32 = 0.0;
pub const FOG_DARK: f32 = 1.0;
pub const FOG_NONE: f32 = 2.0;

/// How many floats a shader gets for a surface.
pub const SURFACE_PARAMS: usize = 24;

/// The floats a shader gets for a surface as it `look`s at the moment: tint and alpha; water
/// colour and whether it is water; alpha test reference (0 for none), how it is lit, whether
/// the second texture blends in, and how fog covers it; the two rows of the base texture's
/// transform with the layer of the frame that shows (`frame`, wrapped to the texture's
/// `frames`) and whether it modulates.
pub fn surface_params(surface: &Surface, look: &Look, frames: u32, has_lightmap: bool) -> [f32; SURFACE_PARAMS] {
    let light = match () {
        _ if !surface.lit => LIGHT_NONE,
        _ if has_lightmap => LIGHT_MAP,
        _ => LIGHT_VERTEX,
    };
    let [r, g, b] = look.tint;
    let water = surface.water.unwrap_or([0.0; 3]);
    let [u, v] = look.transform;
    [
        r,
        g,
        b,
        look.alpha,
        water[0],
        water[1],
        water[2],
        if surface.water.is_some() { 1.0 } else { 0.0 },
        surface.alpha_test.unwrap_or(0.0),
        light,
        if surface.base2.is_some() { 1.0 } else { 0.0 },
        match () {
            _ if surface.no_fog => FOG_NONE,
            _ if surface.additive => FOG_DARK,
            _ => FOG_COLOR,
        },
        u[0],
        u[1],
        u[2],
        (look.frame % frames.max(1)) as f32,
        v[0],
        v[1],
        v[2],
        if surface.modulate { 1.0 } else { 0.0 },
        // the detail: how much finer, how much of it, how it mixes in, and 1 when there is one
        surface.detail.map_or(1.0, |d| d.scale),
        surface.detail.map_or(0.0, |d| d.blend),
        surface.detail.map_or(0.0, |d| d.mode as f32),
        if surface.detail.is_some() { 1.0 } else { 0.0 },
    ]
}

/// The 8 floats a shader gets for fog: its colour and how thick it gets at most; where it
/// starts, one over how far it thickens, and 1 when there is fog (all 0 when there is none).
pub fn fog_params(fog: Option<&Fog>) -> [f32; 8] {
    match fog {
        Some(fog) => {
            let [r, g, b] = fog.color;
            [r, g, b, fog.max_density, fog.start, fog.inverse_range(), 1.0, 0.0]
        }
        None => [0.0; 8],
    }
}

/// The camera, with angles as Source counts them: yaw 0 looks along +x and 90 along +y,
/// positive pitch looks down.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    pub eye: Vec3,
    /// Degrees.
    pub yaw: f32,
    pub pitch: f32,
    /// Field of view in degrees, as the game counts it: across a 4:3 picture.
    pub fov: f32,
}

/// Nothing closer than this is drawn.
pub const NEAR: f32 = 4.0;

/// Matrices of four columns, as the shaders take them.
pub fn multiply(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    std::array::from_fn(|i| {
        let (row, column) = (i % 4, i / 4);
        (0..4).map(|k| a[k * 4 + row] * b[column * 4 + k]).sum()
    })
}

/// What a model drawn at `origin`, turned by `angles`, needs: the matrix from its space to the
/// world's, and where the light of [`SUN`] comes from in its space.
pub fn placement(origin: Vec3, angles: Vec3) -> ([f32; 16], Vec3) {
    let [forward, left, up] = axes(angles);
    let columns = [forward, left, up, origin];
    let matrix = std::array::from_fn(|i| {
        let (row, column) = (i % 4, i / 4);
        if row == 3 { if column == 3 { 1.0 } else { 0.0 } } else { columns[column][row] }
    });
    let sun = normalize(SUN);
    (matrix, [dot(forward, sun), dot(left, sun), dot(up, sun)])
}

impl Camera {
    /// A camera at `eye` looking at `target`.
    pub fn looking_at(eye: Vec3, target: Vec3, fov: f32) -> Camera {
        let d = sub(target, eye);
        let yaw = d[1].atan2(d[0]).to_degrees();
        let pitch = (-d[2]).atan2(d[0].hypot(d[1])).to_degrees();
        Camera { eye, yaw, pitch, fov }
    }

    pub fn forward(&self) -> Vec3 {
        let (yaw, pitch) = (self.yaw.to_radians(), self.pitch.to_radians());
        [pitch.cos() * yaw.cos(), pitch.cos() * yaw.sin(), -pitch.sin()]
    }

    /// To the right of the view, level with the ground.
    pub fn right(&self) -> Vec3 {
        let yaw = self.yaw.to_radians();
        [yaw.sin(), -yaw.cos(), 0.0]
    }

    /// The matrix from the world to clip space, column by column as WGSL reads it.
    pub fn view_projection(&self, aspect: f32) -> [f32; 16] {
        self.matrix(aspect, self.eye, NEAR)
    }

    /// The same, cutting off only what is nearer than `near`: for what is held close to the
    /// eyes.
    pub fn view_projection_near(&self, aspect: f32, near: f32) -> [f32; 16] {
        self.matrix(aspect, self.eye, near)
    }

    /// The same as seen from the world's origin: for the sky box, which goes along with the eye.
    pub fn sky_projection(&self, aspect: f32) -> [f32; 16] {
        self.matrix(aspect, [0.0; 3], NEAR)
    }

    fn matrix(&self, aspect: f32, eye: Vec3, near: f32) -> [f32; 16] {
        let forward = self.forward();
        let right = self.right();
        let up = normalize(crate::cross(right, forward));
        // the game's field of view spans a 4:3 picture; the height decides
        let half_height = (self.fov.clamp(1.0, 170.0).to_radians() / 2.0).tan() * 0.75;
        let fy = 1.0 / half_height;
        let fx = fy / aspect;
        let rows = [
            [fx * right[0], fx * right[1], fx * right[2], -fx * dot(right, eye)],
            [fy * up[0], fy * up[1], fy * up[2], -fy * dot(up, eye)],
            [0.0, 0.0, 0.0, near],
            [forward[0], forward[1], forward[2], -dot(forward, eye)],
        ];
        std::array::from_fn(|i| rows[i % 4][i / 4])
    }
}

/// One face of the sky box as two triangles around the eye: each corner a direction (x, y, z)
/// and texture coordinates (u, v), the face's texture transform applied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkyQuad {
    pub texture: usize,
    pub corners: [[f32; 5]; 4],
}

/// The faces of the sky box that have a texture.
pub fn sky_quads(world: &World) -> Vec<SkyQuad> {
    let mut quads = Vec::new();
    for (face, sky) in world.sky.iter().enumerate() {
        let Some(sky) = sky else { continue };
        // the direction of each corner from its (s, t) on the face, as `sky_face` reads it back
        let direction = |s: f32, t: f32| -> Vec3 {
            match face {
                0 => [1.0, -s, t],
                1 => [-1.0, s, t],
                2 => [s, 1.0, t],
                3 => [-s, -1.0, t],
                4 => [-t, -s, 1.0],
                _ => [t, -s, -1.0],
            }
        };
        let corners = [(-1.0, 1.0), (1.0, 1.0), (1.0, -1.0), (-1.0, -1.0)].map(|(s, t)| {
            let uv = [(s + 1.0) / 2.0, (1.0 - t) / 2.0];
            let [u, v] = sky.transform.map_or(uv, |transform| transform.apply(uv));
            let [x, y, z] = direction(s, t);
            [x, y, z, u, v]
        });
        quads.push(SkyQuad { texture: sky.texture, corners });
    }
    quads
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SkyFace, Transform, Vertex, sky_face};

    fn surface(first: usize, count: usize) -> Surface {
        Surface { base: Some(0), missing: false, ..Surface::plain("m", first, count) }
    }

    fn world() -> World {
        let quad = |z: f32| -> Vec<Vertex> {
            [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]]
                .iter()
                .map(|&[x, y]| Vertex { position: [x, y, z], light: [0.5; 3], ..Vertex::default() })
                .collect()
        };
        let piece = |z: f32| Piece {
            vertices: quad(z),
            indices: vec![0, 1, 2, 0, 2, 3],
            lightmap: None,
            surfaces: vec![surface(0, 6)],
            skin: Vec::new(),
        };
        World {
            pieces: vec![piece(0.0), piece(5.0)],
            textures: Vec::new(),
            sky: [None, None, None, None, Some(SkyFace { texture: 3, transform: None }), None],
            skybox: None,
            fog: None,
            missing_materials: Vec::new(),
            missing_textures: Vec::new(),
            missing_models: Vec::new(),
        }
    }

    #[test]
    fn the_bones_of_the_vertices() {
        let mut world = world();
        // the world does not move: every vertex its first bone's, and no bones needed
        assert_eq!(skin(&world.pieces).len(), 8 * SKIN_SIZE);
        assert_eq!(bone_count(&world.pieces), 0);
        world.pieces[1].skin = vec![crate::Weights { bones: [2, 7, 0], weights: [0.5, 0.5, 0.0] }; 4];
        let bytes = skin(&world.pieces);
        assert_eq!(&bytes[..8], [0, 0, 0, 0, 0, 0, 0x80, 0x3f], "the first piece: bone 0, weight 1");
        assert_eq!(&bytes[4 * SKIN_SIZE..4 * SKIN_SIZE + 8], [2, 7, 0, 0, 0, 0, 0, 0x3f]);
        assert_eq!(bone_count(&world.pieces), 8);
    }

    #[test]
    fn one_buffer_for_all_pieces() {
        let g = geometry(&world().pieces);
        assert_eq!(g.vertices.len(), 8 * VERTEX_SIZE);
        let indices: Vec<u32> = g.indices.chunks(4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).collect();
        assert_eq!(indices, [0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7]);
        assert_eq!(g.draws.len(), 2);
        assert_eq!((g.draws[1].piece, g.draws[1].first_index, g.draws[1].index_count), (1, 6, 6));
        assert_eq!(g.draws[1].center, [5.0, 5.0, 5.0]);
        // the second piece's first vertex: position, then the light at the end
        let at = 4 * VERTEX_SIZE;
        let float = |k: usize| f32::from_le_bytes(g.vertices[at + k * 4..at + k * 4 + 4].try_into().unwrap());
        assert_eq!((float(2), float(11), float(13)), (5.0, 0.5, 0.5));
    }

    #[test]
    fn surface_params_for_the_shader() {
        let mut s = surface(0, 3);
        let params = |s: &Surface, lightmap: bool| surface_params(s, &s.look(0.0), 1, lightmap);
        assert_eq!(params(&s, true)[9], LIGHT_MAP);
        assert_eq!(params(&s, false)[9], LIGHT_VERTEX);
        s.lit = false;
        s.water = Some([0.1, 0.2, 0.3]);
        s.alpha_test = Some(0.4);
        let p = params(&s, true);
        assert_eq!((p[4], p[7], p[8], p[9], p[11]), (0.1, 1.0, 0.4, LIGHT_NONE, FOG_COLOR));
        s.additive = true;
        assert_eq!(params(&s, true)[11], FOG_DARK);
        s.no_fog = true;
        assert_eq!(params(&s, true)[11], FOG_NONE);
        // no transform: the identity, frame 0, not modulating
        assert_eq!(params(&s, true)[12..20], [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        // no detail
        assert_eq!(params(&s, true)[20..], [1.0, 0.0, 0.0, 0.0]);
        // the look of the moment: its tint, alpha, transform and frame, wrapped to the frames
        let look = Look { frame: 7, transform: [[2.0, 0.0, 0.5], [0.0, 2.0, 0.25]], tint: [0.5; 3], alpha: 0.5 };
        s.modulate = true;
        let p = surface_params(&s, &look, 4, true);
        assert_eq!((p[0], p[3]), (0.5, 0.5));
        assert_eq!(p[12..20], [2.0, 0.0, 0.5, 3.0, 0.0, 2.0, 0.25, 1.0]);
        // a detail: how fine, how much, its mode, there
        s.detail = Some(crate::Detail { texture: 0, scale: 8.0, blend: 0.5, mode: 1 });
        assert_eq!(surface_params(&s, &look, 4, true)[20..], [8.0, 0.5, 1.0, 1.0]);
    }

    fn transform(m: &[f32; 16], p: Vec3) -> [f32; 4] {
        std::array::from_fn(|row| m[row] * p[0] + m[4 + row] * p[1] + m[8 + row] * p[2] + m[12 + row])
    }

    #[test]
    fn camera_projection() {
        let camera = Camera { eye: [100.0, 0.0, 50.0], yaw: 90.0, pitch: 0.0, fov: 90.0 };
        let near = |a: Vec3, b: Vec3| sub(a, b).iter().all(|d| d.abs() < 1e-5);
        assert!(near(camera.forward(), [0.0, 1.0, 0.0]));
        assert!(near(camera.right(), [1.0, 0.0, 0.0]));
        let m = camera.view_projection(4.0 / 3.0);
        // straight ahead: in the middle, depth 1 at the near plane and less beyond
        let c = transform(&m, [100.0, NEAR, 50.0]);
        assert!((c[2] / c[3] - 1.0).abs() < 1e-5 && c[0].abs() < 1e-5);
        let c = transform(&m, [100.0, 400.0, 50.0]);
        assert!((c[2] / c[3] - NEAR / 400.0).abs() < 1e-6);
        // with a 90 degree field of view on a 4:3 picture, 45 degrees to the right is its edge
        let c = transform(&m, [500.0, 400.0, 50.0]);
        assert!((c[0] / c[3] - 1.0).abs() < 1e-5, "{c:?}");
        // up is up
        let c = transform(&m, [100.0, 400.0, 100.0]);
        assert!(c[1] > 0.0);
        let looking = Camera::looking_at([0.0; 3], [0.0, -10.0, -10.0], 75.0);
        assert!((looking.yaw + 90.0).abs() < 1e-4 && (looking.pitch - 45.0).abs() < 1e-4, "{looking:?}");
        assert!(near(looking.forward(), normalize([0.0, -1.0, -1.0])));
    }

    #[test]
    fn models_placed_in_the_world() {
        // turned to look along y and moved: its forward is the world's y, its left the world's -x
        let (m, sun) = placement([10.0, 20.0, 30.0], [0.0, 90.0, 0.0]);
        let near = |a: [f32; 4], b: [f32; 4]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5);
        assert!(near(transform(&m, [1.0, 0.0, 0.0]), [10.0, 21.0, 30.0, 1.0]));
        assert!(near(transform(&m, [0.0, 1.0, 0.0]), [9.0, 20.0, 30.0, 1.0]));
        assert!(near(transform(&m, [0.0, 0.0, 1.0]), [10.0, 20.0, 31.0, 1.0]));
        // the light from above stays above; from the side it turns with the model
        let s = normalize(SUN);
        assert!((sun[2] - s[2]).abs() < 1e-5 && (sun[0] - s[1]).abs() < 1e-5 && (sun[1] + s[0]).abs() < 1e-5, "{sun:?}");
        // the camera after the model is the model through the camera
        let camera = Camera { eye: [0.0, -200.0, 30.0], yaw: 90.0, pitch: 0.0, fov: 90.0 }.view_projection(1.0);
        let both = multiply(&camera, &m);
        let through = transform(&camera, [10.0, 21.0, 30.0]);
        assert!(near(transform(&both, [1.0, 0.0, 0.0]), through), "{:?} {through:?}", transform(&both, [1.0, 0.0, 0.0]));
    }

    #[test]
    fn sky_quad_corners_match_the_faces() {
        let mut w = world();
        for face in 0..6 {
            w.sky[face] = Some(SkyFace { texture: face, transform: None });
        }
        let quads = sky_quads(&w);
        assert_eq!(quads.len(), 6);
        for quad in &quads {
            // points between the middle of the face and each corner read back where they are
            let middle: [f32; 3] = std::array::from_fn(|k| quad.corners.iter().map(|c| c[k]).sum::<f32>() / 4.0);
            for corner in quad.corners {
                let point: [f32; 3] = std::array::from_fn(|k| middle[k] + (corner[k] - middle[k]) * 0.9);
                let (face, uv) = sky_face(point);
                assert_eq!(face, quad.texture);
                let expected = [0.5 + (corner[3] - 0.5) * 0.9, 0.5 + (corner[4] - 0.5) * 0.9];
                assert!((uv[0] - expected[0]).abs() < 1e-5 && (uv[1] - expected[1]).abs() < 1e-5, "{uv:?} {corner:?}");
            }
        }
        w.sky[0] = Some(SkyFace { texture: 0, transform: Transform::parse("center 0 0 scale 1 2 rotate 0 translate 0 0") });
        assert_eq!(sky_quads(&w)[0].corners[2][4], 2.0);
    }

    #[test]
    fn textures_for_the_gpu() {
        let missing = missing_texture();
        assert_eq!((missing.levels.len(), missing.levels[0].len(), missing.levels[6].len()), (7, 64 * 64 * 4, 4));
        assert_eq!(&missing.levels[0][..4], [255, 0, 255, 255]);
        assert_eq!(&missing.levels[0][16 * 4..16 * 4 + 4], [0, 0, 0, 255]);
        assert_eq!(PixelFormat::Bc1.layout(2, 2), (8, 1));
        assert_eq!(PixelFormat::Bc3.layout(16, 8), (64, 2));
        assert_eq!(PixelFormat::Rgba8.layout(3, 2), (12, 2));
        let lightmap = Lightmap { width: 1, height: 1, texels: vec![[1.0, 2.0, 0.5]] };
        assert_eq!(lightmap_data(&lightmap), [0x3C00u16, 0x4000, 0x3800, 0x3C00].iter().flat_map(|h| h.to_le_bytes()).collect::<Vec<u8>>());
    }
}
