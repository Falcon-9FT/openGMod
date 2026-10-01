//! `render`: a picture of a map drawn on the CPU - the world with its textures and light maps,
//! blended displacements, alpha testing, translucency, water and the sky box. A preview, and a
//! check that the readers fit together; the engine itself draws with the GPU.
//!
//! Brush entities (`func_brush`, doors ...) are drawn where they stand: their geometry is kept
//! relative to the entity's `origin`, turned by its `angles`. Not drawn yet: models (static
//! props, entities), decals and overlays, the 3D sky box, fog, reflections and bump maps.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use gmod_bsp::Bsp;
use gmod_bsp::mesh::{self, Mesh};
use gmod_fs::{Backing, Mount, Vfs};
use gmod_material::vtf::flags;
use gmod_material::{Library, Material, Vtf, png};

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

/// A texture coordinate transform (`$basetexturetransform`): `center`, `scale`, `rotate`,
/// `translate`.
#[derive(Debug, Clone, Copy)]
struct Transform {
    center: [f32; 2],
    scale: [f32; 2],
    rotate: f32,
    translate: [f32; 2],
}

impl Transform {
    fn parse(text: &str) -> Option<Transform> {
        let mut t = Transform { center: [0.5, 0.5], scale: [1.0, 1.0], rotate: 0.0, translate: [0.0, 0.0] };
        let words: Vec<&str> = text.split_whitespace().collect();
        let number = |i: usize| words.get(i).and_then(|w| w.parse::<f32>().ok());
        let mut i = 0;
        while i < words.len() {
            match words[i].to_ascii_lowercase().as_str() {
                "center" => t.center = [number(i + 1)?, number(i + 2)?],
                "scale" => t.scale = [number(i + 1)?, number(i + 2)?],
                "translate" => t.translate = [number(i + 1)?, number(i + 2)?],
                "rotate" => {
                    t.rotate = number(i + 1)?;
                    i += 2;
                    continue;
                }
                _ => return None,
            }
            i += 3;
        }
        Some(t)
    }

    fn apply(&self, uv: [f32; 2]) -> [f32; 2] {
        let (s, c) = self.rotate.to_radians().sin_cos();
        let p = [(uv[0] - self.center[0]) * self.scale[0], (uv[1] - self.center[1]) * self.scale[1]];
        let p = [p[0] * c - p[1] * s, p[0] * s + p[1] * c];
        [p[0] + self.center[0] + self.translate[0], p[1] + self.center[1] + self.translate[1]]
    }
}

/// How a batch of the world is drawn, from its material.
struct Surface {
    first: usize,
    count: usize,
    base: Option<usize>,
    /// The second texture of `WorldVertexTransition`, blended in by vertex alpha.
    base2: Option<usize>,
    alpha_test: Option<f32>,
    translucent: bool,
    additive: bool,
    lit: bool,
    no_cull: bool,
    tint: Vec3,
    alpha: f32,
    /// A water surface and its fog colour.
    water: Option<Vec3>,
}

/// Textures by name, decoded once.
struct Textures {
    list: Vec<Texture>,
    by_name: HashMap<String, Option<usize>>,
    missing: Vec<String>,
}

impl Textures {
    fn get(&mut self, vfs: &Vfs, library: &mut Library, name: &str) -> Option<usize> {
        if let Some(&known) = self.by_name.get(name) {
            return known;
        }
        let texture = library.texture(vfs, name).ok().and_then(|vtf| Texture::from_vtf(&vtf));
        let index = texture.map(|t| {
            self.list.push(t);
            self.list.len() - 1
        });
        if index.is_none() {
            self.missing.push(name.to_string());
        }
        self.by_name.insert(name.to_string(), index);
        index
    }
}

fn surface(material: Option<&Material>, first: usize, count: usize, vfs: &Vfs, library: &mut Library, textures: &mut Textures) -> Surface {
    let mut s = Surface {
        first,
        count,
        base: None,
        base2: None,
        alpha_test: None,
        translucent: false,
        additive: false,
        lit: true,
        no_cull: false,
        tint: [1.0; 3],
        alpha: 1.0,
        water: None,
    };
    let Some(m) = material else { return s };
    if let Some(name) = m.texture("$basetexture") {
        s.base = textures.get(vfs, library, &name);
    }
    if m.shader.starts_with("worldvertextransition")
        && let Some(name) = m.texture("$basetexture2")
    {
        s.base2 = textures.get(vfs, library, &name);
    }
    if m.flag("$alphatest") {
        s.alpha_test = Some(m.float("$alphatestreference").filter(|&r| r > 0.0).unwrap_or(0.5));
    }
    s.alpha = m.float("$alpha").unwrap_or(1.0).clamp(0.0, 1.0);
    s.additive = m.flag("$additive");
    s.translucent = m.flag("$translucent") || s.additive || s.alpha < 1.0;
    s.no_cull = m.flag("$nocull");
    s.tint = m.vector("$color").unwrap_or([1.0; 3]);
    s.lit = !m.shader.starts_with("unlit");
    if m.shader == "water" {
        s.water = Some(m.vector("$fogcolor").unwrap_or([0.1, 0.2, 0.25]));
        s.translucent = true;
        s.lit = false;
    }
    s
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

/// A vertex in camera space with what is interpolated across a triangle.
#[derive(Debug, Clone, Copy)]
struct Corner {
    p: Vec3,
    uv: [f32; 2],
    lm: [f32; 2],
    alpha: f32,
}

fn lerp_corner(a: &Corner, b: &Corner, t: f32) -> Corner {
    let l = |x: f32, y: f32| x + (y - x) * t;
    Corner {
        p: [l(a.p[0], b.p[0]), l(a.p[1], b.p[1]), l(a.p[2], b.p[2])],
        uv: [l(a.uv[0], b.uv[0]), l(a.uv[1], b.uv[1])],
        lm: [l(a.lm[0], b.lm[0]), l(a.lm[1], b.lm[1])],
        alpha: l(a.alpha, b.alpha),
    }
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
    q: [f32; 5],
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
    mesh: &'a Mesh,
    atlas: Atlas<'a>,
    textures: &'a [Texture],
    linear: &'a [f32; 256],
}

/// A brush model in the world: the world itself, or a brush entity moved to where it stands.
struct Piece {
    mesh: Mesh,
    surfaces: Vec<Surface>,
}

impl Piece {
    fn scene<'a>(&'a self, textures: &'a [Texture], linear: &'a [f32; 256]) -> Scene<'a> {
        let lightmap = &self.mesh.lightmap;
        Scene {
            mesh: &self.mesh,
            atlas: Atlas { width: lightmap.width as usize, height: lightmap.height as usize, texels: &lightmap.texels },
            textures,
            linear,
        }
    }
}

/// Brush entities that are never seen: volumes for the game's logic and effects.
const UNSEEN: [&str; 12] = [
    "trigger_",
    "func_areaportal",
    "func_occluder",
    "func_viscluster",
    "func_dustmotes",
    "func_dustcloud",
    "func_smokevolume",
    "func_precipitation",
    "func_respawnroom",
    "func_regenerate",
    "func_nobuild",
    "func_clip_vphysics",
];

/// The brush models to draw: the world (model 0) and every brush entity that can be seen, with
/// its origin and angles.
fn placed_models(bsp: &Bsp) -> Vec<(usize, Vec3, Vec3)> {
    let mut models = vec![(0, [0.0; 3], [0.0; 3])];
    for entity in &bsp.entities {
        let Some(model) = entity.get("model").and_then(|m| m.strip_prefix('*')).and_then(|m| m.parse::<usize>().ok()) else { continue };
        let class = entity.class_name();
        let hidden = entity.get("rendermode") == Some("10") || entity.get("StartDisabled") == Some("1");
        if model == 0 || model >= bsp.models.len() || hidden || UNSEEN.iter().any(|unseen| class.starts_with(unseen)) {
            continue;
        }
        models.push((model, entity.vec3("origin").unwrap_or([0.0; 3]), entity.vec3("angles").unwrap_or([0.0; 3])));
    }
    models
}

/// Moves a brush entity's geometry from its own space into the world: turned by pitch, yaw
/// and roll (degrees), then moved to `origin`.
fn place(mesh: &mut Mesh, origin: Vec3, angles: Vec3) {
    let [pitch, yaw, roll] = angles.map(f32::to_radians);
    let ((sp, cp), (sy, cy), (sr, cr)) = (pitch.sin_cos(), yaw.sin_cos(), roll.sin_cos());
    let forward = [cp * cy, cp * sy, -sp];
    let left = [sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, sr * cp];
    let up = [cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp];
    let turn = |v: Vec3| add(add(scale(forward, v[0]), scale(left, v[1])), scale(up, v[2]));
    for vertex in &mut mesh.vertices {
        vertex.position = add(origin, turn(vertex.position));
        vertex.normal = turn(vertex.normal);
    }
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
    let mesh = scene.mesh;
    for tri in mesh.indices[s.first..s.first + s.count].chunks_exact(3) {
        let v = [mesh.vertices[tri[0] as usize], mesh.vertices[tri[1] as usize], mesh.vertices[tri[2] as usize]];
        if !s.no_cull {
            let normal = add(add(v[0].normal, v[1].normal), v[2].normal);
            if dot(normal, sub(camera.eye, v[0].position)) < 0.0 {
                continue;
            }
        }
        let corners = v.map(|v| Corner { p: camera.to_camera(v.position), uv: v.uv, lm: v.lightmap_uv, alpha: v.alpha });
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
                    q: [c.uv[0] * w, c.uv[1] * w, c.lm[0] * w, c.lm[1] * w, c.alpha * w],
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
    let dq: [(f32, f32); 5] = std::array::from_fn(|k| gradient(a.q[k], b.q[k], c.q[k]));
    let edge = |p: &Projected, q: &Projected, x: f32, y: f32| (q.x - p.x) * (y - p.y) - (q.y - p.y) * (x - p.x);
    let base = s.base.map(|i| &scene.textures[i]);
    let base2 = s.base2.map(|i| &scene.textures[i]);
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
            let light = if s.lit { scene.atlas.sample([value(2), value(3)]) } else { [1.0; 3] };
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

/// The six faces of the sky box, as the sky texture names end: the axes +X, -X, +Y, -Y, +Z, -Z.
const SKY_FACES: [&str; 6] = ["rt", "lf", "bk", "ft", "up", "dn"];

/// Face and coordinates on it of the sky box seen in direction `d`.
fn sky_face(d: Vec3) -> (usize, [f32; 2]) {
    let (ax, ay, az) = (d[0].abs(), d[1].abs(), d[2].abs());
    let (face, s, t) = if ax >= ay && ax >= az {
        if d[0] > 0.0 { (0, -d[1] / ax, d[2] / ax) } else { (1, d[1] / ax, d[2] / ax) }
    } else if ay >= az {
        if d[1] > 0.0 { (2, d[0] / ay, d[2] / ay) } else { (3, -d[0] / ay, d[2] / ay) }
    } else if d[2] > 0.0 {
        (4, -d[1] / az, -d[0] / az)
    } else {
        (5, -d[1] / az, d[0] / az)
    };
    (face, [(s + 1.0) / 2.0, (1.0 - t) / 2.0])
}

/// A sky box face: its texture and the texture transform of its material.
type SkyFace = Option<(usize, Option<Transform>)>;

fn fill_sky(frame: &mut Frame, camera: &Camera, sky: &[SkyFace; 6], textures: &[Texture], linear: &[f32; 256]) {
    for y in 0..frame.height {
        for x in 0..frame.width {
            let at = y * frame.width + x;
            if frame.depth[at] > 0.0 {
                continue;
            }
            let direction = camera.direction(x as f32 + 0.5, y as f32 + 0.5);
            let (face, uv) = sky_face(direction);
            frame.color[at] = match sky[face] {
                Some((texture, transform)) => {
                    let uv = transform.map_or(uv, |t| t.apply(uv));
                    let c = textures[texture].sample(uv, 0.0, linear);
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

/// Where to look from when no view is given: a player start or an observer point, else above
/// the middle of the world.
fn default_view(bsp: &Bsp) -> (Vec3, Vec3) {
    const STARTS: [&str; 8] = [
        "info_player_start",
        "info_observer_point",
        "info_player_teamspawn",
        "info_player_deathmatch",
        "info_player_counterterrorist",
        "info_player_terrorist",
        "info_player_combine",
        "info_player_rebel",
    ];
    for class in STARTS {
        if let Some(entity) = bsp.entities.iter().find(|e| e.class_name() == class)
            && let Some(origin) = entity.vec3("origin")
        {
            let [pitch, yaw, _] = entity.vec3("angles").unwrap_or([0.0; 3]);
            let (pitch, yaw) = (pitch.to_radians(), yaw.to_radians());
            let forward = [pitch.cos() * yaw.cos(), pitch.cos() * yaw.sin(), -pitch.sin()];
            let height = if class == "info_observer_point" { 0.0 } else { 64.0 };
            let eye = add(origin, [0.0, 0.0, height]);
            return (eye, add(eye, scale(forward, 100.0)));
        }
    }
    let Some(world) = bsp.models.first() else { return ([0.0; 3], [100.0, 0.0, 0.0]) };
    let middle = scale(add(world.mins, world.maxs), 0.5);
    let size = sub(world.maxs, world.mins);
    let eye = add(middle, [-size[0] * 0.4, -size[1] * 0.4, size[2] * 0.6]);
    (eye, middle)
}

/// Draws a map. `vfs` gets the map's pakfile in front while the materials are read.
pub fn render(vfs: &mut Vfs, bsp: &Bsp, view: &View) -> Result<Picture, String> {
    let started = Instant::now();
    let pak = bsp.pakfile().map_err(|e| format!("pakfile: {e}"))?;
    let mounted = pak.is_some();
    if let Some(pak) = pak {
        vfs.insert(0, Mount::new(&["GAME", "BSP"], Backing::Zip(pak)));
    }
    let result = draw_map(vfs, bsp, view, started);
    if mounted {
        vfs.remove(0);
    }
    result
}

fn draw_map(vfs: &Vfs, bsp: &Bsp, view: &View, started: Instant) -> Result<Picture, String> {
    if view.width == 0 || view.height == 0 || view.width > 16384 || view.height > 16384 {
        return Err(format!("a picture of {}x{} pixels cannot be drawn", view.width, view.height));
    }
    let mut library = Library::new();
    let mut textures = Textures { list: Vec::new(), by_name: HashMap::new(), missing: Vec::new() };
    let mut missing_materials = Vec::new();
    let mut materials: HashMap<usize, Option<Arc<Material>>> = HashMap::new();
    let mut pieces = Vec::new();
    for (model, origin, angles) in placed_models(bsp) {
        let mut mesh = mesh::build(bsp, mesh::Options { model, ..mesh::Options::default() });
        if mesh.indices.is_empty() {
            continue;
        }
        if model != 0 {
            place(&mut mesh, origin, angles);
        }
        let mut surfaces = Vec::new();
        for batch in &mesh.batches {
            let material = materials
                .entry(batch.material)
                .or_insert_with(|| {
                    let name = bsp.materials.get(batch.material).map_or("", String::as_str);
                    library.material(vfs, name).inspect_err(|_| missing_materials.push(name.to_string())).ok()
                })
                .clone();
            let (first, count) = (batch.first_index as usize, batch.index_count as usize);
            surfaces.push(surface(material.as_deref(), first, count, vfs, &mut library, &mut textures));
        }
        pieces.push(Piece { mesh, surfaces });
    }
    // the sky box named by the world
    let sky_name = bsp.worldspawn().and_then(|w| w.get("skyname")).unwrap_or("").to_string();
    let sky: [SkyFace; 6] = std::array::from_fn(|i| {
        if sky_name.is_empty() {
            return None;
        }
        let material = library.material(vfs, &format!("skybox/{sky_name}{}", SKY_FACES[i])).ok()?;
        let texture = textures.get(vfs, &mut library, &material.texture("$basetexture")?)?;
        Some((texture, material.get("$basetexturetransform").and_then(Transform::parse)))
    });

    let (eye, target) = match (view.eye, view.target) {
        (Some(eye), Some(target)) => (eye, target),
        (Some(eye), None) => (eye, add(eye, [100.0, 0.0, 0.0])),
        _ => default_view(bsp),
    };
    let camera = Camera::new(eye, target, view);
    let linear = srgb_to_linear();
    let (width, height) = (view.width as usize, view.height as usize);
    let mut frame = Frame { width, height, color: vec![[0.0; 3]; width * height], depth: vec![0.0; width * height] };
    for piece in &pieces {
        let scene = piece.scene(&textures.list, &linear);
        for s in piece.surfaces.iter().filter(|s| !s.translucent) {
            draw(&mut frame, &camera, &scene, s);
        }
    }
    fill_sky(&mut frame, &camera, &sky, &textures.list, &linear);
    // translucent surfaces last, the farthest first
    let mut translucent: Vec<(f32, &Piece, &Surface)> = pieces
        .iter()
        .flat_map(|piece| piece.surfaces.iter().filter(|s| s.translucent).map(move |s| (piece, s)))
        .map(|(piece, s)| {
            let mesh = &piece.mesh;
            let indices = &mesh.indices[s.first..s.first + s.count];
            let sum = indices.iter().fold([0.0; 3], |acc, &i| add(acc, mesh.vertices[i as usize].position));
            let middle = scale(sum, 1.0 / indices.len().max(1) as f32);
            let d = sub(middle, eye);
            (dot(d, d), piece, s)
        })
        .collect();
    translucent.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (_, piece, s) in translucent {
        draw(&mut frame, &camera, &piece.scene(&textures.list, &linear), s);
    }

    let rgb: Vec<u8> = frame.color.iter().flat_map(|c| c.map(linear_to_srgb)).collect();
    Ok(Picture {
        png: png::encode(view.width, view.height, 3, &rgb),
        eye,
        target,
        triangles: pieces.iter().map(|p| p.mesh.indices.len() / 3).sum(),
        missing_materials,
        missing_textures: textures.missing,
        seconds: started.elapsed().as_secs_f64(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sky_box_faces() {
        // straight ahead along each axis lands in the middle of its face
        let axes = [[1.0, 0.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0, -1.0]];
        for (i, axis) in axes.into_iter().enumerate() {
            assert_eq!(sky_face(axis), (i, [0.5, 0.5]));
        }
        // up is the top of a side face
        let (face, uv) = sky_face(normalize([1.0, 0.0, 0.99]));
        assert_eq!(face, 0);
        assert!(uv[1] < 0.01);
    }

    #[test]
    fn texture_transforms() {
        let t = Transform::parse("center 0 0 scale 1 2 rotate 0 translate 0 0").unwrap();
        assert_eq!(t.apply([0.5, 0.25]), [0.5, 0.5]);
        let t = Transform::parse("center .5 .5 scale 1 1 rotate 90 translate 0 0").unwrap();
        let p = t.apply([1.0, 0.5]);
        assert!((p[0] - 0.5).abs() < 1e-6 && (p[1] - 1.0).abs() < 1e-6, "{p:?}");
        assert!(Transform::parse("bogus").is_none());
    }

    #[test]
    fn brush_entities_are_placed() {
        let mut mesh = Mesh::default();
        mesh.vertices.push(mesh::Vertex { position: [10.0, 0.0, 0.0], normal: [1.0, 0.0, 0.0], ..mesh::Vertex::default() });
        // yaw 90 turns +x into +y; then the origin moves it
        place(&mut mesh, [100.0, 200.0, 300.0], [0.0, 90.0, 0.0]);
        let v = mesh.vertices[0];
        let near = |a: Vec3, b: Vec3| sub(a, b).iter().all(|d| d.abs() < 1e-4);
        assert!(near(v.position, [100.0, 210.0, 300.0]), "{:?}", v.position);
        assert!(near(v.normal, [0.0, 1.0, 0.0]), "{:?}", v.normal);
        // pitch 90 looks down: +x turns into -z
        mesh.vertices[0].normal = [1.0, 0.0, 0.0];
        place(&mut mesh, [0.0; 3], [90.0, 0.0, 0.0]);
        assert!(near(mesh.vertices[0].normal, [0.0, 0.0, -1.0]), "{:?}", mesh.vertices[0].normal);
    }

    #[test]
    fn colour_curves() {
        let linear = srgb_to_linear();
        for i in [0u8, 1, 50, 128, 200, 255] {
            assert_eq!(linear_to_srgb(linear[i as usize]), i);
        }
    }
}
