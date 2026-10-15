//! What the game draws in the world by itself ([`gmod_lua::render3d`]: Lua's beams, sprites and
//! quads, the physics gun's beam) made into triangles for the renderer: the beams and sprites
//! turned to face the camera, each with the texture of its material, blended or added.

use std::collections::HashMap;
use std::sync::Arc;

use gmod_fs::Vfs;
use gmod_lua::render3d::{Material3d, Primitive, Shape};
use gmod_lua::vector::Vector;
use gmod_render::{EffectBatch, EffectVertex};
use gmod_world::gpu::Camera;

/// A material as Lua gives it: its name, and the base texture and additivity it may say.
type MaterialKey = (String, Option<String>, Option<bool>);

/// What a material shows: its texture, if it has one there, and whether it adds its light.
type Look = (Option<gmod_world::Texture>, bool);

/// The materials of effects, read once: their base texture and whether they add their light.
pub struct Materials {
    vfs: Arc<Vfs>,
    looks: HashMap<MaterialKey, Look>,
}

impl Materials {
    pub fn new(vfs: Arc<Vfs>) -> Materials {
        Materials { vfs, looks: HashMap::new() }
    }

    /// The texture a material shows (none for a plain colour, or one that is not there) and
    /// whether it adds to what is behind it.
    fn look(&mut self, material: &Material3d) -> Look {
        let key = (material.name.to_ascii_lowercase(), material.texture.clone(), material.additive);
        if let Some(known) = self.looks.get(&key) {
            return known.clone();
        }
        let vmt = (!key.0.is_empty() && key.0 != "color").then(|| gmod_material::load_material(&self.vfs, &key.0).ok()).flatten();
        // no material of the name: it may name a texture itself (`surface.GetTextureID`)
        let named = || (vmt.is_none() && !key.0.is_empty() && key.0 != "color").then(|| key.0.clone());
        let texture_name = material.texture.clone().or_else(|| vmt.as_ref().and_then(|m| m.texture("$basetexture"))).or_else(named);
        let additive = material.additive.unwrap_or_else(|| vmt.as_ref().is_some_and(|m| m.flag("$additive")));
        let texture = texture_name.and_then(|name| {
            let vtf = gmod_material::load_texture(&self.vfs, &name).ok()?;
            Some(gmod_world::Texture { name, vtf: Arc::new(vtf) })
        });
        let look = (texture, additive);
        self.looks.insert(key, look.clone());
        look
    }
}

fn v3(v: Vector) -> [f32; 3] {
    [v.x, v.y, v.z]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l < 1e-6 { [0.0; 3] } else { v.map(|c| c / l) }
}

fn along(p: [f32; 3], d: [f32; 3], s: f32) -> [f32; 3] {
    [p[0] + d[0] * s, p[1] + d[1] * s, p[2] + d[2] * s]
}

/// Two triangles of a quad, its corners in order round it.
fn quad(out: &mut Vec<EffectVertex>, corners: [[f32; 3]; 4], uvs: [[f32; 2]; 4], colors: [[f32; 4]; 4]) {
    for i in [0, 1, 2, 0, 2, 3] {
        out.push(EffectVertex { position: corners[i], uv: uvs[i], color: colors[i] });
    }
}

/// The triangles of the primitives, as seen from `camera`, in batches of one texture each, in
/// the order they were drawn; the textures they need are handed to `upload` once.
pub fn batches(
    primitives: &[Primitive],
    camera: &Camera,
    materials: &mut Materials,
    mut upload: impl FnMut(&gmod_world::Texture),
) -> Vec<EffectBatch> {
    let eye = camera.eye;
    let (forward, right) = (camera.forward(), camera.right());
    let up = cross(right, forward);
    let mut out: Vec<EffectBatch> = Vec::new();
    for primitive in primitives {
        let (texture, additive) = materials.look(&primitive.material);
        if let Some(texture) = &texture {
            upload(texture);
        }
        let name = texture.map(|t| t.name);
        if out.last().is_none_or(|b| b.texture != name || b.additive != additive) {
            out.push(EffectBatch { texture: name, additive, vertices: Vec::new() });
        }
        let vertices = &mut out.last_mut().expect("a batch").vertices;
        match &primitive.shape {
            // a strip through the points, as wide as each says, across the line of sight
            Shape::Beam(points) => {
                let side = |i: usize| {
                    let p = v3(points[i].pos);
                    let next = v3(points[(i + 1).min(points.len() - 1)].pos);
                    let prev = v3(points[i.saturating_sub(1)].pos);
                    let tangent = normalize(sub(next, prev));
                    normalize(cross(tangent, sub(eye, p)))
                };
                for i in 0..points.len().saturating_sub(1) {
                    let (a, b) = (&points[i], &points[i + 1]);
                    let (sa, sb) = (side(i), side(i + 1));
                    let (pa, pb) = (v3(a.pos), v3(b.pos));
                    let corners = [
                        along(pa, sa, -a.width / 2.0),
                        along(pa, sa, a.width / 2.0),
                        along(pb, sb, b.width / 2.0),
                        along(pb, sb, -b.width / 2.0),
                    ];
                    quad(vertices, corners, [[0.0, a.v], [1.0, a.v], [1.0, b.v], [0.0, b.v]], [a.color, a.color, b.color, b.color]);
                }
            }
            // a rectangle facing the camera
            Shape::Sprite { pos, width, height, color } => {
                let p = v3(*pos);
                let (r, u) = (right.map(|c| c * width / 2.0), up.map(|c| c * height / 2.0));
                let corner = |sr: f32, su: f32| [p[0] + r[0] * sr + u[0] * su, p[1] + r[1] * sr + u[1] * su, p[2] + r[2] * sr + u[2] * su];
                let corners = [corner(-1.0, 1.0), corner(1.0, 1.0), corner(1.0, -1.0), corner(-1.0, -1.0)];
                quad(vertices, corners, [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]], [*color; 4]);
            }
            Shape::Triangles(corners) => {
                vertices.extend(corners.iter().map(|c| EffectVertex { position: v3(c.pos), uv: c.uv, color: c.color }));
            }
            Shape::Quad { corners, color } => {
                quad(vertices, corners.map(v3), [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]], [*color; 4]);
            }
        }
    }
    out.retain(|b| !b.vertices.is_empty());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use gmod_lua::render3d::BeamPoint;

    #[test]
    fn beams_face_the_camera_and_sprites_are_squares() {
        let camera = Camera { eye: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, fov: 75.0 };
        let mut materials = Materials::new(Arc::new(Vfs::new()));
        let color = [1.0; 4];
        let beam = Shape::Beam(vec![
            BeamPoint { pos: Vector::new(100.0, -50.0, 0.0), width: 10.0, v: 0.0, color },
            BeamPoint { pos: Vector::new(100.0, 50.0, 0.0), width: 10.0, v: 1.0, color },
        ]);
        let sprite = Shape::Sprite { pos: Vector::new(100.0, 0.0, 0.0), width: 4.0, height: 4.0, color };
        let plain = Material3d { name: "color".into(), texture: None, additive: Some(true) };
        let primitives = [Primitive { material: plain.clone(), shape: beam }, Primitive { material: plain, shape: sprite }];
        let batches = batches(&primitives, &camera, &mut materials, |_| {});
        // one batch: the same material, plain white, added
        assert_eq!(batches.len(), 1);
        assert!(batches[0].additive && batches[0].texture.is_none());
        let v = &batches[0].vertices;
        assert_eq!(v.len(), 12);
        // the beam along y, seen along x: its width goes up and down, 10 across
        assert!((v[1].position[2] - v[0].position[2]).abs() > 9.9);
        assert!(v[..6].iter().all(|c| (c.position[0] - 100.0).abs() < 1e-3));
        // the sprite: 4 by 4 about its middle, across the line of sight
        let ys: Vec<f32> = v[6..].iter().map(|c| c.position[1]).collect();
        assert!(ys.iter().any(|&y| (y - 2.0).abs() < 1e-3) && ys.iter().any(|&y| (y + 2.0).abs() < 1e-3));
    }
}
