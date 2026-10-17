//! Named metatables and type IDs: `FindMetaTable`, `RegisterMetaTable`, `TYPE_*`.
//!
//! In GMod every type of the engine (`Vector`, `Entity`, `Player`...) has a metatable that Lua
//! can find by name and extend: `FindMetaTable("Vector").Foo = ...` gives every vector a
//! method `Foo`. Its `MetaName` and `MetaID` are how GMod's `type` and `TypeID` (written in Lua,
//! in `includes/util.lua`) tell the types apart.
//!
//! Values of the engine's types are userdata here, whose real metatables belong to mlua; the
//! table scripts see is the named one. `getmetatable(value)` returns it, and the `__index` of
//! the engine's types looks there for everything that is not a field.

use mlua::{AnyUserData, Function, Lua, Table, Value};

const REGISTRY: &str = "gmod-lua.metatables";

/// The type IDs of GMod's Lua (`TYPE_*`, what `TypeID` returns).
pub mod type_id {
    pub const NONE: i32 = -1;
    pub const NIL: i32 = 0;
    pub const BOOL: i32 = 1;
    pub const LIGHTUSERDATA: i32 = 2;
    pub const NUMBER: i32 = 3;
    pub const STRING: i32 = 4;
    pub const TABLE: i32 = 5;
    pub const FUNCTION: i32 = 6;
    pub const USERDATA: i32 = 7;
    pub const THREAD: i32 = 8;
    pub const ENTITY: i32 = 9;
    pub const VECTOR: i32 = 10;
    pub const ANGLE: i32 = 11;
    pub const PHYSOBJ: i32 = 12;
    pub const SAVE: i32 = 13;
    pub const RESTORE: i32 = 14;
    pub const DAMAGEINFO: i32 = 15;
    pub const EFFECTDATA: i32 = 16;
    pub const MOVEDATA: i32 = 17;
    pub const RECIPIENTFILTER: i32 = 18;
    pub const USERCMD: i32 = 19;
    pub const SCRIPTEDVEHICLE: i32 = 20;
    pub const MATERIAL: i32 = 21;
    pub const PANEL: i32 = 22;
    pub const PARTICLE: i32 = 23;
    pub const PARTICLEEMITTER: i32 = 24;
    pub const TEXTURE: i32 = 25;
    pub const USERMSG: i32 = 26;
    pub const CONVAR: i32 = 27;
    pub const IMESH: i32 = 28;
    pub const MATRIX: i32 = 29;
    pub const SOUND: i32 = 30;
    pub const PIXELVISHANDLE: i32 = 31;
    pub const DLIGHT: i32 = 32;
    pub const VIDEO: i32 = 33;
    pub const FILE: i32 = 34;
    pub const LOCOMOTION: i32 = 35;
    pub const PATH: i32 = 36;
    pub const NAVAREA: i32 = 37;
    pub const SOUNDHANDLE: i32 = 38;
    pub const NAVLADDER: i32 = 39;
    pub const PARTICLESYSTEM: i32 = 40;
    pub const PROJECTEDTEXTURE: i32 = 41;
    pub const PHYSCOLLIDE: i32 = 42;
    pub const SURFACEINFO: i32 = 43;
    pub const COUNT: i32 = 44;
    pub const COLOR: i32 = 255;
}

const TYPE_NAMES: &[(&str, i32)] = {
    use type_id::*;
    &[
        ("NONE", NONE),
        ("NIL", NIL),
        ("BOOL", BOOL),
        ("LIGHTUSERDATA", LIGHTUSERDATA),
        ("NUMBER", NUMBER),
        ("STRING", STRING),
        ("TABLE", TABLE),
        ("FUNCTION", FUNCTION),
        ("USERDATA", USERDATA),
        ("THREAD", THREAD),
        ("ENTITY", ENTITY),
        ("VECTOR", VECTOR),
        ("ANGLE", ANGLE),
        ("PHYSOBJ", PHYSOBJ),
        ("SAVE", SAVE),
        ("RESTORE", RESTORE),
        ("DAMAGEINFO", DAMAGEINFO),
        ("EFFECTDATA", EFFECTDATA),
        ("MOVEDATA", MOVEDATA),
        ("RECIPIENTFILTER", RECIPIENTFILTER),
        ("USERCMD", USERCMD),
        ("SCRIPTEDVEHICLE", SCRIPTEDVEHICLE),
        ("MATERIAL", MATERIAL),
        ("PANEL", PANEL),
        ("PARTICLE", PARTICLE),
        ("PARTICLEEMITTER", PARTICLEEMITTER),
        ("TEXTURE", TEXTURE),
        ("USERMSG", USERMSG),
        ("CONVAR", CONVAR),
        ("IMESH", IMESH),
        ("MATRIX", MATRIX),
        ("SOUND", SOUND),
        ("PIXELVISHANDLE", PIXELVISHANDLE),
        ("DLIGHT", DLIGHT),
        ("VIDEO", VIDEO),
        ("FILE", FILE),
        ("LOCOMOTION", LOCOMOTION),
        ("PATH", PATH),
        ("NAVAREA", NAVAREA),
        ("SOUNDHANDLE", SOUNDHANDLE),
        ("NAVLADDER", NAVLADDER),
        ("PARTICLESYSTEM", PARTICLESYSTEM),
        ("PROJECTEDTEXTURE", PROJECTEDTEXTURE),
        ("PHYSCOLLIDE", PHYSCOLLIDE),
        ("SURFACEINFO", SURFACEINFO),
        ("COUNT", COUNT),
        ("COLOR", COLOR),
    ]
};

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    lua.set_named_registry_value(REGISTRY, lua.create_table()?)?;
    for (name, id) in TYPE_NAMES {
        globals.raw_set(format!("TYPE_{name}"), *id)?;
    }

    globals.raw_set("FindMetaTable", lua.create_function(|lua, name: mlua::LuaString| registry(lua)?.raw_get::<Value>(name))?)?;
    globals.raw_set(
        "RegisterMetaTable",
        lua.create_function(|lua, (name, table): (mlua::LuaString, Table)| registry(lua)?.raw_set(name, table))?,
    )?;

    // the named table is the metatable of the engine's userdata, as far as Lua can tell
    let original: Function = globals.get("getmetatable")?;
    globals.raw_set(
        "getmetatable",
        lua.create_function(move |lua, value: Value| match &value {
            Value::UserData(ud) => match type_name(ud) {
                Some(name) => registry(lua)?.raw_get::<Value>(name),
                None => original.call::<Value>(value),
            },
            _ => original.call::<Value>(value),
        })?,
    )?;
    Ok(())
}

fn registry(lua: &Lua) -> mlua::Result<Table> {
    lua.named_registry_value(REGISTRY)
}

/// The `MetaName` of a userdata of the engine; an entity's says what kind it is (`Player`).
pub(crate) fn type_name(ud: &AnyUserData) -> Option<String> {
    if let Ok(handle) = ud.borrow::<crate::entity::EntityHandle>() {
        return Some(handle.kind.meta_name().to_string());
    }
    ud.metatable().ok()?.get::<Option<String>>("MetaName").ok().flatten()
}

/// A named metatable, if there is one.
pub(crate) fn named(lua: &Lua, name: &str) -> mlua::Result<Option<Table>> {
    registry(lua)?.raw_get::<Option<Table>>(name)
}

/// The named metatable of one of the engine's types, made if it is not there yet: `MetaName`,
/// `MetaID`, and `__index` to itself, as GMod's are.
pub(crate) fn meta_table(lua: &Lua, name: &str, id: i32) -> mlua::Result<Table> {
    let registry = registry(lua)?;
    if let Some(table) = registry.raw_get::<Option<Table>>(name)? {
        return Ok(table);
    }
    let table = lua.create_table()?;
    table.raw_set("MetaName", name)?;
    table.raw_set("MetaID", id)?;
    table.raw_set("__index", &table)?;
    registry.raw_set(name, &table)?;
    Ok(table)
}

/// `__index` of the engine's userdata past its fields: the named metatable, as scripts left it.
pub(crate) fn lookup(lua: &Lua, name: &str, key: Value) -> mlua::Result<Value> {
    match registry(lua)?.raw_get::<Option<Table>>(name)? {
        Some(table) => table.get(key),
        None => Ok(Value::Nil),
    }
}

/// `__index` of the engine's userdata: what the named metatable's own `__index` says - a
/// function a script put there (`meta.__index = function(self, key) ... end`), or a table -
/// as in GMod, where that table is the userdata's metatable.
pub(crate) fn index(lua: &Lua, name: &str, ud: AnyUserData, key: Value) -> mlua::Result<Value> {
    let Some(table) = registry(lua)?.raw_get::<Option<Table>>(name)? else { return Ok(Value::Nil) };
    match table.raw_get::<Value>("__index")? {
        Value::Function(f) => f.call((ud, key)),
        Value::Table(t) => t.get(key),
        _ => table.get(key),
    }
}

/// `__newindex` of the engine's userdata: the named metatable's own, if a script gave it one;
/// otherwise a value has no fields to set, as in Lua.
pub(crate) fn new_index(lua: &Lua, name: &str, ud: AnyUserData, key: Value, value: Value) -> mlua::Result<()> {
    let handler = match registry(lua)?.raw_get::<Option<Table>>(name)? {
        Some(table) => table.raw_get::<Value>("__newindex")?,
        None => Value::Nil,
    };
    match handler {
        Value::Function(f) => f.call((ud, key, value)),
        Value::Table(t) => t.set(key, value),
        _ => Err(crate::args::error(lua, format!("attempt to index a {name} value"))),
    }
}
