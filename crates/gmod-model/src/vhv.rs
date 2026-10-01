//! Baked light of static props (`sp_<n>.vhv` and `sp_hdr_<n>.vhv` in a map's pakfile, `n`
//! the prop's number): one colour per hardware vertex.
//!
//! Header (40 bytes): `i32` version (2), `u32` checksum (the model's), `u32` vertex flags,
//! `u32` bytes per vertex (4), `u32` vertex count, `i32` mesh count and 4 unused words. Then a
//! 28-byte record per mesh: `u32` level of detail, vertex count and offset of its colours, 4
//! unused. The meshes come as the `.vtx` nests them: body parts, models, levels, meshes.
//!
//! A colour is B, G, R and A bytes; the colour is linear light, 255 standing for 1. (Compared
//! with the light maps of the floor around 454 props of a TF2 map: the ratio is 0.9 for this
//! reading, and the first byte follows the blue of the light.)

use std::io;

use crate::Vtx;
use crate::reader::{Reader, invalid};

/// Colours of the most detailed level: by body part, model, mesh and hardware vertex.
pub type Colors = Vec<Vec<Vec<Vec<[f32; 3]>>>>;

/// The colours of one mesh at one level of detail.
#[derive(Debug, Clone, PartialEq)]
pub struct Mesh {
    pub level: u32,
    pub colors: Vec<[f32; 3]>,
}

/// A static prop's baked light.
#[derive(Debug, Clone, PartialEq)]
pub struct Vhv {
    pub checksum: i32,
    pub meshes: Vec<Mesh>,
}

impl Vhv {
    pub fn parse(data: &[u8]) -> io::Result<Vhv> {
        let r = Reader::new(data, "prop light");
        if data.len() < 40 || r.i32(0)? != 2 {
            return Err(invalid("not a static prop light file (.vhv) of version 2"));
        }
        let vertex_size = r.count(12, "vertex size")?;
        if vertex_size < 4 {
            return Err(invalid(format!("prop light: {vertex_size} bytes a vertex")));
        }
        let mesh_count = r.count(20, "mesh")?;
        r.table(40, mesh_count, 28, "meshes")?;
        let meshes = (0..mesh_count)
            .map(|m| {
                let at = 40 + m * 28;
                let count = r.count(at + 4, "vertex")?;
                let offset = r.count(at + 8, "offset")?;
                r.table(offset, count, vertex_size, "colours")?;
                let colors = (0..count)
                    .map(|v| {
                        let c = r.bytes(offset + v * vertex_size, 4)?;
                        Ok([c[2], c[1], c[0]].map(|b| f32::from(b) / 255.0))
                    })
                    .collect::<io::Result<Vec<[f32; 3]>>>()?;
                Ok(Mesh { level: r.i32(at)? as u32, colors })
            })
            .collect::<io::Result<Vec<Mesh>>>()?;
        Ok(Vhv { checksum: r.i32(4)?, meshes })
    }

    /// The colours of the most detailed level, by body part, model and mesh - if they match
    /// the model's triangles mesh for mesh; `None` otherwise.
    pub fn levels_of(&self, vtx: &Vtx) -> Option<Colors> {
        if self.checksum != vtx.checksum {
            return None;
        }
        let mut next = self.meshes.iter();
        let mut out = Vec::new();
        for part in &vtx.body_parts {
            let mut models = Vec::new();
            for levels in part {
                let mut first = Vec::new();
                for (l, level) in levels.iter().enumerate() {
                    for mesh in &level.meshes {
                        let baked = next.next()?;
                        if baked.level as usize != l || baked.colors.len() != mesh.vertices.len() {
                            return None;
                        }
                        if l == 0 {
                            first.push(baked.colors.clone());
                        }
                    }
                }
                models.push(first);
            }
            out.push(models);
        }
        Some(out)
    }
}
