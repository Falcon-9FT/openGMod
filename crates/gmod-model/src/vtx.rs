//! Model triangles (`.dx90.vtx`, `.vtx`): the vertices of each mesh put in an order fit for
//! drawing, per level of detail.
//!
//! Header (36 bytes): `i32` version (7), vertex cache size, `u16` bones per strip and per
//! triangle, `i32` bones per vertex, `i32` checksum (the model's), `i32` level count, offset of
//! the material replacements, `i32` body part count and offset.
//!
//! Then nested tables mirroring the model: body parts (count and offset, 8 bytes) hold models
//! (8), models hold levels of detail (count, offset and the distance to switch at, 12), levels
//! hold meshes (count, offset, flags: 9 bytes), meshes hold strip groups. Every offset counts
//! from the record that holds it. A strip group (25 bytes) has vertices (9 bytes: bone data and
//! the `u16` index of the vertex in its mesh), `u16` indices into those vertices and strips
//! (27 bytes) that cut the indices into triangle lists or strips. Files from newer compilers
//! add two fields to strip groups and strips (33 and 35 bytes); a file is read with whichever
//! layout fits it.

use std::io;

use crate::reader::{Reader, invalid};

const VERTEX_SIZE: usize = 9;
const STRIP_IS_TRIANGLE_STRIP: u8 = 2;

/// One mesh at one level of detail.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mesh {
    /// Triangles: three indices into the mesh's vertices each.
    pub triangles: Vec<u32>,
    /// The vertices as the hardware gets them - the strip groups' vertex lists one after
    /// another - each the index of a mesh vertex. Data made per hardware vertex (the light of
    /// static props) comes in this order.
    pub vertices: Vec<u32>,
}

/// One level of detail of a model.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Level {
    /// The distance at which this level takes over.
    pub switch_point: f32,
    pub meshes: Vec<Mesh>,
}

/// The triangles of a model file.
#[derive(Debug, Clone, PartialEq)]
pub struct Vtx {
    pub checksum: i32,
    /// For each body part, each model, its levels of detail.
    pub body_parts: Vec<Vec<Vec<Level>>>,
}

/// Sizes of strip groups and strips.
#[derive(Clone, Copy)]
struct Layout {
    group: usize,
    strip: usize,
}

const LAYOUTS: [Layout; 2] = [Layout { group: 25, strip: 27 }, Layout { group: 33, strip: 35 }];

impl Vtx {
    pub fn parse(data: &[u8]) -> io::Result<Vtx> {
        let r = Reader::new(data, "triangles");
        if data.len() < 36 {
            return Err(invalid("not a model triangle file (.vtx)"));
        }
        let version = r.i32(0)?;
        if version != 7 {
            return Err(invalid(format!("triangle file version {version} is not known")));
        }
        let mut first_error = None;
        for layout in LAYOUTS {
            match read_body_parts(&r, layout) {
                Ok(body_parts) => return Ok(Vtx { checksum: r.i32(16)?, body_parts }),
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        Err(first_error.unwrap())
    }
}

fn read_body_parts(r: &Reader, layout: Layout) -> io::Result<Vec<Vec<Vec<Level>>>> {
    let part_count = r.count(28, "body part")?;
    let part_at = r.offset(0, 32)?;
    r.table(part_at, part_count, 8, "body parts")?;
    (0..part_count)
        .map(|p| {
            let at = part_at + p * 8;
            let model_count = r.count(at, "model")?;
            let model_at = r.offset(at, at + 4)?;
            r.table(model_at, model_count, 8, "models")?;
            (0..model_count)
                .map(|m| {
                    let at = model_at + m * 8;
                    let level_count = r.count(at, "level of detail")?;
                    let level_at = r.offset(at, at + 4)?;
                    r.table(level_at, level_count, 12, "levels of detail")?;
                    (0..level_count).map(|l| read_level(r, level_at + l * 12, layout)).collect()
                })
                .collect()
        })
        .collect()
}

fn read_level(r: &Reader, at: usize, layout: Layout) -> io::Result<Level> {
    let mesh_count = r.count(at, "mesh")?;
    let mesh_at = r.offset(at, at + 4)?;
    r.table(mesh_at, mesh_count, 9, "meshes")?;
    let meshes = (0..mesh_count)
        .map(|k| {
            let at = mesh_at + k * 9;
            let group_count = r.count(at, "strip group")?;
            let group_at = r.offset(at, at + 4)?;
            r.table(group_at, group_count, layout.group, "strip groups")?;
            let mut mesh = Mesh::default();
            for g in 0..group_count {
                read_strip_group(r, group_at + g * layout.group, layout, &mut mesh)?;
            }
            Ok(mesh)
        })
        .collect::<io::Result<Vec<Mesh>>>()?;
    Ok(Level { switch_point: r.f32(at + 8)?, meshes })
}

/// Appends a strip group's vertices and triangles, as indices into its mesh's vertices.
fn read_strip_group(r: &Reader, at: usize, layout: Layout, mesh: &mut Mesh) -> io::Result<()> {
    let vertex_count = r.count(at, "strip group vertex")?;
    let vertex_at = r.offset(at, at + 4)?;
    let index_count = r.count(at + 8, "strip group index")?;
    let index_at = r.offset(at, at + 12)?;
    let strip_count = r.count(at + 16, "strip")?;
    let strip_at = r.offset(at, at + 20)?;
    r.table(vertex_at, vertex_count, VERTEX_SIZE, "strip group vertices")?;
    r.table(index_at, index_count, 2, "strip group indices")?;
    r.table(strip_at, strip_count, layout.strip, "strips")?;
    for v in 0..vertex_count {
        mesh.vertices.push(u32::from(r.u16(vertex_at + v * VERTEX_SIZE + 4)?));
    }
    let triangles = &mut mesh.triangles;
    // the vertex of the mesh behind each of the group's vertices
    let mesh_vertex = |i: usize| -> io::Result<u32> {
        let v = usize::from(r.u16(index_at + i * 2)?);
        if v >= vertex_count {
            return Err(invalid(format!("triangles: index {v} of a strip group with {vertex_count} vertices")));
        }
        Ok(u32::from(r.u16(vertex_at + v * VERTEX_SIZE + 4)?))
    };
    if strip_count == 0 {
        // no strips: the indices are one triangle list
        for i in 0..index_count - index_count % 3 {
            triangles.push(mesh_vertex(i)?);
        }
    }
    for s in 0..strip_count {
        let at = strip_at + s * layout.strip;
        let count = r.count(at, "strip index")?;
        let first = r.count(at + 4, "strip index offset")?;
        let flags = r.u8(at + 18)?;
        if first.checked_add(count).is_none_or(|end| end > index_count) {
            return Err(invalid(format!("triangles: a strip takes indices {first}..{} of {index_count}", first + count)));
        }
        if flags & STRIP_IS_TRIANGLE_STRIP != 0 {
            // each new index makes a triangle with the two before it, turning every other one
            for i in 2..count {
                let (a, b, c) = (mesh_vertex(first + i - 2)?, mesh_vertex(first + i - 1)?, mesh_vertex(first + i)?);
                if a == b || b == c || a == c {
                    continue;
                }
                triangles.extend(if i % 2 == 0 { [a, b, c] } else { [b, a, c] });
            }
        } else {
            for i in 0..count - count % 3 {
                triangles.push(mesh_vertex(first + i)?);
            }
        }
    }
    Ok(())
}
