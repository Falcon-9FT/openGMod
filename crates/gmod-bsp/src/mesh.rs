//! A brush model as triangles: its faces and displacements with texture and lightmap
//! coordinates, grouped by material, and the faces' lightmaps packed into one atlas.
//!
//! * A face is a convex polygon: a fan of triangles. Texture coordinates come from its
//!   `texinfo` (`dot(p, s.xyz) + s.w` over the texture size), lightmap coordinates likewise
//!   in luxels, less the face's lightmap minimum, plus where its lightmap went in the atlas.
//! * A displacement is a grid over its four-sided base face, starting at the corner nearest
//!   its start position: rows run from corner 0 to corner 1, columns from that edge across to
//!   the edge from corner 3 to corner 2. Each vertex is raised along its direction by its
//!   distance; texture and lightmap coordinates are those of the flat base, interpolated.
//! * A face's light is that of its light styles added up - style 0, and those of the lights
//!   that are switched on as the map starts ([`dark_styles`] are not); with bump mapping a
//!   style has four lightmaps, the first unbumped, the others of the light along the three
//!   directions of [`BUMP_BASIS`] (kept in [`Lightmap::bumped`]). Faces without one are lit by
//!   a white luxel.
//! * Faces flagged nodraw, sky, trigger, hint or skip are left out.
//! * The overlays on the faces built come after them, a batch each from the lowest render
//!   order up, lifted a little off their faces and lit by their light maps
//!   ([`crate::overlay`]). Only the world has them: brush entities' faces are in their own
//!   space.

use std::collections::HashMap;

use crate::lighting::rgbe_to_linear;
use crate::math::{Vec3, add, cross, dot, lerp3, normalize, scale, sub};
use crate::{Bsp, Face, Overlay, TexInfo, overlay, surf};

/// A vertex of the world.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    /// Texture coordinates, in repeats of the base texture.
    pub uv: [f32; 2],
    /// Coordinates in the lightmap atlas, 0 to 1.
    pub lightmap_uv: [f32; 2],
    /// The blend between the two textures of a displacement material (0-1).
    pub alpha: f32,
}

/// The triangles of one material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    /// Index of the material (`texdata`) in [`Bsp::materials`].
    pub material: usize,
    pub first_index: u32,
    pub index_count: u32,
    /// An overlay's, drawn over the faces.
    pub overlay: bool,
}

/// The lightmaps of all faces, in linear light.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Lightmap {
    pub width: u32,
    pub height: u32,
    pub texels: Vec<[f32; 3]>,
    /// When some face is bump mapped: three more lightmaps as large, one after another, of the
    /// light coming along the three directions of [`BUMP_BASIS`]; faces that are not have their
    /// one light in all three. Empty otherwise.
    pub bumped: Vec<[f32; 3]>,
}

/// The directions, in the space of a face's texture (x along it, z out of the face), of the
/// light in its bumped lightmaps, as vrad bakes them.
pub const BUMP_BASIS: [[f32; 3]; 3] = [
    [0.816_496_6, 0.0, 0.577_350_3],
    [-0.408_248_3, std::f32::consts::FRAC_1_SQRT_2, 0.577_350_3],
    [-0.408_248_3, -std::f32::consts::FRAC_1_SQRT_2, 0.577_350_3],
];

/// What [`build`] made.
#[derive(Debug, Clone, Default)]
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub batches: Vec<Batch>,
    pub lightmap: Lightmap,
    /// Sky faces, which are drawn as the sky instead.
    pub sky_faces: usize,
    /// Faces left out because they could not be built (a displacement on a face that is not
    /// four-sided, a lightmap outside the lighting lump).
    pub broken_faces: usize,
}

/// What to build.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// The brush model: 0 is the world.
    pub model: usize,
    /// Use the HDR lightmaps when the map has them.
    pub hdr: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options { model: 0, hdr: true }
    }
}

const SKIPPED: i32 = surf::NODRAW | surf::SKY | surf::SKY2D | surf::TRIGGER | surf::HINT | surf::SKIP;
/// Larger lightmaps than this are not real: a face is at most a few hundred luxels across.
const MAX_LIGHTMAP_SIDE: i32 = 1024;
/// Width of the lightmap atlas.
const ATLAS_WIDTH: u32 = 2048;

fn lerp2(a: [f32; 2], b: [f32; 2], t: f32) -> [f32; 2] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

/// Where a face's lightmap went in the atlas, in luxels.
#[derive(Debug, Clone, Copy)]
struct Placement {
    x: u32,
    y: u32,
    face: Face,
}

/// Texture coordinates of a point of a face.
fn texture_uv(info: &TexInfo, size: [f32; 2], p: [f32; 3]) -> [f32; 2] {
    let [s, t] = info.texture_vecs;
    [(dot(p, [s[0], s[1], s[2]]) + s[3]) / size[0], (dot(p, [t[0], t[1], t[2]]) + t[3]) / size[1]]
}

/// Atlas coordinates (in luxels, at luxel centres) of a point of a face.
fn luxel(info: &TexInfo, placement: Option<Placement>, p: [f32; 3]) -> [f32; 2] {
    match placement {
        Some(Placement { x, y, face }) => {
            let [s, t] = info.lightmap_vecs;
            let ls = dot(p, [s[0], s[1], s[2]]) + s[3] - face.lightmap_mins[0] as f32;
            let lt = dot(p, [t[0], t[1], t[2]]) + t[3] - face.lightmap_mins[1] as f32;
            // stay inside the face's own luxels so that filtering does not reach a neighbour
            let ls = ls.clamp(0.0, face.lightmap_size[0] as f32);
            let lt = lt.clamp(0.0, face.lightmap_size[1] as f32);
            [x as f32 + ls + 0.5, y as f32 + lt + 0.5]
        }
        // the white luxel at the atlas origin
        None => [0.5, 0.5],
    }
}

/// Places rectangles of luxels on shelves of a fixed-width atlas; the first is a single white
/// luxel for faces without a lightmap.
fn pack(sizes: &[(u32, u32)]) -> (u32, u32, Vec<(u32, u32)>) {
    let width = sizes.iter().map(|s| s.0).max().unwrap_or(1).max(ATLAS_WIDTH);
    let mut order: Vec<usize> = (0..sizes.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(sizes[i].1));
    let mut positions = vec![(0, 0); sizes.len()];
    let (mut x, mut y, mut shelf) = (1u32, 0u32, 1u32);
    for i in order {
        let (w, h) = sizes[i];
        if x + w > width {
            y += shelf;
            x = 0;
            shelf = 0;
        }
        positions[i] = (x, y);
        x += w;
        shelf = shelf.max(h);
    }
    (width, y + shelf, positions)
}

/// How much brighter an HDR map's light shows than its lightmaps store it.
pub const HDR_LIGHT_SCALE: f32 = 2.0;

/// Builds the triangles of a brush model.
pub fn build(bsp: &Bsp, options: Options) -> Mesh {
    build_faces(bsp, options, |_| true)
}

/// The same, of the faces `keep` says yes to (by their index in the map's faces): the world
/// split in two, the 3D sky box and the rest.
pub fn build_faces(bsp: &Bsp, options: Options, keep: impl Fn(usize) -> bool) -> Mesh {
    let hdr = options.hdr && !bsp.faces_hdr.is_empty() && !bsp.lighting_hdr.is_empty();
    // a map compiled for HDR only has no LDR lighting
    let hdr = hdr || (bsp.lighting.is_empty() && !bsp.faces_hdr.is_empty());
    let (faces, lighting) = if hdr { (&bsp.faces_hdr, &bsp.lighting_hdr) } else { (&bsp.faces, &bsp.lighting) };
    let mut mesh = Mesh::default();
    let Some(model) = bsp.models.get(options.model) else { return mesh };

    // faces to draw, by material
    let mut drawn: Vec<(usize, usize)> = Vec::new();
    for index in model.first_face as usize..(model.first_face + model.num_faces) as usize {
        let Some(face) = faces.get(index).filter(|_| keep(index)) else { continue };
        let Some(info) = usize::try_from(face.texinfo).ok().and_then(|i| bsp.texinfo.get(i)) else { continue };
        if info.flags & SKIPPED != 0 {
            if info.flags & surf::SKY != 0 {
                mesh.sky_faces += 1;
            }
            continue;
        }
        if let Ok(material) = usize::try_from(info.texdata) {
            drawn.push((material, index));
        }
    }
    drawn.sort();

    // lightmaps
    let lit = |face: &Face, info: &TexInfo| {
        let [w, h] = face.lightmap_size;
        let samples = (w as usize + 1) * (h as usize + 1);
        face.light_offset >= 0
            && info.flags & surf::NOLIGHT == 0
            && (0..MAX_LIGHTMAP_SIDE).contains(&w)
            && (0..MAX_LIGHTMAP_SIDE).contains(&h)
            && face.light_offset as usize + samples * 4 <= lighting.len()
    };
    let mut sizes = vec![(1u32, 1u32)];
    let mut slot = vec![None; drawn.len()];
    for (k, &(_, index)) in drawn.iter().enumerate() {
        let face = &faces[index];
        if lit(face, &bsp.texinfo[face.texinfo as usize]) {
            slot[k] = Some(sizes.len());
            sizes.push((face.lightmap_size[0] as u32 + 1, face.lightmap_size[1] as u32 + 1));
        }
    }
    let (width, height, positions) = pack(&sizes);
    let mut texels = vec![[0.0f32; 3]; width as usize * height as usize];
    texels[0] = [1.0, 1.0, 1.0];
    let bumps = |k: usize| {
        let face = &faces[drawn[k].1];
        slot[k].is_some() && bsp.texinfo[face.texinfo as usize].flags & surf::BUMPLIGHT != 0
    };
    let any_bumped = (0..drawn.len()).any(bumps);
    let area = texels.len();
    let mut bumped = if any_bumped { vec![[0.0f32; 3]; area * 3] } else { Vec::new() };
    let dark = dark_styles(bsp);
    // an HDR map's light shows twice as bright as it is stored, an LDR map's as stored (as
    // measured in GMod: gm_construct, gm_polye_v1)
    let scale = if hdr { HDR_LIGHT_SCALE } else { 1.0 };
    for (k, &(_, index)) in drawn.iter().enumerate() {
        let Some(slot) = slot[k] else { continue };
        let face = &faces[index];
        let (x, y) = positions[slot];
        let (w, h) = sizes[slot];
        let page = w as usize * h as usize * 4;
        let pages = if bumps(k) { 4 } else { 1 };
        for (n, &style) in face.styles.iter().enumerate() {
            if style == 255 {
                break;
            }
            let first = face.light_offset as usize + n * pages * page;
            if dark.contains(&style) || first + pages * page > lighting.len() {
                continue;
            }
            // the flat light, then each bumped one
            for p in 0..pages {
                for t in 0..h as usize {
                    for s in 0..w as usize {
                        let at = first + p * page + (t * w as usize + s) * 4;
                        let sample: [u8; 4] = lighting[at..at + 4].try_into().unwrap();
                        let [r, g, b] = rgbe_to_linear(sample).map(|c| c * scale);
                        let index = (y as usize + t) * width as usize + x as usize + s;
                        let texel = if p == 0 { &mut texels[index] } else { &mut bumped[(p - 1) * area + index] };
                        *texel = [texel[0] + r, texel[1] + g, texel[2] + b];
                    }
                }
            }
        }
    }
    // what is not bump mapped has its one light from every direction
    if any_bumped {
        for k in 0..drawn.len() {
            let Some(slot) = slot[k].filter(|_| !bumps(k)) else { continue };
            let ((x, y), (w, h)) = (positions[slot], sizes[slot]);
            for t in 0..h as usize {
                for s in 0..w as usize {
                    let at = (y as usize + t) * width as usize + x as usize + s;
                    for p in 0..3 {
                        bumped[p * area + at] = texels[at];
                    }
                }
            }
        }
        for p in 0..3 {
            bumped[p * area] = texels[0];
        }
    }

    // triangles
    let mut current: Option<Batch> = None;
    for (k, &(material, index)) in drawn.iter().enumerate() {
        let face = faces[index];
        let info = bsp.texinfo[face.texinfo as usize];
        let data = &bsp.texdata[material];
        let size = [data.width.max(1) as f32, data.height.max(1) as f32];
        let placement = slot[k].map(|slot| Placement { x: positions[slot].0, y: positions[slot].1, face });
        let first_index = mesh.indices.len() as u32;
        let built = if face.dispinfo >= 0 {
            displacement(bsp, &face, &info, size, placement, &mut mesh)
        } else {
            polygon(bsp, &face, &info, size, placement, &mut mesh);
            true
        };
        if !built {
            mesh.broken_faces += 1;
            continue;
        }
        let count = mesh.indices.len() as u32 - first_index;
        match &mut current {
            Some(batch) if batch.material == material => batch.index_count += count,
            _ => {
                mesh.batches.extend(current.take());
                current = Some(Batch { material, first_index, index_count: count, overlay: false });
            }
        }
    }
    mesh.batches.extend(current);
    // overlays lie on the world in its coordinates; a brush entity's faces are in its own,
    // and the entity moves them about, so none are put on them
    if options.model == 0 && !bsp.overlays.is_empty() {
        let placed = drawn.iter().enumerate().map(|(k, &(_, index))| {
            (index, slot[k].map(|slot| Placement { x: positions[slot].0, y: positions[slot].1, face: faces[index] }))
        });
        overlays(bsp, faces, &placed.collect(), &mut mesh);
    }

    for vertex in &mut mesh.vertices {
        vertex.lightmap_uv = [vertex.lightmap_uv[0] / width as f32, vertex.lightmap_uv[1] / height as f32];
    }
    mesh.lightmap = Lightmap { width, height, texels, bumped };
    mesh
}

/// The overlays on the faces built - `placed` has each one's index and where its light map
/// went - from the lowest render order up, a batch each.
fn overlays(bsp: &Bsp, faces: &[Face], placed: &HashMap<usize, Option<Placement>>, mesh: &mut Mesh) {
    let mut order: Vec<&Overlay> = bsp.overlays.iter().collect();
    order.sort_by_key(|o| o.render_order);
    for overlay in order {
        let Some(info) = usize::try_from(overlay.texinfo).ok().and_then(|i| bsp.texinfo.get(i)) else { continue };
        let Ok(material) = usize::try_from(info.texdata) else { continue };
        let first_index = mesh.indices.len() as u32;
        for &index in &overlay.faces {
            let Some(index) = usize::try_from(index).ok() else { continue };
            let (Some(face), Some(&placement)) = (faces.get(index), placed.get(&index)) else { continue };
            if face.dispinfo >= 0 {
                continue;
            }
            // a face built has its texinfo and plane
            let face_info = bsp.texinfo[face.texinfo as usize];
            let normal = bsp.planes[face.plane as usize].normal;
            let polygon = overlay::on_face(bsp, overlay, face);
            let base = mesh.vertices.len() as u32;
            for &(p, uv) in &polygon {
                let position = add(p, scale(normal, overlay::LIFT));
                mesh.vertices.push(Vertex { position, normal, uv, lightmap_uv: luxel(&face_info, placement, p), alpha: 0.0 });
            }
            for i in 1..(polygon.len() as u32).saturating_sub(1) {
                mesh.indices.extend([base, base + i, base + i + 1]);
            }
        }
        let index_count = mesh.indices.len() as u32 - first_index;
        if index_count > 0 {
            mesh.batches.push(Batch { material, first_index, index_count, overlay: true });
        }
    }
}

fn polygon(bsp: &Bsp, face: &Face, info: &TexInfo, size: [f32; 2], placement: Option<Placement>, mesh: &mut Mesh) {
    let normal = bsp.planes[face.plane as usize].normal;
    let base = mesh.vertices.len() as u32;
    let mut count = 0;
    for p in bsp.face_vertices(face) {
        mesh.vertices.push(Vertex {
            position: p,
            normal,
            uv: texture_uv(info, size, p),
            lightmap_uv: luxel(info, placement, p),
            alpha: 0.0,
        });
        count += 1;
    }
    for i in 1..count.max(2) - 1 {
        mesh.indices.extend([base, base + i, base + i + 1]);
    }
}

/// The corners of a displacement's four-sided base face, from the one nearest its start
/// position on; `None` when the face has another number of sides.
pub(crate) fn displacement_corners(bsp: &Bsp, face: &Face) -> Option<[Vec3; 4]> {
    let disp = &bsp.displacements[face.dispinfo as usize];
    let corners: Vec<Vec3> = bsp.face_vertices(face).collect();
    if corners.len() != 4 {
        return None;
    }
    let distance = |p: Vec3| {
        let d = sub(p, disp.start_position);
        dot(d, d)
    };
    let start = (0..4).min_by(|&a, &b| distance(corners[a]).total_cmp(&distance(corners[b]))).unwrap_or(0);
    Some(std::array::from_fn(|i| corners[(start + i) % 4]))
}

/// The points of a displacement, row by row (`side` × `side` of them), each raised from the
/// base face; `None` when the base face is not four-sided.
pub(crate) fn displacement_points(bsp: &Bsp, face: &Face) -> Option<Vec<Vec3>> {
    let c = displacement_corners(bsp, face)?;
    let disp = &bsp.displacements[face.dispinfo as usize];
    let side = disp.side();
    let verts = &bsp.disp_verts[disp.first_vert as usize..disp.first_vert as usize + side * side];
    let step = 1.0 / (side - 1) as f32;
    let mut points = Vec::with_capacity(side * side);
    for y in 0..side {
        let ty = y as f32 * step;
        let (a, b) = (lerp3(c[0], c[1], ty), lerp3(c[3], c[2], ty));
        for x in 0..side {
            let v = verts[y * side + x];
            points.push(add(lerp3(a, b, x as f32 * step), scale(v.direction, v.distance)));
        }
    }
    Some(points)
}

/// The triangles of a displacement of `side` × `side` points, as indices of its points: two a
/// cell, with the diagonal alternating as the engine lays it; clockwise seen from the front.
pub(crate) fn displacement_triangles(side: usize) -> impl Iterator<Item = [usize; 3]> {
    (0..side - 1).flat_map(move |y| {
        (0..side - 1).flat_map(move |x| {
            let i0 = y * side + x;
            let (i1, i2, i3) = (i0 + 1, i0 + side, i0 + side + 1);
            if (x + y) % 2 == 0 { [[i0, i2, i1], [i1, i2, i3]] } else { [[i0, i2, i3], [i0, i3, i1]] }
        })
    })
}

/// Builds a displacement; false if its base face is not four-sided.
fn displacement(bsp: &Bsp, face: &Face, info: &TexInfo, size: [f32; 2], placement: Option<Placement>, mesh: &mut Mesh) -> bool {
    let (Some(c), Some(points)) = (displacement_corners(bsp, face), displacement_points(bsp, face)) else { return false };
    let disp = &bsp.displacements[face.dispinfo as usize];
    let uv: [[f32; 2]; 4] = c.map(|p| texture_uv(info, size, p));
    let lm: [[f32; 2]; 4] = c.map(|p| luxel(info, placement, p));

    let side = disp.side();
    let verts = &bsp.disp_verts[disp.first_vert as usize..disp.first_vert as usize + side * side];
    let base = mesh.vertices.len();
    let step = 1.0 / (side - 1) as f32;
    for y in 0..side {
        let ty = y as f32 * step;
        let (uv_a, uv_b) = (lerp2(uv[0], uv[1], ty), lerp2(uv[3], uv[2], ty));
        let (lm_a, lm_b) = (lerp2(lm[0], lm[1], ty), lerp2(lm[3], lm[2], ty));
        for x in 0..side {
            let tx = x as f32 * step;
            mesh.vertices.push(Vertex {
                position: points[y * side + x],
                normal: [0.0; 3],
                uv: lerp2(uv_a, uv_b, tx),
                lightmap_uv: lerp2(lm_a, lm_b, tx),
                alpha: (verts[y * side + x].alpha / 255.0).clamp(0.0, 1.0),
            });
        }
    }
    let first_index = mesh.indices.len();
    for tri in displacement_triangles(side) {
        mesh.indices.extend(tri.map(|i| (base + i) as u32));
    }
    // smooth normals from the triangles, turned to face the way the base face does
    let facing = bsp.planes[face.plane as usize].normal;
    let mut normals = vec![[0.0f32; 3]; side * side];
    for tri in mesh.indices[first_index..].chunks(3) {
        let [a, b, c3] = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
        let n =
            cross(sub(mesh.vertices[b].position, mesh.vertices[a].position), sub(mesh.vertices[c3].position, mesh.vertices[a].position));
        for i in [a, b, c3] {
            let slot = &mut normals[i - base];
            *slot = add(*slot, n);
        }
    }
    let flip = normals.iter().fold([0.0f32; 3], |s, &n| add(s, n));
    let sign = if dot(flip, facing) < 0.0 { -1.0 } else { 1.0 };
    for (i, n) in normals.into_iter().enumerate() {
        mesh.vertices[base + i].normal = normalize(scale(n, sign));
    }
    true
}

/// The light styles of the lights that start switched off: vrad gives each named light a
/// style of its own, from 32 up (its `style` key); with "initially dark" (spawn flag 1) its
/// light is not in the faces until it is turned on.
pub fn dark_styles(bsp: &Bsp) -> Vec<u8> {
    bsp.entities
        .iter()
        .filter(|e| e.class_name().starts_with("light"))
        .filter(|e| e.get("spawnflags").and_then(|f| f.trim().parse::<i64>().ok()).is_some_and(|f| f & 1 != 0))
        .filter_map(|e| e.get("style").and_then(|s| s.trim().parse::<u8>().ok()))
        .filter(|&style| style >= 32)
        .collect()
}
