//! `gameevent`: the engine's events (`player_connect`, `player_hurt` ...) as hooks of the same
//! name, for the scripts that listen to them (`gameevent.Listen`). The server's that GMod's
//! clients hear too go to its players' clients ([`crate::player::ToClient::GameEvent`]).

use std::cell::RefCell;
use std::collections::HashSet;

use mlua::{Lua, MultiValue, Value};

use crate::entity::EntityHandle;
use crate::{Realm, host};

/// A value of an event's field.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Field {
    Int(i64),
    Float(f64),
    Bool(bool),
    Text(String),
}

/// The events Lua listens to.
#[derive(Default)]
pub(crate) struct Listening(RefCell<HashSet<String>>);

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let gameevent = lua.create_table()?;
    gameevent.raw_set(
        "Listen",
        lua.create_function(|lua, values: MultiValue| {
            let a = crate::args::Args::new(lua, values, "Listen");
            let name = a.string(1)?;
            host(lua).game_events.0.borrow_mut().insert(name.to_ascii_lowercase());
            Ok(())
        })?,
    )?;
    lua.globals().raw_set("gameevent", gameevent)
}

/// The events of the server its clients hear too, by the name they hear them by.
fn for_clients(name: &str) -> Option<&'static str> {
    Some(match name {
        "player_connect" => "player_connect_client",
        "player_disconnect" => "player_disconnect",
        "player_spawn" => "player_spawn",
        "player_activate" => "player_activate",
        "player_hurt" => "player_hurt",
        "player_say" => "player_say",
        "player_changename" => "player_changename",
        "player_info" => "player_info",
        _ => return None,
    })
}

/// An event happens: the hook of its name, with its fields, for Lua that listens; the server's
/// go to the clients that hear them.
pub(crate) fn fire(lua: &Lua, name: &str, fields: Vec<(&str, Field)>) {
    if host(lua).realm == Realm::Server
        && let Some(client_name) = for_clients(name)
    {
        // what the clients do not hear: where a player connects from
        let sent: Vec<(String, Field)> = fields.iter().filter(|(k, _)| *k != "address").map(|(k, v)| (k.to_string(), v.clone())).collect();
        for player in crate::player::all(lua) {
            crate::player::to_client(lua, player, crate::player::ToClient::GameEvent(client_name.to_string(), sent.clone()));
        }
    }
    run(lua, name, fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect());
}

/// An event the server's game made, as a client hears it.
pub(crate) fn heard(lua: &Lua, name: &str, fields: &[(String, Field)]) {
    run(lua, name, fields.to_vec());
}

fn run(lua: &Lua, name: &str, fields: Vec<(String, Field)>) {
    if !host(lua).game_events.0.borrow().contains(name) {
        return;
    }
    let made = (|| -> mlua::Result<Value> {
        let table = lua.create_table()?;
        for (key, value) in fields {
            match value {
                Field::Int(n) => table.raw_set(key, n)?,
                Field::Float(n) => table.raw_set(key, n)?,
                Field::Bool(b) => table.raw_set(key, b)?,
                Field::Text(s) => table.raw_set(key, s)?,
            }
        }
        Ok(Value::Table(table))
    })();
    if let Ok(table) = made {
        crate::call_hook(lua, name, MultiValue::from_iter([table]));
    }
}

/// A player's `UserID`, or 0 for what is no player.
pub(crate) fn user_id(lua: &Lua, handle: EntityHandle) -> i64 {
    crate::player::state(lua, handle, |_, p| p.user_id).unwrap_or(0)
}

/// An entity's index, or 0 for none.
pub(crate) fn index(handle: Option<EntityHandle>) -> i64 {
    handle.and_then(|h| h.index).map_or(0, i64::from)
}
