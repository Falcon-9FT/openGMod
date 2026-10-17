//! `Entity:GetBrushSurfaces` and `SurfaceInfo`: the faces of the map's brush models - the
//! world's (`*0`) and those of its brush entities - with their materials and corners.

use std::sync::Arc;

use mlua::{AnyUserData, Lua, MultiValue, Table, UserData, UserDataFields, UserDataMethods, Value};

use crate::args::Args;
use crate::entity::{self, EntityHandle};
use crate::host;
use crate::meta::{self, type_id};
use crate::vector::Vector;

/// A face of a brush model.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BrushSurface {
    pub material: String,
    /// Its corners, in order.
    pub vertices: Vec<[f32; 3]>,
    /// Its `SURF_` flags.
    pub flags: i32,
}

/// The faces of each brush model of a map, `*0` (the world) first.
pub fn of_bsp(bsp: &gmod_bsp::Bsp) -> Arc<Vec<Vec<BrushSurface>>> {
    let models = bsp
        .models
        .iter()
        .map(|model| {
            let first = model.first_face.max(0) as usize;
            let faces = bsp.faces.get(first..first + model.num_faces.max(0) as usize).unwrap_or_default();
            faces
                .iter()
                .map(|face| BrushSurface {
                    material: bsp.face_material(face).unwrap_or_default().to_string(),
                    vertices: bsp.face_vertices(face).collect(),
                    flags: usize::try_from(face.texinfo).ok().and_then(|i| bsp.texinfo.get(i)).map_or(0, |t| t.flags),
                })
                .collect()
        })
        .collect();
    Arc::new(models)
}

const NAME: &str = "SurfaceInfo";

struct SurfaceInfo(BrushSurface);

impl UserData for SurfaceInfo {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("MetaName", NAME);
        fields.add_meta_field("MetaID", type_id::SURFACEINFO);
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function("__index", |lua, (ud, key): (AnyUserData, Value)| meta::index(lua, NAME, ud, key));
        methods.add_meta_function("__newindex", |lua, (ud, key, value): (AnyUserData, Value, Value)| {
            meta::new_index(lua, NAME, ud, key, value)
        });
        methods.add_meta_method("__tostring", |_, s, ()| Ok(format!("SurfaceInfo [{}]", s.0.material)));
    }
}

fn with<R>(a: &Args, f: impl FnOnce(&BrushSurface) -> R) -> mlua::Result<R> {
    match a.get(1) {
        Some(Value::UserData(ud)) => Ok(f(&ud.borrow::<SurfaceInfo>().map_err(|_| a.bad(1, NAME))?.0)),
        _ => Err(a.bad(1, NAME)),
    }
}

fn def(lua: &Lua, table: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<Value>) -> mlua::Result<()> {
    table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let ent = meta::meta_table(lua, "Entity", type_id::ENTITY)?;
    // the faces of the entity's brush model, as it is in the map; none for an entity of no brushes
    def(lua, &ent, "GetBrushSurfaces", |a| {
        let handle = entity::handle_of(a.get(1)).ok_or_else(|| a.bad(1, "Entity"))?;
        let model = if handle == EntityHandle::WORLD {
            Some(0)
        } else {
            host(a.lua).entities.get(handle).and_then(|e| e.model.strip_prefix('*').and_then(|n| n.parse::<usize>().ok()))
        };
        let surfaces = host(a.lua).world.surfaces.borrow().clone();
        let Some(faces) = model.and_then(|m| surfaces.get(m)) else { return Ok(Value::Nil) };
        let list = a.lua.create_table_with_capacity(faces.len(), 0)?;
        for face in faces {
            list.raw_push(a.lua.create_userdata(SurfaceInfo(face.clone()))?)?;
        }
        Ok(Value::Table(list))
    })?;

    let t = meta::meta_table(lua, NAME, type_id::SURFACEINFO)?;
    def(lua, &t, "GetMaterial", |a| {
        let name = with(a, |s| s.material.clone())?;
        crate::material::push_named(a.lua, &name)
    })?;
    def(lua, &t, "GetVertices", |a| {
        let corners = with(a, |s| s.vertices.clone())?;
        let list = a.lua.create_table_with_capacity(corners.len(), 0)?;
        for [x, y, z] in corners {
            list.raw_push(entity::vector(a.lua, Vector::new(x, y, z))?)?;
        }
        Ok(Value::Table(list))
    })?;
    def(lua, &t, "IsNoDraw", |a| Ok(Value::Boolean(with(a, |s| s.flags & gmod_bsp::surf::NODRAW != 0)?)))?;
    def(lua, &t, "IsSky", |a| Ok(Value::Boolean(with(a, |s| s.flags & (gmod_bsp::surf::SKY | gmod_bsp::surf::SKY2D) != 0)?)))?;
    def(lua, &t, "IsWater", |a| Ok(Value::Boolean(with(a, |s| s.flags & gmod_bsp::surf::WARP != 0)?)))?;
    Ok(())
}
