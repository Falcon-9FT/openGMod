//! `render`: a picture of a map drawn on the CPU - the world, brush entities and static props
//! of [`gmod_world`] with their textures and light, blended displacements, alpha testing,
//! translucency, water and the sky box. A preview, and a check that the readers fit together;
//! the engine itself draws with the GPU.
//!
//! Not drawn yet: models of entities, decals and overlays, the 3D sky box, fog, reflections
//! and bump maps.

use std::time::Instant;

use gmod_bsp::Bsp;
use gmod_fs::Vfs;
use gmod_material::vtf::flags;
use gmod_material::{Vtf, png};
use gmod_world::{Piece, SkyFace, Surface, World, sky_face};

/// What to draw and from where.
#[derive(Debug, Clone)]
pub struct View {
    pub width: u32,
    pub height: u32,
    /// Horizontal field of view in degrees.
    pub fov: f32,
    pub eye: Option<[f32; 3]>,
    pub target: Option<[f32; 3]>,
}

impl Default for View {
    fn default() -> View {
        View { width: 1280, height: 720, fov: 90.0, eye: None, target: None }
    }
}

/// What came out.
pub struct Picture {
    pub png: Vec<u8>,
    pub eye: [f32; 3],
    pub target: [f32; 3],
    pub triangles: usize,
    pub missing_materials: Vec<String>,
    pub missing_textures: Vec<String>,
    pub missing_models: Vec<String>,
    pub seconds: f64,
}

/// The largest mipmap used: plenty for a preview, and memory stays small.
const MAX_TEXTURE: u32 = 512;
/// Nothing closer than this to the eye is drawn.
const NEAR: f32 = 1.0;

type Vec3 = [f32; 3];

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale(a: Vec3, s: f32) -> Vec3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn normalize(a: Vec3) -> Vec3 {
    let length = dot(a, a).sqrt();
    if length > 0.0 { scale(a, 1.0 / length) } else { a }
}

fn srgb_to_linear() -> [f32; 256] {
    std::array::from_fn(|i| {
        let c = i as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    })
}

fn linear_to_srgb(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    (s * 255.0 + 0.5) as u8
}

/// One mipmap level, RGBA.
struct Level {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

/// A texture's levels from at most [`MAX_TEXTURE`] down, and how it wraps.
struct Texture {
    levels: Vec<Level>,
    clamp_s: bool,
    clamp_t: bool,
}

impl Texture {
    fn from_vtf(vtf: &Vtf) -> Option<Texture> {
        let first = vtf.level_within(MAX_TEXTURE);
        let levels: Vec<Level> = (first..vtf.mipmaps)
            .map_while(|level| vtf.rgba8(level, 0, 0, 0))
            .map(|image| Level { width: image.width, height: image.height, rgba: image.rgba })
            .collect();
        (!levels.is_empty()).then_some(Texture {
            levels,
            clamp_s: vtf.flags & flags::CLAMP_S != 0,
            clamp_t: vtf.flags & flags::CLAMP_T != 0,
        })
    }

    fn size(&self) -> (f32, f32) {
        (self.levels[0].width as f32, self.levels[0].height as f32)
    }

    /// Linear colour and alpha at `uv` (1 is the whole texture), filtered between texels, from
    /// the level for `lod`.
    fn sample(&self, uv: [f32; 2], lod: f32, linear: &[f32; 256]) -> [f32; 4] {
        let level = &self.levels[(lod.max(0.0) as usize).min(self.levels.len() - 1)];
        let (w, h) = (level.width as i64, level.height as i64);
        let x = uv[0] * w as f32 - 0.5;
        let y = uv[1] * h as f32 - 0.5;
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let texel = |xi: i64, yi: i64| {
            let xi = if self.clamp_s { xi.clamp(0, w - 1) } else { xi.rem_euclid(w) };
            let yi = if self.clamp_t { yi.clamp(0, h - 1) } else { yi.rem_euclid(h) };
            let at = ((yi * w + xi) * 4) as usize;
            let p = &level.rgba[at..at + 4];
            [linear[p[0] as usize], linear[p[1] as usize], linear[p[2] as usize], f32::from(p[3]) / 255.0]
        };
        let (a, b, c, d) = (texel(x0, y0), texel(x0 + 1, y0), texel(x0, y0 + 1), texel(x0 + 1, y0 + 1));
        std::array::from_fn(|i| (a[i] * (1.0 - fx) + b[i] * fx) * (1.0 - fy) + (c[i] * (1.0 - fx) + d[i] * fx) * fy)
    }
}

/// The purple and black squares the game shows for a texture it does not have.
fn missing_texture(uv: [f32; 2]) -> [f32; 4] {
    let cell = (uv[0] * 4.0).floor() as i64 + (uv[1] * 4.0).floor() as i64;
    if cell.rem_euclid(2) == 0 { [1.0, 0.0, 1.0, 1.0] } else { [0.0, 0.0, 0.0, 1.0] }
}

/// The eye, where it looks and the image plane.
struct Camera {
    eye: Vec3,
    right: Vec3,
    up: Vec3,
    forward: Vec3,
    /// Pixels per unit of the image plane at distance 1.
    focal: f32,
    center: [f32; 2],
}

impl Camera {
    fn new(eye: Vec3, target: Vec3, view: &View) -> Camera {
        let forward = normalize(sub(target, eye));
        let side = cross(forward, [0.0, 0.0, 1.0]);
        // looking straight up or down: any right will do
        let right = if dot(side, side) < 1e-6 { [0.0, -1.0, 0.0] } else { normalize(side) };
        let up = cross(right, forward);
        let focal = view.width as f32 / 2.0 / (view.fov.clamp(1.0, 170.0).to_radians() / 2.0).tan();
        Camera { eye, right, up, forward, focal, center: [view.width as f32 / 2.0, view.height as f32 / 2.0] }
    }

    fn to_camera(&self, p: Vec3) -> Vec3 {
        let d = sub(p, self.eye);
        [dot(d, self.right), dot(d, self.up), dot(d, self.forward)]
    }

    fn direction(&self, x: f32, y: f32) -> Vec3 {
        let d =
            add(add(self.forward, scale(self.right, (x - self.center[0]) / self.focal)), scale(self.up, (self.center[1] - y) / self.focal));
        normalize(d)
    }
}

/// A vertex in camera space with what is interpolated across a triangle: texture and light
/// map coordinates, blend alpha and vertex light.
#[derive(Debug, Clone, Copy)]
struct Corner {
    p: Vec3,
    q: [f32; 8],
}

fn lerp_corner(a: &Corner, b: &Corner, t: f32) -> Corner {
    let l = |x: f32, y: f32| x + (y - x) * t;
    Corner { p: std::array::from_fn(|i| l(a.p[i], b.p[i])), q: std::array::from_fn(|i| l(a.q[i], b.q[i])) }
}

/// Cuts away the part of a triangle in front of the near plane.
fn clip_near(corners: &[Corner; 3]) -> Vec<Corner> {
    let mut out = Vec::with_capacity(4);
    for i in 0..3 {
        let (a, b) = (&corners[i], &corners[(i + 1) % 3]);
        let (a_in, b_in) = (a.p[2] >= NEAR, b.p[2] >= NEAR);
        if a_in {
            out.push(*a);
        }
        if a_in != b_in {
            out.push(lerp_corner(a, b, (NEAR - a.p[2]) / (b.p[2] - a.p[2])));
        }
    }
    out
}

/// A corner on the screen; everything after `w` is divided by depth, to be interpolated
/// linearly across the screen.
#[derive(Debug, Clone, Copy)]
struct Projected {
    x: f32,
    y: f32,
    /// 1 / depth.
    w: f32,
    q: [f32; 8],
}

/// What a frame is made of: colour in linear light, and 1 / depth (0 is nothing).
struct Frame {
    width: usize,
    height: usize,
    color: Vec<Vec3>,
    depth: Vec<f32>,
}

/// The light map atlas, linear.
struct Atlas<'a> {
    width: usize,
    height: usize,
    texels: &'a [[f32; 3]],
}

impl Atlas<'_> {
    fn sample(&self, uv: [f32; 2]) -> Vec3 {
        let x = (uv[0] * self.width as f32 - 0.5).clamp(0.0, (self.width - 1) as f32);
        let y = (uv[1] * self.height as f32 - 0.5).clamp(0.0, (self.height - 1) as f32);
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(self.width - 1), (y0 + 1).min(self.height - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let t = |xi: usize, yi: usize| self.texels[yi * self.width + xi];
        let (a, b, c, d) = (t(x0, y0), t(x1, y0), t(x0, y1), t(x1, y1));
        std::array::from_fn(|i| (a[i] * (1.0 - fx) + b[i] * fx) * (1.0 - fy) + (c[i] * (1.0 - fx) + d[i] * fx) * fy)
    }
}

struct Scene<'a> {
    piece: &'a Piece,
    /// The light map, for brushes; models are lit per vertex.
    atlas: Option<Atlas<'a>>,
    textures: &'a [Option<Texture>],
    linear: &'a [f32; 256],
}

fn scene<'a>(piece: &'a Piece, textures: &'a [Option<Texture>], linear: &'a [f32; 256]) -> Scene<'a> {
    let atlas = piece.lightmap.as_ref().map(|l| Atlas { width: l.width as usize, height: l.height as usize, texels: &l.texels });
    Scene { piece, atlas, textures, linear }
}

/// Level of detail: how many texels of `texture` one pixel covers, as a mipmap level.
fn lod(texture: &Texture, du: [f32; 2], dv: [f32; 2]) -> f32 {
    let (w, h) = texture.size();
    let x = (du[0] * w).hypot(dv[0] * h);
    let y = (du[1] * w).hypot(dv[1] * h);
    x.max(y).max(1e-6).log2()
}

/// Draws the triangles of a surface: opaque ones write depth, translucent ones blend.
fn draw(frame: &mut Frame, camera: &Camera, scene: &Scene, s: &Surface) {
    let piece = scene.piece;
    for tri in piece.indices[s.first..s.first + s.count].as_chunks::<3>().0 {
        let v = tri.map(|i| piece.vertices[i as usize]);
        if !s.no_cull {
            let normal = add(add(v[0].normal, v[1].normal), v[2].normal);
            if dot(normal, sub(camera.eye, v[0].position)) < 0.0 {
                continue;
            }
        }
        let corners = v.map(|v| {
            let [u, t] = v.uv;
            let [lu, lv] = v.lightmap_uv;
            let [r, g, b] = v.light;
            Corner { p: camera.to_camera(v.position), q: [u, t, lu, lv, v.alpha, r, g, b] }
        });
        let polygon = clip_near(&corners);
        if polygon.len() < 3 {
            continue;
        }
        let projected: Vec<Projected> = polygon
            .iter()
            .map(|c| {
                let w = 1.0 / c.p[2];
                Projected {
                    x: camera.center[0] + c.p[0] * w * camera.focal,
                    y: camera.center[1] - c.p[1] * w * camera.focal,
                    w,
                    q: c.q.map(|q| q * w),
                }
            })
            .collect();
        for i in 1..projected.len() - 1 {
            raster(frame, scene, s, [projected[0], projected[i], projected[i + 1]]);
        }
    }
}

fn raster(frame: &mut Frame, scene: &Scene, s: &Surface, corners: [Projected; 3]) {
    let [a, mut b, mut c] = corners;
    let mut area = (b.x - a.x) * (c.y - a.y) - (c.x - a.x) * (b.y - a.y);
    if area.abs() < 1e-8 {
        return;
    }
    if area < 0.0 {
        std::mem::swap(&mut b, &mut c);
        area = -area;
    }
    let min_x = a.x.min(b.x).min(c.x).floor().max(0.0) as usize;
    let max_x = (a.x.max(b.x).max(c.x).ceil() as i64).min(frame.width as i64 - 1);
    let min_y = a.y.min(b.y).min(c.y).floor().max(0.0) as usize;
    let max_y = (a.y.max(b.y).max(c.y).ceil() as i64).min(frame.height as i64 - 1);
    if max_x < 0 || max_y < 0 || min_x as i64 > max_x || min_y as i64 > max_y {
        return;
    }
    // how a value interpolated across the screen changes per pixel
    let gradient = |qa: f32, qb: f32, qc: f32| {
        let dx = ((qb - qa) * (c.y - a.y) - (qc - qa) * (b.y - a.y)) / area;
        let dy = ((qc - qa) * (b.x - a.x) - (qb - qa) * (c.x - a.x)) / area;
        (dx, dy)
    };
    let (dw_dx, dw_dy) = gradient(a.w, b.w, c.w);
    let dq: [(f32, f32); 8] = std::array::from_fn(|k| gradient(a.q[k], b.q[k], c.q[k]));
    let edge = |p: &Projected, q: &Projected, x: f32, y: f32| (q.x - p.x) * (y - p.y) - (q.y - p.y) * (x - p.x);
    let base = s.base.and_then(|i| scene.textures[i].as_ref());
    let base2 = s.base2.and_then(|i| scene.textures[i].as_ref());
    for y in min_y..=max_y as usize {
        let py = y as f32 + 0.5;
        for x in min_x..=max_x as usize {
            let px = x as f32 + 0.5;
            if edge(&a, &b, px, py) < 0.0 || edge(&b, &c, px, py) < 0.0 || edge(&c, &a, px, py) < 0.0 {
                continue;
            }
            let (ox, oy) = (px - a.x, py - a.y);
            let w = a.w + dw_dx * ox + dw_dy * oy;
            let at = y * frame.width + x;
            if w <= frame.depth[at] || w <= 0.0 {
                continue;
            }
            let z = 1.0 / w;
            let value = |k: usize| (a.q[k] + dq[k].0 * ox + dq[k].1 * oy) * z;
            let uv = [value(0), value(1)];
            // derivatives of the texture coordinates per pixel, for the mipmap level
            let du = [(dq[0].0 - uv[0] * dw_dx) * z, (dq[0].1 - uv[0] * dw_dy) * z];
            let dv = [(dq[1].0 - uv[1] * dw_dx) * z, (dq[1].1 - uv[1] * dw_dy) * z];
            let mut albedo = match base {
                Some(t) => t.sample(uv, lod(t, du, dv), scene.linear),
                None if s.water.is_some() => [1.0; 4],
                None => missing_texture(uv),
            };
            if let Some(t) = base2 {
                let blend = value(4).clamp(0.0, 1.0);
                let other = t.sample(uv, lod(t, du, dv), scene.linear);
                albedo = std::array::from_fn(|i| albedo[i] * (1.0 - blend) + other[i] * blend);
            }
            if let Some(reference) = s.alpha_test
                && albedo[3] < reference
            {
                continue;
            }
            let light = match &scene.atlas {
                _ if !s.lit => [1.0; 3],
                Some(atlas) => atlas.sample([value(2), value(3)]),
                None => [value(5), value(6), value(7)],
            };
            let mut color: Vec3 = std::array::from_fn(|i| albedo[i] * s.tint[i] * light[i]);
            let mut opacity = albedo[3] * s.alpha;
            if let Some(fog) = s.water {
                color = fog;
                opacity = 0.75;
            }
            let pixel = &mut frame.color[at];
            if !s.translucent {
                *pixel = color;
                frame.depth[at] = w;
            } else if s.additive {
                *pixel = std::array::from_fn(|i| pixel[i] + color[i] * opacity);
            } else {
                *pixel = std::array::from_fn(|i| pixel[i] * (1.0 - opacity) + color[i] * opacity);
            }
        }
    }
}

fn fill_sky(frame: &mut Frame, camera: &Camera, sky: &[Option<SkyFace>; 6], textures: &[Option<Texture>], linear: &[f32; 256]) {
    for y in 0..frame.height {
        for x in 0..frame.width {
            let at = y * frame.width + x;
            if frame.depth[at] > 0.0 {
                continue;
            }
            let direction = camera.direction(x as f32 + 0.5, y as f32 + 0.5);
            let (face, uv) = sky_face(direction);
            frame.color[at] = match sky[face].and_then(|f| textures[f.texture].as_ref().map(|t| (t, f.transform))) {
                Some((texture, transform)) => {
                    let uv = transform.map_or(uv, |t| t.apply(uv));
                    let c = texture.sample(uv, 0.0, linear);
                    [c[0], c[1], c[2]]
                }
                // a plain sky: lighter toward the top
                None => {
                    let h = direction[2].max(0.0);
                    [0.25 + 0.2 * h, 0.35 + 0.25 * h, 0.5 + 0.3 * h]
                }
            };
        }
    }
}

/// Draws a map. `vfs` gets the map's pakfile in front while the materials are read.
pub fn render(vfs: &mut Vfs, bsp: &Bsp, view: &View) -> Result<Picture, String> {
    let started = Instant::now();
    if view.width == 0 || view.height == 0 || view.width > 16384 || view.height > 16384 {
        return Err(format!("a picture of {}x{} pixels cannot be drawn", view.width, view.height));
    }
    let mounted = gmod_world::mount_pakfile(vfs, bsp).map_err(|e| format!("pakfile: {e}"))?;
    // a still picture: what may move stands where the map puts it
    let world = gmod_world::load_all(vfs, bsp);
    if mounted {
        vfs.remove(0);
    }
    Ok(draw_world(&world, bsp, view, started))
}

fn draw_world(world: &World, bsp: &Bsp, view: &View, started: Instant) -> Picture {
    let textures: Vec<Option<Texture>> = world.textures.iter().map(|t| Texture::from_vtf(&t.vtf)).collect();
    let (eye, target) = match (view.eye, view.target) {
        (Some(eye), Some(target)) => (eye, target),
        (Some(eye), None) => (eye, add(eye, [100.0, 0.0, 0.0])),
        _ => gmod_world::default_view(bsp),
    };
    let camera = Camera::new(eye, target, view);
    let linear = srgb_to_linear();
    let (width, height) = (view.width as usize, view.height as usize);
    let mut frame = Frame { width, height, color: vec![[0.0; 3]; width * height], depth: vec![0.0; width * height] };
    for piece in &world.pieces {
        let scene = scene(piece, &textures, &linear);
        for s in piece.surfaces.iter().filter(|s| !s.translucent) {
            draw(&mut frame, &camera, &scene, s);
        }
    }
    fill_sky(&mut frame, &camera, &world.sky, &textures, &linear);
    // translucent surfaces last, the farthest first
    let mut translucent: Vec<(f32, &Piece, &Surface)> = world
        .pieces
        .iter()
        .flat_map(|piece| piece.surfaces.iter().filter(|s| s.translucent).map(move |s| (piece, s)))
        .map(|(piece, s)| {
            let indices = &piece.indices[s.first..s.first + s.count];
            let sum = indices.iter().fold([0.0; 3], |acc, &i| add(acc, piece.vertices[i as usize].position));
            let middle = scale(sum, 1.0 / indices.len().max(1) as f32);
            let d = sub(middle, eye);
            (dot(d, d), piece, s)
        })
        .collect();
    translucent.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (_, piece, s) in translucent {
        draw(&mut frame, &camera, &scene(piece, &textures, &linear), s);
    }

    let rgb: Vec<u8> = frame.color.iter().flat_map(|c| c.map(linear_to_srgb)).collect();
    // textures that are there but cannot be shown count as missing too
    let mut missing_textures = world.missing_textures.clone();
    missing_textures.extend(world.textures.iter().zip(&textures).filter(|(_, t)| t.is_none()).map(|(t, _)| t.name.clone()));
    Picture {
        png: png::encode(view.width, view.height, 3, &rgb),
        eye,
        target,
        triangles: world.triangles(),
        missing_materials: world.missing_materials.clone(),
        missing_textures,
        missing_models: world.missing_models.clone(),
        seconds: started.elapsed().as_secs_f64(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_curves() {
        let linear = srgb_to_linear();
        for i in [0u8, 1, 50, 128, 200, 255] {
            assert_eq!(linear_to_srgb(linear[i as usize]), i);
        }
    }

    #[test]
    fn draws_the_test_map() {
        // without the game every material is missing: purple and black squares, lit
        let bsp = Bsp::parse("test.bsp".into(), std::sync::Arc::from(gmod_bsp::testmap::bytes())).unwrap();
        let view = View { width: 64, height: 48, fov: 90.0, eye: Some([64.0, -200.0, 200.0]), target: Some([64.0, 64.0, 0.0]) };
        let picture = render(&mut Vfs::new(), &bsp, &view).unwrap();
        assert!(picture.png.starts_with(b"\x89PNG"));
        assert_eq!(picture.triangles, 36);
        assert_eq!(picture.missing_materials.len(), 3);
        assert_eq!(picture.missing_models, ["models/props_c17/oildrum001.mdl"]);
    }
}
