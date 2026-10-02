//! The libraries about the world that still answer as if it were empty - `player`, `ai`,
//! `motionsensor`.
//!
//! Players come with the next steps of stage 4: until then there are none, and these
//! functions say exactly that (`player.GetAll()` is `{}`, `Player(1)` is `NULL`). What would
//! make one (`player.CreateNextBot`) is not defined, so that a script that needs it fails
//! where it asks. Entities are in [`crate::ents`].

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
    crate::entity::push(a.lua, EntityHandle::NULL)
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

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    def(lua, &globals, "Player", null)?;

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

    Ok(())
}

/// The ID of a name: the one it has, or the next.
fn number(names: &RefCell<BTreeMap<String, i64>>, name: String) -> i64 {
    let mut names = names.borrow_mut();
    let next = names.len() as i64;
    *names.entry(name).or_insert(next)
}
