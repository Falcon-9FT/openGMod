//! The light of a map for what moves in it: ambient light cubes and the lights themselves.
//!
//! The compiler (`vrad`) samples the light in the empty leaves of the map: at each sample, the
//! light coming from each of the six directions (+x, -x, +y, -y, +z, -z), what bounced off the
//! world and the sky's - an ambient light cube. Version 20 maps keep them in their own lumps
//! (`LEAF_AMBIENT_INDEX` gives each leaf a range of `LEAF_AMBIENT_LIGHTING`, the samples placed
//! inside the leaf's box by three bytes); version 19 keeps one cube in every leaf.
//!
//! The lights (`WORLDLIGHTS`) light what moves directly: the engine adds the strongest that see
//! a model to its cube. Both come as HDR and LDR; the HDR ones are taken when the map has them,
//! as the world's light maps are.

use crate::lighting::rgbe_to_linear;
use crate::lumps::{Fields, Leaf, lump_bytes};
use crate::{Bsp, invalid};

/// Lumps of light.
mod index {
    pub const WORLDLIGHTS: usize = 15;
    pub const LEAF_AMBIENT_INDEX_HDR: usize = 51;
    pub const LEAF_AMBIENT_INDEX: usize = 52;
    pub const WORLDLIGHTS_HDR: usize = 54;
    pub const LEAF_AMBIENT_LIGHTING_HDR: usize = 55;
    pub const LEAF_AMBIENT_LIGHTING: usize = 56;
    pub const LEAFS: usize = 10;
}

/// Linear light from +x, -x, +y, -y, +z and -z.
pub type LightCube = [[f32; 3]; 6];

/// One sample of the ambient light.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AmbientSample {
    pub cube: LightCube,
    /// Where, in the world.
    pub position: [f32; 3],
}

/// What a light is (`emittype_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightKind {
    /// A lit texture: its light is in the cubes already.
    Surface,
    Point,
    /// A cone: `stopdot` and `stopdot2` its inner and outer cosines.
    Spot,
    /// The sun: light along `normal` from far away, where the sky shows.
    Sky,
    /// A point light with Quake's straight fall-off.
    Quake,
    /// The sky's light from every direction: in the cubes already.
    SkyAmbient,
    Other(i32),
}

/// A light of the map (`dworldlight_t`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldLight {
    pub origin: [f32; 3],
    /// Linear light; how bright it is at a distance is this over the fall-off.
    pub intensity: [f32; 3],
    /// Where it shines: a spot's axis, the direction the sun's light travels.
    pub normal: [f32; 3],
    pub cluster: i32,
    pub kind: LightKind,
    pub style: i32,
    pub stopdot: f32,
    pub stopdot2: f32,
    pub exponent: f32,
    /// Beyond this distance it gives nothing (0: no limit).
    pub radius: f32,
    pub constant: f32,
    pub linear: f32,
    pub quadratic: f32,
    pub flags: i32,
}

impl WorldLight {
    /// The size of a record in maps of version 19 and 20; version 21 adds 12 bytes.
    const SIZE: usize = 88;
    const SIZE_V21: usize = 100;

    fn parse(f: &mut Fields) -> WorldLight {
        let origin = f.vec3();
        let intensity = f.vec3();
        let normal = f.vec3();
        let cluster = f.i32();
        let kind = match f.i32() {
            0 => LightKind::Surface,
            1 => LightKind::Point,
            2 => LightKind::Spot,
            3 => LightKind::Sky,
            4 => LightKind::Quake,
            5 => LightKind::SkyAmbient,
            other => LightKind::Other(other),
        };
        WorldLight {
            origin,
            intensity,
            normal,
            cluster,
            kind,
            style: f.i32(),
            stopdot: f.f32(),
            stopdot2: f.f32(),
            exponent: f.f32(),
            radius: f.f32(),
            constant: f.f32(),
            linear: f.f32(),
            quadratic: f.f32(),
            flags: f.i32(),
        }
    }
}

/// A light cube as stored: six `ColorRGBExp32`.
fn cube(f: &mut Fields) -> LightCube {
    std::array::from_fn(|_| rgbe_to_linear([f.u8(), f.u8(), f.u8(), f.u8()]))
}

impl Bsp {
    fn raw_lump(&self, index: usize) -> Vec<u8> {
        lump_bytes(&self.bytes, &self.lumps[index], "light").map(|b| b.into_owned()).unwrap_or_default()
    }

    /// The lights of the map: the HDR ones if there are any.
    pub fn world_lights(&self) -> Vec<WorldLight> {
        let hdr = self.raw_lump(index::WORLDLIGHTS_HDR);
        let bytes = if hdr.is_empty() { self.raw_lump(index::WORLDLIGHTS) } else { hdr };
        let size =
            if self.version >= 21 && bytes.len().is_multiple_of(WorldLight::SIZE_V21) && !bytes.len().is_multiple_of(WorldLight::SIZE) {
                WorldLight::SIZE_V21
            } else {
                WorldLight::SIZE
            };
        if !bytes.len().is_multiple_of(size) {
            return Vec::new();
        }
        // the record's tail (texinfo, owner; version 21's shadow offset) is not needed
        bytes.chunks(size).map(|chunk| WorldLight::parse(&mut Fields::new(chunk))).collect()
    }

    /// The ambient light samples of each leaf, by leaf (empty for leaves with none): the HDR
    /// ones if there are any.
    pub fn ambient_samples(&self) -> std::io::Result<Vec<Vec<AmbientSample>>> {
        let leaf_box = |leaf: &Leaf, at: [u8; 3]| -> [f32; 3] {
            std::array::from_fn(|i| {
                let (lo, hi) = (f32::from(leaf.mins[i]), f32::from(leaf.maxs[i]));
                lo + (hi - lo) * f32::from(at[i]) / 255.0
            })
        };
        let middle = |leaf: &Leaf| leaf_box(leaf, [128; 3]);
        // version 19: one cube in every leaf, after its first 32 bytes
        if self.lumps[index::LEAFS].version == 0 {
            let bytes = self.raw_lump(index::LEAFS);
            if bytes.len() == self.leaves.len() * Leaf::SIZE_WITH_LIGHT {
                return Ok(bytes
                    .chunks(Leaf::SIZE_WITH_LIGHT)
                    .zip(&self.leaves)
                    .map(|(chunk, leaf)| {
                        let mut f = Fields::new(&chunk[Leaf::SIZE - 2..]);
                        vec![AmbientSample { cube: cube(&mut f), position: middle(leaf) }]
                    })
                    .collect());
            }
        }
        // the HDR samples, unless they are not there or hold nothing: some maps (gm_construct)
        // have HDR ambient lumps next to black everywhere beside LDR ones of the real light
        let hdr = self.ambient_samples_of(true)?;
        if hdr.iter().any(|s| !s.is_empty()) && !holds_nothing(&hdr) {
            return Ok(hdr);
        }
        let ldr = self.ambient_samples_of(false)?;
        Ok(if ldr.iter().any(|s| !s.is_empty()) { ldr } else { hdr })
    }

    /// The ambient light samples of each leaf from the HDR lumps or the LDR ones (version 20
    /// and later), empty for each leaf when the map has none of those.
    pub fn ambient_samples_of(&self, hdr: bool) -> std::io::Result<Vec<Vec<AmbientSample>>> {
        let leaf_box = |leaf: &Leaf, at: [u8; 3]| -> [f32; 3] {
            std::array::from_fn(|i| {
                let (lo, hi) = (f32::from(leaf.mins[i]), f32::from(leaf.maxs[i]));
                lo + (hi - lo) * f32::from(at[i]) / 255.0
            })
        };
        let (indices, samples) = if hdr {
            (self.raw_lump(index::LEAF_AMBIENT_INDEX_HDR), self.raw_lump(index::LEAF_AMBIENT_LIGHTING_HDR))
        } else {
            (self.raw_lump(index::LEAF_AMBIENT_INDEX), self.raw_lump(index::LEAF_AMBIENT_LIGHTING))
        };
        if indices.is_empty() {
            return Ok(vec![Vec::new(); self.leaves.len()]);
        }
        if !indices.len().is_multiple_of(4) || !samples.len().is_multiple_of(28) {
            return Err(invalid("the ambient light lumps are not whole records".to_string()));
        }
        let samples: Vec<(LightCube, [u8; 3])> = samples
            .chunks(28)
            .map(|chunk| {
                let mut f = Fields::new(chunk);
                (cube(&mut f), [f.u8(), f.u8(), f.u8()])
            })
            .collect();
        Ok(indices
            .chunks(4)
            .zip(&self.leaves)
            .map(|(chunk, leaf)| {
                let count = usize::from(u16::from_le_bytes([chunk[0], chunk[1]]));
                let first = usize::from(u16::from_le_bytes([chunk[2], chunk[3]]));
                samples
                    .get(first..first + count)
                    .unwrap_or_default()
                    .iter()
                    .map(|&(cube, at)| AmbientSample { cube, position: leaf_box(leaf, at) })
                    .collect()
            })
            .collect())
    }
}

/// Whether samples hold no light worth the name: all of them next to black.
fn holds_nothing(samples: &[Vec<AmbientSample>]) -> bool {
    samples.iter().flatten().flat_map(|s| s.cube.iter().flatten()).all(|&v| v < 0.01)
}
