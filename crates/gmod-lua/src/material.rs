//! `Material` and `IMaterial`: materials of the game as Lua reads and changes their
//! parameters. They are read with [`gmod_material`]; drawing them is the renderer's business.

use std::cell::RefCell;
use std::time::Instant;

use mlua::{AnyUserData, Lua, MultiValue, UserData, UserDataFields, UserDataMethods, Value};

use crate::args::Args;
use crate::host;
use crate::meta::{self, type_id};
use crate::vector::Vector;

/// A material: its name and shader, and its parameters, which Lua may change.
pub struct LuaMaterial {
    name: String,
    shader: String,
    /// Lower-case names, in file order.
    params: RefCell<Vec<(String, String)>>,
    error: bool,
}

impl LuaMaterial {
    fn get(&self, name: &str) -> Option<String> {
        self.params.borrow().iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone())
    }

    fn set(&self, name: &str, value: Option<String>) {
        let mut params = self.params.borrow_mut();
        let name = name.to_ascii_lowercase();
        params.retain(|(n, _)| *n != name);
        if let Some(value) = value {
            params.push((name, value));
        }
    }
}

impl UserData for LuaMaterial {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("MetaName", "IMaterial");
        fields.add_meta_field("MetaID", type_id::MATERIAL);
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function("__index", |lua, (_, key): (AnyUserData, Value)| meta::lookup(lua, "IMaterial", key));
        methods.add_meta_method("__tostring", |_, m, ()| Ok(format!("Material [{}]", m.name)));
    }
}

/// Loads a material by name: a `.vmt`, or an image (`.png`, `.jpg`) used as one.
fn load(lua: &Lua, name: &str) -> LuaMaterial {
    let vfs = host(lua).vfs.clone();
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".png") || lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        let found = [format!("materials/{lower}"), lower.clone()].iter().any(|p| vfs.exists(p, "GAME"));
        return LuaMaterial {
            name: name.to_string(),
            shader: "UnlitGeneric".into(),
            params: RefCell::new(vec![("$basetexture".into(), name.to_string())]),
            error: !found,
        };
    }
    match gmod_material::load_material(&vfs, name) {
        Ok(material) => LuaMaterial {
            name: name.to_string(),
            shader: material.shader.clone(),
            params: RefCell::new(material.params().to_vec()),
            error: false,
        },
        Err(_) => LuaMaterial { name: name.to_string(), shader: String::new(), params: RefCell::new(Vec::new()), error: true },
    }
}

fn def(lua: &Lua, table: &mlua::Table, name: &'static str, f: fn(&Args) -> mlua::Result<Value>) -> mlua::Result<()> {
    table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
}

fn with<R>(a: &Args, f: impl FnOnce(&LuaMaterial) -> R) -> mlua::Result<R> {
    match a.get(1) {
        Some(Value::UserData(ud)) => Ok(f(&*ud.borrow::<LuaMaterial>().map_err(|_| a.bad(1, "IMaterial"))?)),
        _ => Err(a.bad(1, "IMaterial")),
    }
}

fn text(a: &Args, value: Option<String>) -> mlua::Result<Value> {
    Ok(match value {
        Some(value) => Value::String(a.lua.create_string(value)?),
        None => Value::Nil,
    })
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    // `includes/util.lua` wraps this one and passes the flags as a string of digits
    lua.globals().raw_set(
        "Material",
        lua.create_function(|lua, values: MultiValue| {
            let started = Instant::now();
            let name = Args::new(lua, values, "Material").string(1)?;
            let material = lua.create_userdata(load(lua, &name))?;
            Ok((material, started.elapsed().as_secs_f64()))
        })?,
    )?;

    let m = meta::meta_table(lua, "IMaterial", type_id::MATERIAL)?;
    def(lua, &m, "GetName", |a| text(a, Some(with(a, |m| m.name.clone())?)))?;
    def(lua, &m, "GetShader", |a| text(a, Some(with(a, |m| m.shader.clone())?)))?;
    def(lua, &m, "IsError", |a| Ok(Value::Boolean(with(a, |m| m.error)?)))?;
    def(lua, &m, "GetString", |a| {
        let name = a.string(2)?;
        text(a, with(a, |m| m.get(&name))?)
    })?;
    def(lua, &m, "GetFloat", |a| {
        let name = a.string(2)?;
        let value = with(a, |m| m.get(&name))?;
        Ok(value.and_then(|v| v.trim().parse::<f64>().ok()).map_or(Value::Nil, Value::Number))
    })?;
    def(lua, &m, "GetInt", |a| {
        let name = a.string(2)?;
        let value = with(a, |m| m.get(&name))?;
        Ok(value.and_then(|v| v.trim().parse::<f64>().ok()).map_or(Value::Nil, |n| Value::Integer(n as i64)))
    })?;
    def(lua, &m, "GetVector", |a| {
        let name = a.string(2)?;
        let value = with(a, |m| m.get(&name))?;
        Ok(match value.and_then(|v| gmod_material::vmt::parse_vector(&v)) {
            Some([x, y, z]) => Value::UserData(a.lua.create_userdata(Vector::new(x, y, z))?),
            None => Value::Nil,
        })
    })?;
    def(lua, &m, "GetKeyValues", |a| {
        let params = with(a, |m| m.params.borrow().clone())?;
        let table = a.lua.create_table()?;
        for (name, value) in params {
            match value.trim().parse::<f64>() {
                Ok(n) => table.raw_set(name, n)?,
                Err(_) => table.raw_set(name, value)?,
            }
        }
        Ok(Value::Table(table))
    })?;
    def(lua, &m, "SetString", |a| {
        let (name, value) = (a.string(2)?, a.string(3)?);
        with(a, |m| m.set(&name, Some(value)))?;
        Ok(Value::Nil)
    })?;
    def(lua, &m, "SetFloat", |a| {
        let (name, value) = (a.string(2)?, a.number(3)?);
        with(a, |m| m.set(&name, Some(value.to_string())))?;
        Ok(Value::Nil)
    })?;
    def(lua, &m, "SetInt", |a| {
        let (name, value) = (a.string(2)?, a.number(3)? as i64);
        with(a, |m| m.set(&name, Some(value.to_string())))?;
        Ok(Value::Nil)
    })?;
    def(lua, &m, "SetVector", |a| {
        let (name, v) = (a.string(2)?, a.userdata::<Vector>(3, "Vector")?);
        with(a, |m| m.set(&name, Some(format!("[{} {} {}]", v.x, v.y, v.z))))?;
        Ok(Value::Nil)
    })?;
    def(lua, &m, "SetUndefined", |a| {
        let name = a.string(2)?;
        with(a, |m| m.set(&name, None))?;
        Ok(Value::Nil)
    })?;
    Ok(())
}
