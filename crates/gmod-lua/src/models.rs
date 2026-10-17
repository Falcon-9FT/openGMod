//! What `util` tells of a model's files: its triangles (`util.GetModelMeshes`), what it says
//! of itself (`util.GetModelInfo`), whether it is there (`util.IsModelLoaded`).

use mlua::{Lua, MultiValue, Table, Value};

use crate::args::Args;
use crate::entity;
use crate::host;
use crate::vector::Vector;

/// The material a texture of a model is: in the first of its folders that has it.
fn material_path(lua: &Lua, mdl: &gmod_model::Mdl, texture: usize) -> String {
    let Some(name) = mdl.textures.get(texture) else { return String::new() };
    let name = name.replace('\\', "/");
    let vfs = host(lua).vfs.clone();
    let candidates: Vec<String> = mdl.texture_folders.iter().map(|folder| format!("{}{name}", folder.replace('\\', "/"))).collect();
    candidates
        .iter()
        .find(|path| vfs.exists(&format!("materials/{path}.vmt"), "GAME"))
        .or(candidates.first())
        .cloned()
        .unwrap_or(name)
        .to_ascii_lowercase()
}

fn vertex(lua: &Lua, v: &gmod_model::Vertex) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    let [x, y, z] = v.position;
    t.raw_set("pos", entity::vector(lua, Vector::new(x, y, z))?)?;
    let [nx, ny, nz] = v.normal;
    t.raw_set("normal", entity::vector(lua, Vector::new(nx, ny, nz))?)?;
    t.raw_set("u", v.uv[0])?;
    t.raw_set("v", v.uv[1])?;
    let weights = lua.create_table()?;
    for i in 0..usize::from(v.bone_count.min(3)) {
        let w = lua.create_table()?;
        w.raw_set("bone", v.bones[i])?;
        w.raw_set("weight", v.weights[i])?;
        weights.raw_push(w)?;
    }
    t.raw_set("weights", weights)?;
    Ok(t)
}

fn def(lua: &Lua, table: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<Value>) -> mlua::Result<()> {
    table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let util: Table = lua.globals().get("util")?;
    // `util.GetModelMeshes(model, lod, bodygroupMask)`: each mesh's material and triangles
    def(lua, &util, "GetModelMeshes", |a| {
        let path = a.string(1)?.replace('\\', "/").to_ascii_lowercase();
        let body = a.opt_number(3)?.unwrap_or(0.0).max(0.0) as usize;
        let vfs = host(a.lua).vfs.clone();
        let Ok(model) = gmod_model::load(&vfs, &path) else { return Ok(Value::Nil) };
        let Ok(mesh) = model.mesh(body, 0) else { return Ok(Value::Nil) };
        let list = a.lua.create_table()?;
        for batch in &mesh.batches {
            let first = batch.first_index as usize;
            let indices = mesh.indices.get(first..first + batch.index_count as usize).unwrap_or_default();
            let triangles = a.lua.create_table_with_capacity(indices.len(), 0)?;
            for &i in indices {
                if let Some(v) = mesh.vertices.get(i as usize) {
                    triangles.raw_push(vertex(a.lua, v)?)?;
                }
            }
            let mut used: Vec<u32> = indices.to_vec();
            used.sort_unstable();
            used.dedup();
            let vertices = a.lua.create_table_with_capacity(used.len(), 0)?;
            for i in used {
                if let Some(v) = mesh.vertices.get(i as usize) {
                    vertices.raw_push(vertex(a.lua, v)?)?;
                }
            }
            let entry = a.lua.create_table()?;
            entry.raw_set("material", material_path(a.lua, &model.mdl, batch.texture))?;
            entry.raw_set("triangles", triangles)?;
            entry.raw_set("verticies", vertices)?;
            list.raw_push(entry)?;
        }
        Ok(Value::Table(list))
    })?;
    def(lua, &util, "GetModelInfo", |a| {
        let path = a.string(1)?.replace('\\', "/").to_ascii_lowercase();
        let vfs = host(a.lua).vfs.clone();
        let Ok(data) = vfs.read(&path, "GAME") else { return Ok(Value::Nil) };
        let Ok(mdl) = gmod_model::Mdl::parse(&data) else { return Ok(Value::Nil) };
        let info = a.lua.create_table()?;
        info.raw_set("SkinCount", mdl.skins.len().max(1))?;
        info.raw_set("SurfacePropName", mdl.surface_prop.clone())?;
        info.raw_set("KeyValues", mdl.key_values.clone())?;
        info.raw_set("ModelMass", mdl.mass)?;
        info.raw_set("BoneCount", mdl.bones.len())?;
        info.raw_set("HitBoxSetCount", mdl.hitbox_sets.len())?;
        Ok(Value::Table(info))
    })?;
    def(lua, &util, "IsModelLoaded", |a| {
        let path = a.string(1)?;
        Ok(Value::Boolean(host(a.lua).vfs.exists(&path, "GAME")))
    })?;
    Ok(())
}
