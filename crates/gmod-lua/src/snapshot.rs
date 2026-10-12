//! What the server sends its clients of the entities, and what a client makes of it.
//!
//! A [`Snapshot`] is every entity a client has at one moment - the world, players, props,
//! weapons, scripted entities, but not what only the server keeps (`info_`, `logic_`,
//! `trigger_` ...) - with its networked variables (`SetNW*`, `SetNW2*`, the data tables of
//! `NetworkVar`). The client puts each at the index and serial number it has on the server, so
//! a handle means the same entity in both; an entity new to the client takes its class's
//! table (`scripted_ents.Get`, `weapons.Get`) and runs `Initialize` as on the server, one gone
//! from the server runs `OnRemove` and goes. Bodies of physics stay on the server, and with
//! them what only the server's engine keeps of an entity (the outputs of the map's entities,
//! the state of triggers, doors and NPCs); a ragdoll goes with where its parts are.
//!
//! A player is sent only what it may see ([`crate::pvs`]); what its client has and is out of
//! sight now the snapshot names as dormant: the client keeps it as it last was, not drawn and
//! not thinking (`IsDormant`), and `NotifyShouldTransmit` tells of it going and coming back.
//!
//! Every type here crosses the network with serde. Between two snapshots a client sees the
//! entities moving smoothly ([`Snapshot::between`]).

use mlua::{AnyUserData, Lua, MultiValue, Table, Value};

use crate::entity::{self, Ent, EntityHandle};
use crate::host;
use crate::vector::{Angle, Vector};

/// A networked variable's value.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EntityState {
    pub index: u32,
    pub serial: u32,
    pub ent: Ent,
    /// The variables of each store: `SetNW*`, `SetNW2*`, `SetDT*`.
    pub vars: [Vec<(Vec<u8>, NetValue)>; 3],
}

/// The entities of the server at one moment, for a client, in index order.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Snapshot {
    /// The server's `CurTime`.
    pub time: f64,
    pub entities: Vec<EntityState>,
    /// Entities - index and serial number - the server has but does not send now: the client
    /// keeps them dormant if it has them.
    #[serde(default)]
    pub dormant: Vec<(u32, u32)>,
}

impl Snapshot {
    /// How many entities it has.
    pub fn len(&self) -> usize {
        self.entities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    /// What a client sees `t` of the way (0 to 1) from `a` to `b`: the entities of `b`, those
    /// also in `a` moved and turned between where they were, the cycles of what they play, the
    /// eyes of players and the parts of ragdolls with them.
    pub fn between(a: &Snapshot, b: &Snapshot, t: f32) -> Snapshot {
        let t = t.clamp(0.0, 1.0);
        let mut out = b.clone();
        out.time = a.time + (b.time - a.time) * f64::from(t);
        let mut before = a.entities.iter().peekable();
        for state in &mut out.entities {
            while before.next_if(|e| e.index < state.index).is_some() {}
            let Some(old) = before.peek().filter(|e| e.index == state.index && e.serial == state.serial) else { continue };
            let (from, to) = (&old.ent, &mut state.ent);
            // what jumped (a teleport) is where it is now
            if (to.local_pos - from.local_pos).length() > TELEPORTED {
                continue;
            }
            to.local_pos = lerp(from.local_pos, to.local_pos, t);
            to.local_angles = lerp_angle(from.local_angles, to.local_angles, t);
            if from.anim.sequence == to.anim.sequence {
                // a cycle that went round goes on from the end
                let end = if to.anim.cycle < from.anim.cycle { to.anim.cycle + 1.0 } else { to.anim.cycle };
                to.anim.cycle = (from.anim.cycle + (end - from.anim.cycle) * t).fract();
            }
            if let (Some(from), Some(to)) = (&from.player, &mut to.player) {
                to.eye_angles = lerp_angle(from.eye_angles, to.eye_angles, t);
                to.render_angles = lerp_angle(from.render_angles, to.render_angles, t);
                to.moving.view_height = from.moving.view_height + (to.moving.view_height - from.moving.view_height) * t;
            }
            if from.ragdoll_pose.len() == to.ragdoll_pose.len() {
                for (old, new) in from.ragdoll_pose.iter().zip(&mut to.ragdoll_pose) {
                    if old.0 == new.0 {
                        new.1 = lerp(old.1, new.1, t);
                        new.2 = lerp_angle(old.2, new.2, t);
                    }
                }
            }
        }
        out
    }
}

/// How far an entity moves between two snapshots before it is taken to have jumped there.
const TELEPORTED: f32 = 256.0;

fn lerp(a: Vector, b: Vector, t: f32) -> Vector {
    a + (b - a) * t
}

/// Between two angles the short way round.
fn lerp_angle(a: Angle, b: Angle, t: f32) -> Angle {
    let turn = |from: f32, to: f32| from + ((to - from + 180.0).rem_euclid(360.0) - 180.0) * t;
    Angle::new(turn(a.p, b.p), turn(a.y, b.y), turn(a.r, b.r))
}

/// Classes only the server has: the engine sends clients nothing of them.
const SERVER_ONLY: &[&str] = &["info_", "logic_", "filter_", "ai_", "path_", "trigger_", "game_", "math_", "lua_run", "point_template"];

pub(crate) fn server_only(class: &str) -> bool {
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

/// An entity without what only the server's engine keeps of it.
fn for_clients(ent: Ent) -> Ent {
    Ent { physics: None, joint: None, outputs: Vec::new(), logic: None, trigger_state: None, mover: None, charger: None, npc: None, ..ent }
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
                for_clients(ent.clone())
            };
            let mut ent = Ent { ragdoll_pose: crate::ragdoll::pose(lua, handle), ..ent };
            if let Some(player) = &mut ent.player {
                player.physgun = crate::physgun::holding(lua, handle).unwrap_or(EntityHandle::NULL);
            }
            let vars = match entity::push(lua, handle) {
                Ok(Value::UserData(ud)) => STORES.map(|store| read_store(&ud, store)),
                _ => Default::default(),
            };
            entities.push(EntityState { index, serial: handle.serial, ent, vars });
        }
        Snapshot { time: self.cur_time(), entities, dormant: Vec::new() }
    }

    /// A client takes what the server sent: entities gone from the server go (`OnRemove`,
    /// `EntityRemoved`), those out of sight are kept dormant, the others are as the server has
    /// them, and those new to the client come into being (`OnEntityCreated`, `Initialize`).
    /// `CurTime` is the server's.
    pub fn apply_snapshot(&self, snapshot: &Snapshot) {
        let lua = &self.lua;
        host(lua).clock.cur_time.set(snapshot.time);
        let wanted: std::collections::HashMap<u32, u32> =
            snapshot.entities.iter().map(|e| (e.index, e.serial)).chain(snapshot.dormant.iter().copied()).collect();
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
        // out of sight: kept as they were
        let sleeping: std::collections::HashSet<(u32, u32)> = snapshot.dormant.iter().copied().collect();
        let fell_asleep: Vec<EntityHandle> = host(lua)
            .entities
            .handles()
            .into_iter()
            .filter(|h| h.index.is_some_and(|i| sleeping.contains(&(i, h.serial))))
            .filter(|&h| {
                let entities = &host(lua).entities;
                let awake = entities.get(h).is_some_and(|e| !e.dormant);
                entities.with_mut(h, |e| e.dormant = true);
                awake
            })
            .collect();

        let mut new = Vec::new();
        let mut woke = Vec::new();
        let mut handles = Vec::with_capacity(snapshot.entities.len());
        for state in &snapshot.entities {
            let handle = EntityHandle { index: Some(state.index), serial: state.serial, kind: state.ent.kind };
            let entities = &host(lua).entities;
            if entities.is_valid(handle) {
                if entities.get(handle).is_some_and(|e| e.dormant) {
                    woke.push(handle);
                }
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
        for (handle, sent) in fell_asleep.into_iter().map(|h| (h, false)).chain(woke.into_iter().map(|h| (h, true))) {
            if let Ok(value) = entity::push(lua, handle) {
                crate::call_hook(lua, "NotifyShouldTransmit", MultiValue::from_iter([value, Value::Boolean(sent)]));
            }
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
