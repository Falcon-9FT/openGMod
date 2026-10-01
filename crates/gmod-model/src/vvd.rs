//! Model vertices (`.vvd`).
//!
//! Header: `"IDSV"`, `i32` version (4), `i32` checksum (the model's), `i32` level-of-detail
//! count, `i32`×8 vertices per level, `i32` fixup count and offset, `i32` offsets of the
//! vertices and of the tangents.
//!
//! A vertex is 48 bytes: 3 bone weights (`f32`), 3 bone indices and a bone count (bytes), the
//! position, the normal and the texture coordinates. Tangents are 4 floats each.
//!
//! Without fixups the vertices are the ones of the most detailed level. With them, the level
//! `L` takes, in order, every fixup range (`i32` level, first vertex, count) whose level is at
//! least `L`. The model's meshes count their vertices in the most detailed level, which is
//! what is read here.

use std::io;

use crate::reader::{Reader, invalid};

const FIXUP_SIZE: usize = 12;

/// A vertex of a model.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub bones: [u8; 3],
    pub weights: [f32; 3],
    pub bone_count: u8,
}

/// The vertices of a model, most detailed level.
#[derive(Debug, Clone, PartialEq)]
pub struct Vvd {
    pub checksum: i32,
    pub levels: usize,
    pub vertices: Vec<Vertex>,
    /// One per vertex (x y z, and w: the side of the bitangent), if the file has them.
    pub tangents: Vec<[f32; 4]>,
}

impl Vvd {
    pub fn parse(data: &[u8]) -> io::Result<Vvd> {
        let r = Reader::new(data, "vertices");
        if data.len() < 64 || !data.starts_with(b"IDSV") {
            return Err(invalid("not a model vertex file (.vvd)"));
        }
        let version = r.i32(4)?;
        if version != 4 {
            return Err(invalid(format!("vertex file version {version} is not known")));
        }
        let levels = r.count(12, "level of detail")?.clamp(1, 8);
        let total = r.count(16, "vertex")?;
        let fixup_count = r.count(48, "fixup")?;
        let fixup_at = r.offset(0, 52)?;
        let vertex_at = r.offset(0, 56)?;
        let tangent_at = r.i32(60)?;
        r.table(vertex_at, total, 48, "vertices")?;

        let mut ranges = Vec::new();
        if fixup_count == 0 {
            ranges.push((0, total));
        } else {
            // every fixup belongs to level 0 or a coarser one, so level 0 takes them all
            r.table(fixup_at, fixup_count, FIXUP_SIZE, "fixups")?;
            for i in 0..fixup_count {
                let at = fixup_at + i * FIXUP_SIZE;
                ranges.push((r.count(at + 4, "fixup vertex")?, r.count(at + 8, "fixup vertex")?));
            }
        }
        let mut vertices = Vec::with_capacity(total);
        let mut tangents = Vec::new();
        let with_tangents = tangent_at > 0 && (tangent_at as usize).checked_add(total * 16).is_some_and(|end| end <= data.len());
        for (first, count) in ranges {
            if first.checked_add(count).is_none_or(|end| end > total) || vertices.len() + count > total {
                return Err(invalid(format!("vertices: the fixups take more than the {total} vertices there are")));
            }
            for v in first..first + count {
                let at = vertex_at + v * 48;
                vertices.push(Vertex {
                    weights: [r.f32(at)?, r.f32(at + 4)?, r.f32(at + 8)?],
                    bones: [r.u8(at + 12)?, r.u8(at + 13)?, r.u8(at + 14)?],
                    bone_count: r.u8(at + 15)?,
                    position: r.vec3(at + 16)?,
                    normal: r.vec3(at + 28)?,
                    uv: [r.f32(at + 40)?, r.f32(at + 44)?],
                });
                if with_tangents {
                    let at = tangent_at as usize + v * 16;
                    tangents.push([r.f32(at)?, r.f32(at + 4)?, r.f32(at + 8)?, r.f32(at + 12)?]);
                }
            }
        }
        Ok(Vvd { checksum: r.i32(8)?, levels, vertices, tangents })
    }
}
