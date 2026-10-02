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

/// A texture by name (`ITexture`): a file of the game, or one the engine renders into
/// (`_rt_FullFrameFB`), which is not there to read.
#[derive(Debug, Clone)]
pub struct LuaTexture {
    name: String,
}

impl UserData for LuaTexture {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("MetaName", "ITexture");
        fields.add_meta_field("MetaID", type_id::TEXTURE);
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function("__index", |lua, (_, key): (AnyUserData, Value)| meta::lookup(lua, "ITexture", key));
        methods.add_meta_method("__tostring", |_, t, ()| Ok(format!("ITexture [{}]", t.name)));
    }
}

/// A texture by name, for Lua.
pub(crate) fn push_texture(lua: &Lua, name: &str) -> mlua::Result<Value> {
    Ok(Value::UserData(lua.create_userdata(LuaTexture { name: name.to_string() })?))
}

/// The colour of a pixel of an image, as `Color`.
fn pixel(a: &Args, image: Option<std::rc::Rc<gmod_material::png::Rgba>>, x: f64, y: f64) -> mlua::Result<Value> {
    let color = image
        .and_then(|image| {
            let (x, y) = (x.max(0.0) as u32, y.max(0.0) as u32);
            if x >= image.width || y >= image.height {
                return None;
            }
            let i = (y as usize * image.width as usize + x as usize) * 4;
            image.pixels.get(i..i + 4).map(|p| [p[0], p[1], p[2], p[3]])
        })
        .unwrap_or([0, 0, 0, 255]);
    crate::surface::push_color(a.lua, color)
}

/// Loads a material by name: a `.vmt`, or an image (`.png`, `.jpg`) used as one.
fn load(lua: &Lua, name: &str) -> LuaMaterial {
    let vfs = host(lua).vfs.clone();
    if crate::surface::is_image_file(name) {
        let size = crate::surface::read_image_file(lua, name).map(|data| crate::surface::image_size(&data).unwrap_or((0, 0)));
        let mut params = vec![("$basetexture".to_string(), name.to_string())];
        if let Some((w, h)) = size {
            params.push(("$realwidth".into(), w.to_string()));
            params.push(("$realheight".into(), h.to_string()));
        }
        return LuaMaterial { name: name.to_string(), shader: "UnlitGeneric".into(), params: RefCell::new(params), error: size.is_none() };
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

/// A material's image for `surface`: its number on the screen and whether it adds light;
/// none for a material that is not there.
pub(crate) fn image_of(a: &Args, position: usize) -> mlua::Result<Option<(u32, bool)>> {
    let (name, additive, error) = match a.get(position) {
        Some(Value::UserData(ud)) => {
            let m = ud.borrow::<LuaMaterial>().map_err(|_| a.bad(position, "IMaterial"))?;
            (m.name.clone(), m.get("$additive").is_some_and(|v| v.trim() != "0"), m.error)
        }
        None => return Ok(None),
        _ => return Err(a.bad(position, "IMaterial")),
    };
    if error {
        return Ok(None);
    }
    Ok(Some((host(a.lua).screen.image_id(&name, additive), additive)))
}

/// A material by name, as `Material` makes it.
pub(crate) fn push_named(lua: &Lua, name: &str) -> mlua::Result<Value> {
    Ok(Value::UserData(lua.create_userdata(load(lua, name))?))
}

/// A material's size: its image's, or 0 when it has none.
fn size(a: &Args) -> mlua::Result<(u32, u32)> {
    let (name, error) = with(a, |m| (m.name.clone(), m.error))?;
    if error {
        return Ok((0, 0));
    }
    let id = host(a.lua).screen.image_id(&name, false);
    Ok(crate::surface::image_size_of(a.lua, id).unwrap_or((0, 0)))
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

    // the menu's: a material from an image file, whose image can be swapped
    // (`SetDynamicImage`); the flags are `Material`'s
    lua.globals().raw_set(
        "DynamicMaterial",
        lua.create_function(|lua, values: MultiValue| {
            let name = Args::new(lua, values, "DynamicMaterial").string(1)?;
            lua.create_userdata(load(lua, &name))
        })?,
    )?;

    let m = meta::meta_table(lua, "IMaterial", type_id::MATERIAL)?;
    def(lua, &m, "SetDynamicImage", |a| {
        let path = a.string(2)?;
        let Some(Value::UserData(ud)) = a.get(1) else { return Err(a.bad(1, "IMaterial")) };
        let fresh = load(a.lua, &path);
        let mut m = ud.borrow_mut::<LuaMaterial>().map_err(|_| a.bad(1, "IMaterial"))?;
        *m = fresh;
        Ok(Value::Nil)
    })?;
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
    def(lua, &m, "Width", |a| Ok(Value::Integer(i64::from(size(a)?.0))))?;
    def(lua, &m, "Height", |a| Ok(Value::Integer(i64::from(size(a)?.1))))?;
    def(lua, &m, "Recompute", |a| with(a, |_| Value::Nil))?;
    def(lua, &m, "SetUndefined", |a| {
        let name = a.string(2)?;
        with(a, |m| m.set(&name, None))?;
        Ok(Value::Nil)
    })?;
    def(lua, &m, "SetTexture", |a| {
        let name = a.string(2)?;
        let texture = match a.get(3) {
            Some(Value::UserData(ud)) => ud.borrow::<LuaTexture>().map_err(|_| a.bad(3, "ITexture"))?.name.clone(),
            _ => a.string(3)?,
        };
        with(a, |m| m.set(&name, Some(texture)))?;
        Ok(Value::Nil)
    })?;
    def(lua, &m, "GetTexture", |a| {
        let name = a.string(2)?;
        match with(a, |m| m.get(&name))? {
            Some(texture) => push_texture(a.lua, &texture),
            None => Ok(Value::Nil),
        }
    })?;
    def(lua, &m, "SetMatrix", |a| with(a, |_| Value::Nil))?;
    def(lua, &m, "GetMatrix", |a| Ok(Value::UserData(a.lua.create_userdata(crate::matrix::Matrix::IDENTITY)?)))?;
    def(lua, &m, "SetShader", |a| with(a, |_| Value::Nil))?;
    def(lua, &m, "GetColor", |a| {
        let (x, y) = (a.number(2)?, a.number(3)?);
        let name = with(a, |m| m.name.clone())?;
        pixel(a, crate::surface::pixels(a.lua, &name, false), x, y)
    })?;

    let t = meta::meta_table(lua, "ITexture", type_id::TEXTURE)?;
    def(lua, &t, "GetName", |a| {
        let name = texture_name(a)?;
        text(a, Some(name))
    })?;
    def(lua, &t, "IsError", |a| {
        let name = texture_name(a)?;
        Ok(Value::Boolean(!name.starts_with("_rt_") && crate::surface::pixels(a.lua, &name, true).is_none()))
    })?;
    def(lua, &t, "IsErrorTexture", |a| {
        let name = texture_name(a)?;
        Ok(Value::Boolean(!name.starts_with("_rt_") && crate::surface::pixels(a.lua, &name, true).is_none()))
    })?;
    for name in ["Width", "GetMappingWidth"] {
        def(lua, &t, name, |a| {
            let name = texture_name(a)?;
            Ok(Value::Integer(texture_size(a.lua, &name).0.into()))
        })?;
    }
    for name in ["Height", "GetMappingHeight"] {
        def(lua, &t, name, |a| {
            let name = texture_name(a)?;
            Ok(Value::Integer(texture_size(a.lua, &name).1.into()))
        })?;
    }
    def(lua, &t, "GetNumAnimationFrames", |_| Ok(Value::Integer(1)))?;
    def(lua, &t, "Download", |_| Ok(Value::Nil))?;
    def(lua, &t, "GetColor", |a| {
        let (x, y) = (a.number(2)?, a.number(3)?);
        let name = texture_name(a)?;
        pixel(a, crate::surface::pixels(a.lua, &name, true), x, y)
    })?;
    Ok(())
}

fn texture_name(a: &Args) -> mlua::Result<String> {
    match a.get(1) {
        Some(Value::UserData(ud)) => Ok(ud.borrow::<LuaTexture>().map_err(|_| a.bad(1, "ITexture"))?.name.clone()),
        _ => Err(a.bad(1, "ITexture")),
    }
}

/// A texture's size: the screen's for those the engine renders into.
fn texture_size(lua: &Lua, name: &str) -> (u32, u32) {
    if name.starts_with("_rt_") {
        return host(lua).screen.size.get();
    }
    crate::surface::pixels(lua, name, true).map_or((0, 0), |image| (image.width, image.height))
}
