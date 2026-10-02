//! What the server sends its clients of the entities, and what a client makes of it.
//!
//! A [`Snapshot`] is every entity a client has at one moment - the world, players, props,
//! weapons, scripted entities, but not what only the server keeps (`info_`, `logic_`,
//! `trigger_` ...) - with its networked variables (`SetNW*`, `SetNW2*`, the data tables of
//! `NetworkVar`). The client puts each at the index and serial number it has on the server, so
//! a handle means the same entity in both; an entity new to the client takes its class's
//! table (`scripted_ents.Get`, `weapons.Get`) and runs `Initialize` as on the server, one gone
//! from the server runs `OnRemove` and goes. Bodies of physics stay on the server.

use mlua::{AnyUserData, Lua, Table, Value};

use crate::entity::{self, Ent, EntityHandle};
use crate::host;
use crate::vector::{Angle, Vector};

/// A networked variable's value.
#[derive(Debug, Clone, PartialEq)]
pub enum NetValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    String(Vec<u8>),
    Entity(EntityHandle),
    Vector(Vector),
    Angle(Angle),
}

/// The tables of networked variables an entity keeps: `SetNW*`, `SetNW2*`, `SetDT*`.
const STORES: [&str; 3] = ["nw", "nw2", "dt"];

/// One entity as a client receives it.
#[derive(Debug, Clone)]
struct EntityState {
    index: u32,
    serial: u32,
    ent: Ent,
    /// The variables of each of [`STORES`].
    vars: [Vec<(Vec<u8>, NetValue)>; 3],
}

/// The entities of the server at one moment, for a client.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// The server's `CurTime`.
    pub time: f64,
    entities: Vec<EntityState>,
}

impl Snapshot {
    /// How many entities it has.
    pub fn len(&self) -> usize {
        self.entities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }
}

/// Classes only the server has: the engine sends clients nothing of them.
const SERVER_ONLY: &[&str] = &["info_", "logic_", "filter_", "ai_", "path_", "trigger_", "game_", "math_", "lua_run", "point_template"];

fn server_only(class: &str) -> bool {
    let class = class.to_ascii_lowercase();
    SERVER_ONLY.iter().any(|prefix| class.starts_with(prefix))
}

fn to_net(value: &Value) -> Option<NetValue> {
    Some(match value {
        Value::Boolean(b) => NetValue::Bool(*b),
        Value::Integer(i) => NetValue::Int(*i),
        Value::Number(n) => NetValue::Float(*n),
        Value::String(s) => NetValue::String(s.as_bytes().to_vec()),
        Value::UserData(ud) => {
            if let Ok(handle) = ud.borrow::<EntityHandle>() {
                NetValue::Entity(*handle)
            } else if let Ok(v) = ud.borrow::<Vector>() {
                NetValue::Vector(*v)
            } else if let Ok(a) = ud.borrow::<Angle>() {
                NetValue::Angle(*a)
            } else {
                return None;
            }
        }
        _ => return None,
    })
}

fn to_lua(lua: &Lua, value: &NetValue) -> mlua::Result<Value> {
    Ok(match value {
        NetValue::Bool(b) => Value::Boolean(*b),
        NetValue::Int(i) => Value::Integer(*i),
        NetValue::Float(n) => Value::Number(*n),
        NetValue::String(s) => Value::String(lua.create_string(s)?),
        NetValue::Entity(handle) => entity::push(lua, *handle)?,
        NetValue::Vector(v) => entity::vector(lua, *v)?,
        NetValue::Angle(a) => entity::angle(lua, *a)?,
    })
}

/// The variables of one store of an entity, in a stable order.
fn read_store(ud: &AnyUserData, store: &str) -> Vec<(Vec<u8>, NetValue)> {
    let Ok(Some(table)) = ud.named_user_value::<Option<Table>>(store) else { return Vec::new() };
    let mut vars: Vec<(Vec<u8>, NetValue)> = table
        .pairs::<Value, Value>()
        .flatten()
        .filter_map(|(key, value)| match key {
            Value::String(key) => Some((key.as_bytes().to_vec(), to_net(&value)?)),
            _ => None,
        })
        .collect();
    vars.sort_by(|a, b| a.0.cmp(&b.0));
    vars
}

impl crate::Runtime {
    /// What a client is sent of the entities now (on the server, after
    /// [`think`](Self::think)).
    pub fn snapshot(&self) -> Snapshot {
        let lua = &self.lua;
        let handles = host(lua).entities.handles();
        let mut entities = Vec::with_capacity(handles.len());
        for handle in handles {
            let Some(index) = handle.index else { continue };
            let ent = {
                let host = host(lua);
                let Some(ent) = host.entities.get(handle) else { continue };
                if ent.removed || (index != 0 && server_only(&ent.class)) {
                    continue;
                }
                let mut ent = ent.clone();
                ent.physics = None;
                ent.joint = None;
                ent
            };
            let vars = match entity::push(lua, handle) {
                Ok(Value::UserData(ud)) => STORES.map(|store| read_store(&ud, store)),
                _ => Default::default(),
            };
            entities.push(EntityState { index, serial: handle.serial, ent, vars });
        }
        Snapshot { time: self.cur_time(), entities }
    }

    /// A client takes what the server sent: entities gone from the server go (`OnRemove`,
    /// `EntityRemoved`), the others are as the server has them, and those new to the client
    /// come into being (`OnEntityCreated`, `Initialize`). `CurTime` is the server's.
    pub fn apply_snapshot(&self, snapshot: &Snapshot) {
        let lua = &self.lua;
        host(lua).clock.cur_time.set(snapshot.time);
        let wanted: std::collections::HashMap<u32, u32> = snapshot.entities.iter().map(|e| (e.index, e.serial)).collect();
        let gone: Vec<EntityHandle> = host(lua)
            .entities
            .handles()
            .into_iter()
            .filter(|h| h.index.is_some_and(|i| i != 0 && wanted.get(&i) != Some(&h.serial)))
            .collect();
        for handle in gone {
            crate::ents::remove(lua, handle);
        }
        crate::ents::collect(lua);

        let mut new = Vec::new();
        let mut handles = Vec::with_capacity(snapshot.entities.len());
        for state in &snapshot.entities {
            let handle = EntityHandle { index: Some(state.index), serial: state.serial, kind: state.ent.kind };
            let entities = &host(lua).entities;
            if entities.is_valid(handle) {
                entities.with_mut(handle, |ent| *ent = state.ent.clone());
            } else {
                entities.put(state.index, state.serial, state.ent.clone());
                new.push(handle);
            }
            handles.push(handle);
        }
        for &handle in &new {
            if let Err(e) = crate::ents::received(lua, handle) {
                host(lua).report(crate::ScriptError::from_lua(&e));
            }
        }
        for (state, handle) in snapshot.entities.iter().zip(handles) {
            if let Err(e) = write_vars(lua, handle, &state.vars) {
                host(lua).report(crate::ScriptError::from_lua(&e));
            }
        }
        for handle in new {
            crate::ents::arrived(lua, handle);
        }
    }
}

/// An entity's networked variables, as the server has them.
fn write_vars(lua: &Lua, handle: EntityHandle, vars: &[Vec<(Vec<u8>, NetValue)>; 3]) -> mlua::Result<()> {
    let Value::UserData(ud) = entity::push(lua, handle)? else { return Ok(()) };
    for (store, values) in STORES.iter().zip(vars) {
        if values.is_empty() && ud.named_user_value::<Option<Table>>(store)?.is_none() {
            continue;
        }
        let table = entity::store_of(lua, &ud, store)?;
        table.clear()?;
        for (key, value) in values {
            table.raw_set(lua.create_string(key)?, to_lua(lua, value)?)?;
        }
    }
    Ok(())
}
