//! The menu's server browser: the `serverlist` library, its network side done by the program.
//!
//! What the menu asks - the servers of a list (`serverlist.Query`: `lan`, `favorite`,
//! `history`, `internet`), one server (`PingServer`), the players of one (`PlayerList`) - waits
//! for the program ([`crate::Runtime::take_server_queries`]), which asks the network and hands
//! the answers back ([`crate::Runtime::server_listed`] and the rest): the menu's callbacks then
//! run, with the arguments GMod gives them. The favourite servers are the program's to keep;
//! the menu adds and removes them (`AddServerToFavorites`...).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use mlua::{Function, Lua, MultiValue, Table, Value};

use crate::args::Args;
use crate::host;

/// A server as the menu lists it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServerEntry {
    /// `ip:port`.
    pub address: String,
    /// The round trip, in milliseconds.
    pub ping: u32,
    pub name: String,
    /// What it plays, as its gamemode calls itself.
    pub description: String,
    pub map: String,
    pub players: u32,
    pub max_players: u32,
    pub bots: u32,
    pub password: bool,
    /// The gamemode's folder.
    pub gamemode: String,
    /// The workshop item of the gamemode, if it is one.
    pub workshop_id: String,
    pub version: String,
}

/// A player of a server, as `PlayerList` lists it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServerPlayer {
    pub name: String,
    pub score: i32,
    /// How long they have played, in seconds.
    pub time: f32,
}

/// What the menu asks of the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerQuery {
    /// The servers of a list: `lan`, `favorite`, `history` or `internet`.
    List { id: u64, list: String },
    /// One server.
    Ping { id: u64, address: String },
    /// The players of one server.
    Players { id: u64, address: String },
}

/// What waits for an answer: a query's table of callbacks, or a callback.
enum Waiting {
    List(Table),
    Callback(Function),
}

/// The menu's questions and favourites.
#[derive(Default)]
pub(crate) struct Servers {
    queries: RefCell<Vec<ServerQuery>>,
    waiting: RefCell<HashMap<u64, Waiting>>,
    next: Cell<u64>,
    favorites: RefCell<Vec<String>>,
    favorites_changed: Cell<bool>,
    /// The server the game is on, if it is on one.
    current: RefCell<Option<String>>,
}

fn ask(lua: &Lua, waiting: Waiting, query: impl FnOnce(u64) -> ServerQuery) {
    let host = host(lua);
    let s = &host.menu.servers;
    let id = s.next.get() + 1;
    s.next.set(id);
    s.waiting.borrow_mut().insert(id, waiting);
    s.queries.borrow_mut().push(query(id));
}

/// The arguments of a found server's callback, in GMod's order: ping, name, description, map,
/// players, max players, bots, password, last played, address, gamemode, workshop item,
/// anonymous, network version, Lua version, location, gamemode category.
fn entry_args(lua: &Lua, e: &ServerEntry) -> mlua::Result<MultiValue> {
    let s = |text: &str| lua.create_string(text).map(Value::String);
    Ok(MultiValue::from_iter([
        Value::Integer(i64::from(e.ping)),
        s(&e.name)?,
        s(&e.description)?,
        s(&e.map)?,
        Value::Integer(i64::from(e.players)),
        Value::Integer(i64::from(e.max_players)),
        Value::Integer(i64::from(e.bots)),
        Value::Boolean(e.password),
        Value::Integer(0),
        s(&e.address)?,
        s(&e.gamemode)?,
        s(&e.workshop_id)?,
        Value::Boolean(false),
        s(&e.version)?,
        s(&e.version)?,
        s("")?,
        s("")?,
    ]))
}

/// Runs a callback; what it returned, or nothing when it failed (the error is reported).
fn call(lua: &Lua, f: Function, args: MultiValue) -> Option<MultiValue> {
    crate::files::call_reporting(lua, f, args).ok()
}

fn favorite(lua: &Lua, address: &str) -> bool {
    host(lua).menu.servers.favorites.borrow().iter().any(|f| f.eq_ignore_ascii_case(address))
}

fn set_favorite(lua: &Lua, address: &str, on: bool) {
    let host = host(lua);
    let s = &host.menu.servers;
    let mut favorites = s.favorites.borrow_mut();
    let had = favorites.iter().any(|f| f.eq_ignore_ascii_case(address));
    if on && !had {
        favorites.push(address.to_string());
    } else if !on && had {
        favorites.retain(|f| !f.eq_ignore_ascii_case(address));
    } else {
        return;
    }
    s.favorites_changed.set(true);
}

fn def(lua: &Lua, table: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<Value>) -> mlua::Result<()> {
    table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
}

pub(crate) fn install(lua: &Lua, serverlist: &Table) -> mlua::Result<()> {
    def(lua, serverlist, "Query", |a| {
        let data = a.table(1)?;
        let list = data.get::<Option<String>>("Type")?.unwrap_or_else(|| "internet".into()).to_ascii_lowercase();
        ask(a.lua, Waiting::List(data), |id| ServerQuery::List { id, list });
        Ok(Value::Nil)
    })?;
    def(lua, serverlist, "PingServer", |a| {
        let address = a.string(1)?;
        let callback = a.function(2)?;
        ask(a.lua, Waiting::Callback(callback), |id| ServerQuery::Ping { id, address });
        Ok(Value::Nil)
    })?;
    def(lua, serverlist, "PlayerList", |a| {
        let address = a.string(1)?;
        let callback = a.function(2)?;
        ask(a.lua, Waiting::Callback(callback), |id| ServerQuery::Players { id, address });
        Ok(Value::Nil)
    })?;
    def(lua, serverlist, "IsServerFavorite", |a| Ok(Value::Boolean(favorite(a.lua, &a.string(1)?))))?;
    def(lua, serverlist, "AddServerToFavorites", |a| {
        set_favorite(a.lua, &a.string(1)?, true);
        Ok(Value::Nil)
    })?;
    def(lua, serverlist, "RemoveServerFromFavorites", |a| {
        set_favorite(a.lua, &a.string(1)?, false);
        Ok(Value::Nil)
    })?;
    def(lua, serverlist, "IsCurrentServerFavorite", |a| {
        let current = host(a.lua).menu.servers.current.borrow().clone();
        Ok(Value::Boolean(current.is_some_and(|c| favorite(a.lua, &c))))
    })?;
    def(lua, serverlist, "AddCurrentServerToFavorites", |a| {
        let on = a.get(1).is_none_or(|v| !matches!(v, Value::Nil | Value::Boolean(false)));
        if let Some(current) = host(a.lua).menu.servers.current.borrow().clone() {
            set_favorite(a.lua, &current, on);
        }
        Ok(Value::Nil)
    })?;
    lua.globals().raw_set("CanAddServerToFavorites", lua.create_function(|lua, ()| Ok(host(lua).menu.servers.current.borrow().is_some()))?)
}

/// The program's side of the server browser.
impl crate::Runtime {
    /// What the menu asked of the network since the last call.
    pub fn take_server_queries(&self) -> Vec<ServerQuery> {
        std::mem::take(&mut *host(&self.lua).menu.servers.queries.borrow_mut())
    }

    /// A server of a list answered: the list's `Callback`. Whether the menu wants more.
    pub fn server_listed(&self, id: u64, entry: &ServerEntry) -> bool {
        let callback = match host(&self.lua).menu.servers.waiting.borrow().get(&id) {
            Some(Waiting::List(data)) => data.get::<Option<Function>>("Callback").ok().flatten(),
            _ => None,
        };
        let Some(callback) = callback else { return false };
        let Ok(args) = entry_args(&self.lua, entry) else { return false };
        let answer = call(&self.lua, callback, args);
        // the menu says to stop with a false
        !matches!(answer.as_ref().and_then(|r| r.front()), Some(Value::Boolean(false)))
    }

    /// A server of a list did not answer: the list's `CallbackFailed`. Whether the menu wants
    /// more.
    pub fn server_unanswered(&self, id: u64, address: &str) -> bool {
        let callback = match host(&self.lua).menu.servers.waiting.borrow().get(&id) {
            Some(Waiting::List(data)) => data.get::<Option<Function>>("CallbackFailed").ok().flatten(),
            _ => None,
        };
        let Some(callback) = callback else { return true };
        let Ok(address) = self.lua.create_string(address) else { return true };
        let answer = call(&self.lua, callback, MultiValue::from_iter([Value::String(address)]));
        !matches!(answer.as_ref().and_then(|r| r.front()), Some(Value::Boolean(false)))
    }

    /// A list is done: its `Finished`.
    pub fn servers_listed(&self, id: u64) {
        let waiting = host(&self.lua).menu.servers.waiting.borrow_mut().remove(&id);
        if let Some(Waiting::List(data)) = waiting
            && let Ok(Some(finished)) = data.get::<Option<Function>>("Finished")
        {
            call(&self.lua, finished, MultiValue::new());
        }
    }

    /// A server answered a ping, or did not (`None`: the callback gets nothing).
    pub fn server_pinged(&self, id: u64, entry: Option<&ServerEntry>) {
        let waiting = host(&self.lua).menu.servers.waiting.borrow_mut().remove(&id);
        let Some(Waiting::Callback(callback)) = waiting else { return };
        let args = entry.map_or_else(|| Ok(MultiValue::new()), |e| entry_args(&self.lua, e));
        if let Ok(args) = args {
            call(&self.lua, callback, args);
        }
    }

    /// The players of a server, or none when it did not answer: an empty table.
    pub fn server_players(&self, id: u64, players: &[ServerPlayer]) {
        let waiting = host(&self.lua).menu.servers.waiting.borrow_mut().remove(&id);
        let Some(Waiting::Callback(callback)) = waiting else { return };
        let list = || -> mlua::Result<Table> {
            let t = self.lua.create_table()?;
            for p in players {
                let row = self.lua.create_table()?;
                row.raw_set("name", p.name.as_str())?;
                row.raw_set("score", p.score)?;
                row.raw_set("time", p.time)?;
                t.raw_push(row)?;
            }
            Ok(t)
        };
        if let Ok(t) = list() {
            call(&self.lua, callback, MultiValue::from_iter([Value::Table(t)]));
        }
    }

    /// The favourite servers, as the program keeps them.
    pub fn set_favorite_servers(&self, favorites: Vec<String>) {
        *host(&self.lua).menu.servers.favorites.borrow_mut() = favorites;
    }

    /// The favourite servers, when the menu changed them since the last call.
    pub fn take_favorite_servers(&self) -> Option<Vec<String>> {
        let host = host(&self.lua);
        let s = &host.menu.servers;
        s.favorites_changed.replace(false).then(|| s.favorites.borrow().clone())
    }

    /// The server the game is on (`None`: none, or the player's own game).
    pub fn set_current_server(&self, address: Option<String>) {
        *host(&self.lua).menu.servers.current.borrow_mut() = address;
    }
}

/// A player as a server tells those who ask of it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScoreRow {
    pub name: String,
    pub frags: i64,
    /// How long they have played, in seconds.
    pub time: f64,
    pub bot: bool,
}

/// The server's side: what it says of itself.
impl crate::Runtime {
    /// The players: names, frags, how long they have played, whether bots.
    pub fn scoreboard(&self) -> Vec<ScoreRow> {
        let now = self.cur_time();
        crate::player::all(&self.lua)
            .into_iter()
            .filter_map(|p| {
                crate::player::state(&self.lua, p, |_, s| ScoreRow {
                    name: s.info.name.clone(),
                    frags: s.frags,
                    time: (now - s.joined).max(0.0),
                    bot: s.info.bot,
                })
            })
            .collect()
    }

    /// What the gamemode played calls itself (`Sandbox`); its folder's name when it says nothing.
    pub fn gamemode_title(&self) -> String {
        let name = host(&self.lua).game.gamemode_name.borrow().clone();
        crate::engine::gamemodes(&self.lua)
            .into_iter()
            .find(|(folder, _)| folder.eq_ignore_ascii_case(&name))
            .and_then(|(_, info)| info.str("title").map(str::to_string))
            .filter(|t| !t.is_empty())
            .unwrap_or(name)
    }
}
