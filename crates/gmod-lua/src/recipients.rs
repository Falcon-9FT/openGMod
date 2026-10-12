//! Who something the server sends goes to: `RecipientFilter` (`CRecipientFilter`), and the
//! players a `net` message names - a player, a table of them, or a filter.
//!
//! The server knows no sets of what is seen and heard from where yet: `AddPVS` and `AddPAS`
//! (and `net.SendPVS`, `net.SendPAS`) take every player.

use mlua::{AnyUserData, Lua, MultiValue, Table, UserData, UserDataFields, UserDataMethods, Value};

use crate::args::Args;
use crate::entity::{self, EntityHandle};
use crate::meta::{self, type_id};

const NAME: &str = "CRecipientFilter";

/// A `RecipientFilter`: players, each once, in the order they were added.
#[derive(Debug, Clone, Default)]
pub struct Recipients {
    players: Vec<EntityHandle>,
}

impl Recipients {
    fn add(&mut self, players: impl IntoIterator<Item = EntityHandle>) {
        for player in players {
            if !self.players.contains(&player) {
                self.players.push(player);
            }
        }
    }

    fn remove(&mut self, players: &[EntityHandle]) {
        self.players.retain(|p| !players.contains(p));
    }
}

impl UserData for Recipients {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("MetaName", NAME);
        fields.add_meta_field("MetaID", type_id::RECIPIENTFILTER);
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function("__index", |lua, (_, key): (AnyUserData, Value)| meta::lookup(lua, NAME, key));
    }
}

/// The players now in the game.
fn everyone(lua: &Lua) -> Vec<EntityHandle> {
    crate::player::all(lua)
}

/// The players a value names: a player, a table of players, or a filter. Whatever else is in
/// a table is passed over.
pub(crate) fn players_of(lua: &Lua, value: Option<&Value>) -> Vec<EntityHandle> {
    let is_player = |handle: &EntityHandle| crate::player::all(lua).contains(handle);
    match value {
        Some(Value::UserData(ud)) => {
            if let Ok(filter) = ud.borrow::<Recipients>() {
                return filter.players.iter().copied().filter(is_player).collect();
            }
            entity::handle_of(value).filter(is_player).into_iter().collect()
        }
        Some(Value::Table(table)) => {
            let mut players = Vec::new();
            for value in table.sequence_values::<Value>().flatten() {
                if let Some(handle) = entity::handle_of(Some(&value)).filter(is_player)
                    && !players.contains(&handle)
                {
                    players.push(handle);
                }
            }
            players
        }
        _ => Vec::new(),
    }
}

/// Every player but those a value names.
pub(crate) fn all_but(lua: &Lua, value: Option<&Value>) -> Vec<EntityHandle> {
    let omitted = players_of(lua, value);
    everyone(lua).into_iter().filter(|p| !omitted.contains(p)).collect()
}

/// Every player: what reaches as far as the server can tell from a place.
pub(crate) fn near(lua: &Lua, _position: Option<&Value>) -> Vec<EntityHandle> {
    everyone(lua)
}

fn def(lua: &Lua, t: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<Value>) -> mlua::Result<()> {
    t.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
}

fn change(a: &Args, f: impl FnOnce(&mut Recipients)) -> mlua::Result<Value> {
    a.change::<Recipients>(1, NAME, f)
}

fn players_by_team(lua: &Lua, team: i64) -> Vec<EntityHandle> {
    everyone(lua).into_iter().filter(|&p| crate::player::state(lua, p, |_, s| s.team) == Some(team)).collect()
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    // whether what goes to them may be lost (the argument) is the network's to decide
    lua.globals().raw_set("RecipientFilter", lua.create_function(|_, _: MultiValue| Ok(Recipients::default()))?)?;
    let t = meta::meta_table(lua, NAME, type_id::RECIPIENTFILTER)?;
    def(lua, &t, "AddAllPlayers", |a| {
        let all = everyone(a.lua);
        change(a, |r| r.add(all))
    })?;
    def(lua, &t, "AddPlayer", |a| {
        let players = players_of(a.lua, a.get(2));
        change(a, |r| r.add(players))
    })?;
    def(lua, &t, "AddPlayers", |a| {
        let players = players_of(a.lua, a.get(2));
        change(a, |r| r.add(players))
    })?;
    for name in ["AddPAS", "AddPVS"] {
        def(lua, &t, name, |a| {
            let players = near(a.lua, a.get(2));
            change(a, |r| r.add(players))
        })?;
    }
    def(lua, &t, "AddRecipientsByTeam", |a| {
        let players = players_by_team(a.lua, a.number(2)? as i64);
        change(a, |r| r.add(players))
    })?;
    def(lua, &t, "RemoveAllPlayers", |a| change(a, |r| r.players.clear()))?;
    for name in ["RemovePlayer", "RemovePlayers"] {
        def(lua, &t, name, |a| {
            let players = players_of(a.lua, a.get(2));
            change(a, |r| r.remove(&players))
        })?;
    }
    for name in ["RemovePAS", "RemovePVS"] {
        def(lua, &t, name, |a| {
            let players = near(a.lua, a.get(2));
            change(a, |r| r.remove(&players))
        })?;
    }
    def(lua, &t, "RemoveRecipientsByTeam", |a| {
        let players = players_by_team(a.lua, a.number(2)? as i64);
        change(a, |r| r.remove(&players))
    })?;
    def(lua, &t, "RemoveMismatchedPlayers", |a| {
        let keep = players_of(a.lua, a.get(2));
        change(a, |r| r.players.retain(|p| keep.contains(p)))
    })?;
    def(lua, &t, "GetCount", |a| {
        let players = players_of(a.lua, a.get(1));
        Ok(Value::Integer(players.len() as i64))
    })?;
    def(lua, &t, "GetPlayers", |a| {
        let players = players_of(a.lua, a.get(1));
        let table = a.lua.create_table()?;
        for player in players {
            table.raw_push(entity::push(a.lua, player)?)?;
        }
        Ok(Value::Table(table))
    })?;
    Ok(())
}
