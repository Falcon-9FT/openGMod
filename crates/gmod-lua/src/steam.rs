//! The main menu's `steamworks` through Steam itself, when the program signed in to it
//! ([`crate::Runtime::set_steam`]): the details of workshop items, the names of their authors,
//! subscribing. Without Steam the menu asks Steam's public web API instead ([`crate::menu`]).
//!
//! The menu asks item by item; what it asked in a frame goes to Steam as one request when the
//! program hands the runtime what Steam said ([`crate::Runtime::steam_events`], every frame).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gmod_steam::{Details, Event, Steam};
use mlua::{Function, Lua, Table, Value};

use crate::{files, host};

/// Steam, and what the menu asked of it that it has not answered yet.
#[derive(Default)]
pub(crate) struct SteamState {
    pub steam: RefCell<Option<Rc<Steam>>>,
    /// Items asked for this frame, not sent yet.
    wanted: RefCell<Vec<u64>>,
    /// The callbacks waiting for an item's details, by its ID.
    details: RefCell<HashMap<u64, Vec<Function>>>,
    /// The items of each request sent, by its number.
    requests: RefCell<HashMap<u64, Vec<u64>>>,
    /// The callbacks waiting for a player's name, by their SteamID.
    names: RefCell<HashMap<u64, Vec<Function>>>,
}

fn steam(lua: &Lua) -> Option<Rc<Steam>> {
    host(lua).menu.steam.steam.borrow().clone()
}

fn call(lua: &Lua, f: &Function, value: impl mlua::IntoLuaMulti) {
    if let Ok(args) = value.into_lua_multi(lua) {
        let _ = files::call_reporting(lua, f.clone(), args);
    }
}

/// A number the menu gives as a string or a number: a workshop ID, a SteamID.
fn id_of(value: &Value) -> Option<u64> {
    match value {
        Value::String(s) => s.to_str().ok()?.trim().parse().ok(),
        Value::Integer(n) => u64::try_from(*n).ok(),
        Value::Number(n) if *n >= 0.0 => Some(*n as u64),
        _ => None,
    }
}

/// The details as `steamworks.FileInfo` gives them (what is the menu's own - subscribed,
/// mounted - it adds).
fn info(lua: &Lua, item: &Details) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    if item.result != 1 {
        t.raw_set("error", item.result)?;
        return Ok(t);
    }
    t.raw_set("id", item.id.to_string())?;
    t.raw_set("title", item.title.as_str())?;
    t.raw_set("description", item.description.as_str())?;
    t.raw_set("owner", item.owner.to_string())?;
    t.raw_set("ownername", "")?;
    t.raw_set("previewid", item.preview_url.as_str())?;
    t.raw_set("previewurl", item.preview_url.as_str())?;
    t.raw_set("size", item.size)?;
    t.raw_set("created", item.created)?;
    t.raw_set("updated", item.updated)?;
    t.raw_set("tags", item.tags.as_str())?;
    t.raw_set("banned", item.banned)?;
    t.raw_set("up", item.votes_up)?;
    t.raw_set("down", item.votes_down)?;
    t.raw_set("score", item.score)?;
    Ok(t)
}

/// The functions the menu's Lua (`MENU_LUA`) asks Steam with: each says false when there is no
/// Steam, and the menu goes to the web instead.
pub(crate) fn natives(lua: &Lua) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    // FileInfo(id, callback): the item's details, later
    t.raw_set(
        "FileInfo",
        lua.create_function(|lua, (id, callback): (Value, Function)| {
            if steam(lua).is_none() {
                return Ok(false);
            }
            let Some(id) = id_of(&id) else { return Ok(false) };
            let state = &host(lua).menu.steam;
            let mut details = state.details.borrow_mut();
            let waiting = details.entry(id).or_default();
            if waiting.is_empty() {
                state.wanted.borrow_mut().push(id);
            }
            waiting.push(callback);
            Ok(true)
        })?,
    )?;
    // PlayerName(steamid, callback): the name, now or later
    t.raw_set(
        "PlayerName",
        lua.create_function(|lua, (id, callback): (Value, Function)| {
            let Some(steam) = steam(lua) else { return Ok(false) };
            let Some(id) = id_of(&id) else { return Ok(false) };
            match steam.player_name(id) {
                Some(name) => call(lua, &callback, name),
                None => host(lua).menu.steam.names.borrow_mut().entry(id).or_default().push(callback),
            }
            Ok(true)
        })?,
    )?;
    Ok(t)
}

/// `steamworks.GetPlayerName`, `Subscribe` and `Unsubscribe`, when there is Steam.
pub(crate) fn install(lua: &Lua, steamworks: &Table) -> mlua::Result<()> {
    steamworks.raw_set(
        "GetPlayerName",
        lua.create_function(|lua, id: Value| {
            Ok(match (steam(lua), id_of(&id)) {
                (Some(steam), Some(id)) => steam.known_name(id),
                _ => String::new(),
            })
        })?,
    )?;
    steamworks.raw_set(
        "Subscribe",
        lua.create_function(|lua, id: Value| {
            if let (Some(steam), Some(id)) = (steam(lua), id_of(&id)) {
                steam.subscribe(id);
            }
            Ok(())
        })?,
    )?;
    steamworks.raw_set(
        "Unsubscribe",
        lua.create_function(|lua, id: Value| {
            if let (Some(steam), Some(id)) = (steam(lua), id_of(&id)) {
                steam.unsubscribe(id);
            }
            Ok(())
        })?,
    )?;
    Ok(())
}

/// What Steam said this frame, to the callbacks waiting for it; then what was asked this frame
/// goes to Steam.
pub(crate) fn dispatch(lua: &Lua, events: &[Event]) {
    let Some(steam) = steam(lua) else { return };
    let state = &host(lua).menu.steam;
    for event in events {
        match event {
            Event::Details { request, items } => {
                let Some(ids) = state.requests.borrow_mut().remove(request) else { continue };
                let mut answers: Vec<(u64, Table)> = Vec::new();
                for &id in &ids {
                    let found = items.as_ref().ok().and_then(|items| items.iter().find(|item| item.id == id));
                    let table = match (found, items) {
                        (Some(item), _) => info(lua, item),
                        (None, Err(code)) => error(lua, *code),
                        // an item Steam left out of its answer
                        (None, Ok(_)) => error(lua, 9),
                    };
                    if let Ok(table) = table {
                        answers.push((id, table));
                    }
                }
                for (id, table) in answers {
                    let callbacks = state.details.borrow_mut().remove(&id).unwrap_or_default();
                    for callback in callbacks {
                        call(lua, &callback, table.clone());
                    }
                }
            }
            Event::Name(id) => {
                let callbacks = state.names.borrow_mut().remove(id).unwrap_or_default();
                let name = steam.known_name(*id);
                for callback in callbacks {
                    call(lua, &callback, name.as_str());
                }
            }
            _ => {}
        }
    }
    let wanted = std::mem::take(&mut *state.wanted.borrow_mut());
    // a request takes at most 50 items
    for ids in wanted.chunks(50) {
        match steam.request_details(ids) {
            Some(request) => {
                state.requests.borrow_mut().insert(request, ids.to_vec());
            }
            None => {
                for id in ids {
                    let callbacks = state.details.borrow_mut().remove(id).unwrap_or_default();
                    if let Ok(table) = error(lua, 2) {
                        for callback in callbacks {
                            call(lua, &callback, table.clone());
                        }
                    }
                }
            }
        }
    }
}

/// `{ error = code }`: how `steamworks.FileInfo` says it failed.
fn error(lua: &Lua, code: i32) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.raw_set("error", code)?;
    Ok(t)
}

impl crate::Runtime {
    /// Signs the menu's `steamworks` in to Steam: from now on it asks Steam itself.
    pub fn set_steam(&self, steam: Rc<Steam>) {
        *host(&self.lua).menu.steam.steam.borrow_mut() = Some(steam);
    }

    /// What Steam said this frame ([`Steam::run_frame`]): call it every frame, also when Steam
    /// said nothing, for what the menu asked to go to Steam.
    pub fn steam_events(&self, events: &[Event]) {
        dispatch(&self.lua, events);
    }
}
