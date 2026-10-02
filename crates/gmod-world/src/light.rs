//! The light on a model of an entity, as the engine lights what moves.
//!
//! A model takes the ambient light cube of the sample nearest to it in its leaf of the map -
//! the light from the six sides that `vrad` baked there: what bounced off the world, the sky's,
//! lit textures' - and the strongest lights of the map that see it, added directly: points
//! and spots falling off with distance as their constant, linear and quadratic terms say, a
//! spot only within its cone, the sun only where the sky shows in its direction. Light is
//! linear, as the world's light maps are.

use gmod_bsp::{AmbientSample, Bsp, Collision, LightCube, LightKind, WorldLight, mask};

/// The lights a model takes at most, as the engine's `r_worldlights` is about.
pub const MAX_LIGHTS: usize = 2;
/// Less light than this from a light is none.
const LEAST_LIGHT: f32 = 0.002;
/// How far away the sky is looked for.
const SKY_DISTANCE: f32 = 32768.0;

/// The light a model is lit by.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelLight {
    /// From +x, -x, +y, -y, +z, -z.
    pub ambient: LightCube,
    /// The direction towards each light, in the world, and the light that comes from it.
    pub lights: Vec<([f32; 3], [f32; 3])>,
}

impl Default for ModelLight {
    /// What a map without baked light gives: some from everywhere, more from above.
    fn default() -> ModelLight {
        let side = [0.35; 3];
        ModelLight { ambient: [side, side, side, side, [0.85; 3], [0.2; 3]], lights: Vec::new() }
    }
}

/// The light of a map for its entities.
#[derive(Debug, Clone, Default)]
pub struct MapLight {
    /// The samples of each leaf.
    samples: Vec<Vec<AmbientSample>>,
    lights: Vec<WorldLight>,
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn length(a: [f32; 3]) -> f32 {
    dot(a, a).sqrt()
}

fn scale(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

/// How bright a colour is, as the eye sees it.
fn luminance(c: [f32; 3]) -> f32 {
    0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]
}

impl MapLight {
    pub fn new(bsp: &Bsp) -> MapLight {
        let lights = bsp
            .world_lights()
            .into_iter()
            .filter(|l| matches!(l.kind, LightKind::Point | LightKind::Spot | LightKind::Sky | LightKind::Quake))
            .collect();
        MapLight { samples: bsp.ambient_samples().unwrap_or_default(), lights }
    }

    /// Whether the map has light of its own for models.
    pub fn is_empty(&self) -> bool {
        self.lights.is_empty() && self.samples.iter().all(Vec::is_empty)
    }

    /// The light at a point of the map - an entity's origin, often on the floor: when its leaf
    /// has no samples (inside the floor), a little above it.
    pub fn at(&self, collision: &Collision, origin: [f32; 3]) -> ModelLight {
        let mut light = ModelLight::default();
        if self.is_empty() {
            return light;
        }
        let leaf_samples = |p: [f32; 3]| collision.leaf_at(p).and_then(|leaf| self.samples.get(leaf)).filter(|s| !s.is_empty());
        let (point, samples) = [0.0, 16.0, 48.0]
            .iter()
            .map(|up| [origin[0], origin[1], origin[2] + up])
            .find_map(|p| leaf_samples(p).map(|s| (p, Some(s))))
            .unwrap_or((origin, None));
        if let Some(samples) = samples {
            let nearest = samples.iter().min_by(|a, b| length(sub(a.position, point)).total_cmp(&length(sub(b.position, point))));
            if let Some(sample) = nearest {
                light.ambient = sample.cube;
            }
        } else {
            // in a leaf with no samples (inside a wall, outside the map): nothing baked
            light.ambient = [[0.05; 3]; 6];
        }
        // the strongest first; each must see the point
        let mut candidates: Vec<([f32; 3], [f32; 3])> = self.lights.iter().filter_map(|l| self.reaching(l, point)).collect();
        candidates.sort_by(|a, b| luminance(b.1).total_cmp(&luminance(a.1)));
        for (towards, color) in candidates {
            if light.lights.len() == MAX_LIGHTS {
                break;
            }
            if self.sees(collision, point, towards) {
                light.lights.push((towards, color));
            }
        }
        light
    }

    /// How a light reaches a point, before anything is in the way: the direction towards it
    /// (for the sun, how far to look for the sky) and its light there.
    fn reaching(&self, l: &WorldLight, point: [f32; 3]) -> Option<([f32; 3], [f32; 3])> {
        if l.kind == LightKind::Sky {
            let towards = scale(l.normal, -1.0);
            let norm = length(towards);
            return (norm > 0.0 && luminance(l.intensity) > LEAST_LIGHT).then(|| (scale(towards, SKY_DISTANCE / norm), l.intensity));
        }
        let to_light = sub(l.origin, point);
        let distance = length(to_light).max(1.0);
        if l.radius > 0.0 && distance > l.radius {
            return None;
        }
        let falloff = l.constant + l.linear * distance + l.quadratic * distance * distance;
        let mut strength = if falloff > 0.0 { 1.0 / falloff } else { 1.0 / (distance * distance) };
        if l.kind == LightKind::Spot {
            // the cone: full inside stopdot, nothing outside stopdot2, a ramp between
            let along = -dot(scale(to_light, 1.0 / distance), l.normal);
            if along < l.stopdot2 {
                return None;
            }
            if along < l.stopdot && l.stopdot > l.stopdot2 {
                strength *= (along - l.stopdot2) / (l.stopdot - l.stopdot2);
            }
            if l.exponent > 0.0 && l.exponent != 1.0 {
                strength *= along.max(0.0).powf(l.exponent);
            }
        }
        let color = scale(l.intensity, strength);
        (luminance(color) > LEAST_LIGHT).then_some((to_light, color))
    }

    /// Whether nothing opaque stands between a point and its light; the sun needs the sky.
    fn sees(&self, collision: &Collision, point: [f32; 3], towards: [f32; 3]) -> bool {
        let end = [point[0] + towards[0], point[1] + towards[1], point[2] + towards[2]];
        let trace = collision.trace_line(point, end, mask::OPAQUE);
        if length(towards) >= SKY_DISTANCE - 1.0 {
            // the sun: the line must end on the sky
            trace.fraction < 1.0 && trace.surface_flags & gmod_bsp::surf::SKY != 0
        } else {
            trace.fraction > 0.99
        }
    }
}

/// The direction to each light and the cube, turned into a model's own space by its axes
/// (forward, left, up): what its shader takes.
pub fn in_model_space(light: &ModelLight, axes: [[f32; 3]; 3]) -> ModelLight {
    // the cube turns with the model: a side's light is what comes from that side in the world
    let world = |d: [f32; 3]| -> [f32; 3] {
        let n = [
            axes[0][0] * d[0] + axes[1][0] * d[1] + axes[2][0] * d[2],
            axes[0][1] * d[0] + axes[1][1] * d[1] + axes[2][1] * d[2],
            axes[0][2] * d[0] + axes[1][2] * d[1] + axes[2][2] * d[2],
        ];
        cube_light(&light.ambient, n)
    };
    let sides = [[1.0, 0.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0, -1.0]];
    let ambient = sides.map(world);
    let lights = light
        .lights
        .iter()
        .map(|&(towards, color)| {
            let n = length(towards).max(1e-6);
            let d = scale(towards, 1.0 / n);
            ([dot(d, axes[0]), dot(d, axes[1]), dot(d, axes[2])], color)
        })
        .collect();
    ModelLight { ambient, lights }
}

/// The light a cube gives a surface facing `normal`, as the engine weighs the sides: by the
/// squares of the normal's parts.
pub fn cube_light(cube: &LightCube, normal: [f32; 3]) -> [f32; 3] {
    let n = scale(normal, 1.0 / length(normal).max(1e-6));
    let w = [n[0] * n[0], n[1] * n[1], n[2] * n[2]];
    let side = |axis: usize| if n[axis] >= 0.0 { cube[axis * 2] } else { cube[axis * 2 + 1] };
    std::array::from_fn(|c| w[0] * side(0)[c] + w[1] * side(1)[c] + w[2] * side(2)[c])
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gmod_bsp::testmap::{TestBrush, TestLight, TestMap};
    use gmod_bsp::{Bsp, Collision, surf};

    use super::*;

    /// A floor, a sky high above, a wall at x 100; light from above in every leaf, a lamp at
    /// x 200 behind the wall, the sun straight down.
    fn room(lights: Vec<TestLight>) -> (MapLight, Collision) {
        let brushes = vec![
            TestBrush::cuboid([-1024.0, -1024.0, -16.0], [1024.0, 1024.0, 0.0]),
            TestBrush::cuboid([-1024.0, -1024.0, 1024.0], [1024.0, 1024.0, 1040.0]).material("TOOLS/TOOLSSKYBOX", surf::SKY),
            TestBrush::cuboid([100.0, -1024.0, 0.0], [116.0, 1024.0, 512.0]),
        ];
        let half = [128, 128, 128, (-1i8) as u8];
        let map = TestMap {
            models: vec![brushes],
            ambient: Some([half, half, half, half, [255, 255, 255, 0], half]),
            lights,
            ..TestMap::default()
        };
        let bsp = Bsp::parse("room.bsp".into(), Arc::from(map.bytes())).unwrap();
        (MapLight::new(&bsp), Collision::new(&bsp))
    }

    #[test]
    fn models_take_the_light_of_the_map() {
        let lamp = TestLight { kind: 1, origin: [200.0, 0.0, 64.0], intensity: [10000.0, 10000.0, 0.0], normal: [0.0; 3] };
        let (light, collision) = room(vec![lamp]);
        // the cube of the leaf
        let here = light.at(&collision, [0.0, 0.0, 64.0]);
        assert_eq!(here.ambient[4], [1.0; 3]);
        assert!((here.ambient[0][0] - 128.0 / 255.0 / 2.0).abs() < 1e-5);
        // the lamp is behind the wall
        assert!(here.lights.is_empty(), "{:?}", here.lights);
        // on its side of the wall it lights, falling off as the square of the distance
        let near = light.at(&collision, [200.0, 0.0, 0.0]);
        assert_eq!(near.lights.len(), 1);
        let (towards, color) = near.lights[0];
        assert!(towards[2] > 0.0 && (color[0] - 10000.0 / (64.0 * 64.0)).abs() < 0.01 && color[2] == 0.0, "{:?}", near.lights);

        // the sun lights where the sky is above
        let sun = TestLight { kind: 3, origin: [0.0; 3], intensity: [2.0, 1.8, 1.5], normal: [0.0, 0.0, -1.0] };
        let (light, collision) = room(vec![sun]);
        let outside = light.at(&collision, [0.0, 0.0, 64.0]);
        assert_eq!(outside.lights.len(), 1);
        assert!(outside.lights[0].0[2] > 0.0 && outside.lights[0].1 == [2.0, 1.8, 1.5]);
    }

    #[test]
    fn the_light_turns_with_the_model() {
        let cube = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0; 3], [0.0; 3], [0.0; 3], [0.0; 3]];
        let light = ModelLight { ambient: cube, lights: vec![([0.0, 0.0, 10.0], [1.0; 3])] };
        // a model turned to face -x: its forward side gets the light of -x
        let turned = in_model_space(&light, [[-1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0]]);
        assert_eq!(turned.ambient[0], [0.0, 1.0, 0.0]);
        assert_eq!(turned.ambient[1], [1.0, 0.0, 0.0]);
        assert_eq!(turned.lights[0].0, [0.0, 0.0, 1.0]);
        // a surface between two sides takes both, by the squares
        let between = cube_light(&cube, [1.0, 0.0, 1.0]);
        assert!((between[0] - 0.5).abs() < 1e-6);
    }
}
