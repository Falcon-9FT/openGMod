//! The libraries about the world - `ents`, `player`, `ai`, `motionsensor` - as they answer while
//! the world is empty, and the engine's service that is not there yet (`HTTP`).
//!
//! Entities come with stage 4: until then the server has no entities and no players, and
//! these functions say exactly that (`player.GetAll()` is `{}`, `Entity(1)` is `NULL`). What
//! would make something (`ents.Create`, `player.CreateNextBot`) is not defined, so that a
//! script that needs it fails where it asks.

use std::cell::RefCell;
use std::collections::BTreeMap;

use mlua::{Lua, MultiValue, Table, Value};

use crate::args::Args;
use crate::entity::EntityHandle;
use crate::host;

/// Schedule and task names of `ai.GetScheduleID` and `ai.GetTaskID`, numbered as they come.
#[derive(Default)]
pub(crate) struct AiNames {
    schedules: RefCell<BTreeMap<String, i64>>,
    tasks: RefCell<BTreeMap<String, i64>>,
}

fn def(lua: &Lua, table: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<Value>) -> mlua::Result<()> {
    table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
}

fn empty(a: &Args) -> mlua::Result<Value> {
    Ok(Value::Table(a.lua.create_table()?))
}

fn null(a: &Args) -> mlua::Result<Value> {
    Ok(Value::UserData(a.lua.create_userdata(EntityHandle::NULL)?))
}

fn zero(_: &Args) -> mlua::Result<Value> {
    Ok(Value::Integer(0))
}

fn no(_: &Args) -> mlua::Result<Value> {
    Ok(Value::Boolean(false))
}

fn nothing(_: &Args) -> mlua::Result<Value> {
    Ok(Value::Nil)
}

/// The world is entity 0; nothing else exists yet.
fn entity_at(a: &Args, position: usize) -> mlua::Result<Value> {
    let index = a.number(position)?;
    let handle = if index == 0.0 { EntityHandle { index: Some(0) } } else { EntityHandle::NULL };
    Ok(Value::UserData(a.lua.create_userdata(handle)?))
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    def(lua, &globals, "Entity", |a| entity_at(a, 1))?;
    def(lua, &globals, "Player", null)?;

    let ents = lua.create_table()?;
    for name in [
        "GetAll",
        "FindByClass",
        "FindByClassAndParent",
        "FindByModel",
        "FindByName",
        "FindInBox",
        "FindInCone",
        "FindInSphere",
        "FindAlongRay",
        "FindInPVS",
    ] {
        def(lua, &ents, name, empty)?;
    }
    def(lua, &ents, "GetCount", zero)?;
    def(lua, &ents, "GetEdictCount", zero)?;
    def(lua, &ents, "GetByIndex", |a| entity_at(a, 1))?;
    def(lua, &ents, "GetMapCreatedEntity", null)?;
    globals.raw_set("ents", ents)?;

    let player = lua.create_table()?;
    for name in ["GetAll", "GetHumans", "GetBots"] {
        def(lua, &player, name, empty)?;
    }
    def(lua, &player, "GetCount", zero)?;
    def(lua, &player, "GetByID", null)?;
    for name in ["GetBySteamID", "GetBySteamID64", "GetByAccountID", "GetByUniqueID"] {
        def(lua, &player, name, no)?;
    }
    globals.raw_set("player", player)?;

    let ai = lua.create_table()?;
    def(lua, &ai, "GetScheduleID", |a| {
        let name = a.string(1)?;
        Ok(Value::Integer(number(&host(a.lua).ai_names.schedules, name)))
    })?;
    def(lua, &ai, "GetTaskID", |a| {
        let name = a.string(1)?;
        Ok(Value::Integer(number(&host(a.lua).ai_names.tasks, name)))
    })?;
    def(lua, &ai, "GetNodeCount", zero)?;
    globals.raw_set("ai", ai)?;

    // Kinect: never there
    let motionsensor = lua.create_table()?;
    for name in ["IsAvailable", "IsActive", "Start"] {
        def(lua, &motionsensor, name, no)?;
    }
    for name in ["Stop", "GetSkeleton", "GetColourMaterial"] {
        def(lua, &motionsensor, name, nothing)?;
    }
    globals.raw_set("motionsensor", motionsensor)?;

    // no requests go out yet: HTTP says it could not make one
    def(lua, &globals, "HTTP", no)?;
    Ok(())
}

/// The ID of a name: the one it has, or the next.
fn number(names: &RefCell<BTreeMap<String, i64>>, name: String) -> i64 {
    let mut names = names.borrow_mut();
    let next = names.len() as i64;
    *names.entry(name).or_insert(next)
}
