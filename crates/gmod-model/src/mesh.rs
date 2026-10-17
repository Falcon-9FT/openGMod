//! A model as triangles to draw: the most detailed level of the models the body group
//! chooses, with the textures of a skin.

use std::io;

use crate::reader::invalid;
use crate::{Mdl, Vertex, Vhv, Vtx, Vvd};

/// Triangles that share a texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Batch {
    /// Index into [`Mdl::textures`].
    pub texture: usize,
    pub first_index: u32,
    pub index_count: u32,
}

/// A model's triangles, in model space.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StaticMesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub batches: Vec<Batch>,
    /// Linear light per vertex, when baked light was given and fits ([`Vhv`]).
    pub light: Option<Vec<[f32; 3]>>,
    /// The tangent of each vertex (x y z, and w: the side of the bitangent); none (all 0) when
    /// the `.vvd` has none.
    pub tangents: Vec<[f32; 4]>,
}

/// Builds the triangles of a model for body group number `body` and skin `skin`, lit by the
/// baked light of a static prop if given.
pub fn build(mdl: &Mdl, vvd: &Vvd, vtx: &Vtx, body: usize, skin: usize, baked: Option<&Vhv>) -> io::Result<StaticMesh> {
    if mdl.checksum != vvd.checksum || mdl.checksum != vtx.checksum {
        return Err(invalid(format!(
            "the model files do not belong together: checksums {:#x} (.mdl), {:#x} (.vvd), {:#x} (.vtx)",
            mdl.checksum, vvd.checksum, vtx.checksum
        )));
    }
    let mut out = StaticMesh::default();
    // the baked light per mesh of the most detailed level, by body part and model
    let lit = baked.and_then(|vhv| vhv.levels_of(vtx));
    let mut light = Vec::new();
    for (part, choice) in mdl.chosen_models(body) {
        let model = &mdl.body_parts[part].models[choice];
        let Some(level) = vtx.body_parts.get(part).and_then(|p| p.get(choice)).and_then(|levels| levels.first()) else { continue };
        if level.meshes.len() != model.meshes.len() {
            return Err(invalid(format!("model {:?}: {} meshes, but {} in the .vtx", model.name, model.meshes.len(), level.meshes.len())));
        }
        let mesh_light = lit.as_ref().and_then(|l| l.get(part)).and_then(|p| p.get(choice));
        for (k, (mesh, triangles)) in model.meshes.iter().zip(level.meshes.iter().map(|m| &m.triangles)).enumerate() {
            let first = model.first_vertex + mesh.first_vertex;
            let Some(vertices) = first.checked_add(mesh.vertex_count).and_then(|end| vvd.vertices.get(first..end)) else {
                return Err(invalid(format!("model {:?}: a mesh takes vertices past the {} there are", model.name, vvd.vertices.len())));
            };
            if let Some(&bad) = triangles.iter().find(|&&i| i as usize >= mesh.vertex_count) {
                return Err(invalid(format!("model {:?}: a triangle uses vertex {bad} of a mesh of {}", model.name, mesh.vertex_count)));
            }
            let base = out.vertices.len() as u32;
            out.vertices.extend_from_slice(vertices);
            match vvd.tangents.get(first..first + mesh.vertex_count) {
                Some(tangents) => out.tangents.extend_from_slice(tangents),
                None => out.tangents.resize(out.vertices.len(), [0.0; 4]),
            }
            // light comes per hardware vertex: give it to the mesh vertex behind each
            let mut colors = vec![[0.0f32; 3]; mesh.vertex_count];
            if let Some(baked) = mesh_light.and_then(|m| m.get(k)) {
                for (&vertex, &color) in level.meshes[k].vertices.iter().zip(baked) {
                    if let Some(slot) = colors.get_mut(vertex as usize) {
                        *slot = color;
                    }
                }
            }
            light.extend(colors);
            let first_index = out.indices.len() as u32;
            out.indices.extend(triangles.iter().map(|&i| base + i));
            out.batches.push(Batch { texture: mdl.texture_of(mesh.material, skin), first_index, index_count: triangles.len() as u32 });
        }
    }
    if lit.is_some() {
        out.light = Some(light);
    }
    Ok(out)
}
