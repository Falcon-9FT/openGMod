//! The world laid out for a GPU: the bytes to upload and what to draw with them. Nothing here
//! knows a graphics API, so all of it is tested without one.
//!
//! * All pieces go into one vertex and one index buffer; a [`Draw`] is a surface's range.
//! * Textures stay block-compressed when the GPU takes BC formats: DXT1, DXT3 and DXT5 are
//!   BC1, BC2 and BC3. Everything else becomes 8-bit RGBA.
//! * Light maps become half floats (RGBA, 8 bytes a texel): light can be brighter than 1.
//! * The camera looks the way Source counts angles and projects with depth reversed: 1 at
//!   the near plane, 0 at infinity, which keeps float depth precise far away.

use gmod_bsp::mesh::Lightmap;
use gmod_material::decode::to_half;
use gmod_material::vtf::flags;
use gmod_material::{Format, Vtf};

use crate::{SUN, Surface, Vec3, World, add, axes, dot, normalize, scale, sub};

/// Bytes of one vertex: position (3 `f32`), normal (3), texture coordinates (2), light map
/// coordinates (2), blend alpha (1) and vertex light (3).
pub const VERTEX_SIZE: usize = 56;

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
}

/// Every piece in one vertex buffer and one index buffer (`u32`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Geometry {
    pub vertices: Vec<u8>,
    pub indices: Vec<u8>,
    pub draws: Vec<Draw>,
}

/// Lays out the world's triangles.
pub fn geometry(world: &World) -> Geometry {
    let mut out = Geometry::default();
    let (mut vertex_count, mut index_count) = (0u32, 0u32);
    for (p, piece) in world.pieces.iter().enumerate() {
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
    /// Mipmap levels, the largest first; each as wide and high as `width >> level` (at least
    /// 1), compressed levels in whole blocks.
    pub levels: Vec<Vec<u8>>,
    pub clamp_s: bool,
    pub clamp_t: bool,
}

/// A texture's pixels from its largest level no bigger than `max_size` down. DXT stays
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
    let levels: Option<Vec<Vec<u8>>> = (first..vtf.mipmaps)
        .map(|level| match kept {
            Some(_) => vtf.image(level, 0, 0, 0).map(<[u8]>::to_vec),
            None => vtf.rgba8(level, 0, 0, 0).map(|image| image.rgba),
        })
        .collect();
    Some(TextureData {
        width,
        height,
        format: kept.unwrap_or(PixelFormat::Rgba8),
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
    TextureData { width: 64, height: 64, format: PixelFormat::Rgba8, levels, clamp_s: false, clamp_t: false }
}

/// A light map as RGBA half floats.
pub fn lightmap_data(lightmap: &Lightmap) -> Vec<u8> {
    lightmap.texels.iter().flat_map(|&[r, g, b]| [r, g, b, 1.0]).flat_map(|v| to_half(v).to_le_bytes()).collect()
}

/// How a surface is lit, for the shader.
pub const LIGHT_NONE: f32 = 0.0;
pub const LIGHT_MAP: f32 = 1.0;
pub const LIGHT_VERTEX: f32 = 2.0;

/// The 12 floats a shader gets for a surface: tint and alpha; water colour and whether it is
/// water; alpha test reference (0 for none), how it is lit, whether the second texture blends
/// in, and 0.
pub fn surface_params(surface: &Surface, has_lightmap: bool) -> [f32; 12] {
    let light = match () {
        _ if !surface.lit => LIGHT_NONE,
        _ if has_lightmap => LIGHT_MAP,
        _ => LIGHT_VERTEX,
    };
    let [r, g, b] = surface.tint;
    let water = surface.water.unwrap_or([0.0; 3]);
    [
        r,
        g,
        b,
        surface.alpha,
        water[0],
        water[1],
        water[2],
        if surface.water.is_some() { 1.0 } else { 0.0 },
        surface.alpha_test.unwrap_or(0.0),
        light,
        if surface.base2.is_some() { 1.0 } else { 0.0 },
        0.0,
    ]
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
        self.matrix(aspect, self.eye)
    }

    /// The same as seen from the world's origin: for the sky box, which goes along with the eye.
    pub fn sky_projection(&self, aspect: f32) -> [f32; 16] {
        self.matrix(aspect, [0.0; 3])
    }

    fn matrix(&self, aspect: f32, eye: Vec3) -> [f32; 16] {
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
            [0.0, 0.0, 0.0, NEAR],
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
    use crate::{Piece, SkyFace, Transform, Vertex, sky_face};

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
        let piece = |z: f32| Piece { vertices: quad(z), indices: vec![0, 1, 2, 0, 2, 3], lightmap: None, surfaces: vec![surface(0, 6)] };
        World {
            pieces: vec![piece(0.0), piece(5.0)],
            textures: Vec::new(),
            sky: [None, None, None, None, Some(SkyFace { texture: 3, transform: None }), None],
            missing_materials: Vec::new(),
            missing_textures: Vec::new(),
            missing_models: Vec::new(),
        }
    }

    #[test]
    fn one_buffer_for_all_pieces() {
        let g = geometry(&world());
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
        assert_eq!(surface_params(&s, true)[9], LIGHT_MAP);
        assert_eq!(surface_params(&s, false)[9], LIGHT_VERTEX);
        s.lit = false;
        s.water = Some([0.1, 0.2, 0.3]);
        s.alpha_test = Some(0.4);
        let p = surface_params(&s, true);
        assert_eq!((p[4], p[7], p[8], p[9]), (0.1, 1.0, 0.4, LIGHT_NONE));
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
