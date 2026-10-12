//! Lag compensation: a player shoots at what they saw, which over the network is where things
//! were a while ago - the round trip and the client's drawing a little behind. The server keeps
//! where each player (and each entity Lua asks it to, `SetLagCompensated`) was at each tick
//! for a second; while a player's shot is compensated (`Player:LagCompensation(true)` around
//! `FireBullets`, as `weapon_base` does, and the engine's own weapons always), the others are
//! put back where that player saw them - with their angles, legs and what they played, which
//! place their hitboxes - and afterwards where they are.
//!
//! When a player saw the game is what their client says with its commands
//! ([`crate::Runtime::set_view_time`]); a player that has said nothing (the one at the server's
//! own screen) sees the game as it is, and nothing is moved for them.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use mlua::{Lua, Value};

use crate::anim::AnimState;
use crate::entity::{self, EntityHandle};
use crate::host;
use crate::vector::{Angle, Vector};

/// How far back the server goes: `sv_maxunlag`.
const MAX_UNLAG: f64 = 1.0;

/// Where an entity was at a tick.
#[derive(Debug, Clone)]
struct Record {
    time: f64,
    pos: Vector,
    angles: Angle,
    render_angles: Option<Angle>,
    anim: AnimState,
    mins: Vector,
    maxs: Vector,
}

/// What a shot moved back: each entity, with where it is.
type Moved = Vec<(EntityHandle, Record)>;

/// What the server keeps for lag compensation.
#[derive(Default)]
pub(crate) struct History {
    records: RefCell<HashMap<EntityHandle, VecDeque<Record>>>,
    /// The time of the game each player's client showed with its latest command.
    view_times: RefCell<HashMap<EntityHandle, f64>>,
    /// Entities other than players that are compensated.
    flagged: RefCell<Vec<EntityHandle>>,
    /// Where the entities moved back were, by the player whose shot moved them.
    moved: RefCell<Vec<(EntityHandle, Moved)>>,
}

fn record_of(lua: &Lua, handle: EntityHandle, time: f64) -> Option<Record> {
    let host = host(lua);
    let ent = host.entities.get(handle)?;
    Some(Record {
        time,
        pos: ent.local_pos,
        angles: ent.local_angles,
        render_angles: ent.player.as_ref().map(|p| p.render_angles),
        anim: ent.anim.clone(),
        mins: ent.mins,
        maxs: ent.maxs,
    })
}

fn put(lua: &Lua, handle: EntityHandle, r: &Record) {
    host(lua).entities.with_mut(handle, |ent| {
        ent.local_pos = r.pos;
        ent.local_angles = r.angles;
        ent.anim = r.anim.clone();
        (ent.mins, ent.maxs) = (r.mins, r.maxs);
        if let (Some(p), Some(angles)) = (ent.player.as_mut(), r.render_angles) {
            p.render_angles = angles;
        }
    });
}

/// What is compensated: the living players, and the entities flagged.
fn compensated(lua: &Lua) -> Vec<EntityHandle> {
    let mut all: Vec<EntityHandle> = crate::player::all(lua).into_iter().filter(|&p| crate::player::alive(lua, p)).collect();
    let host = host(lua);
    let mut flagged = host.lagcomp.flagged.borrow_mut();
    flagged.retain(|&h| host.entities.is_valid(h));
    all.extend(flagged.iter().copied());
    all
}

/// After a tick: where everything compensated is now, the oldest forgotten.
pub(crate) fn record(lua: &Lua) {
    let now = host(lua).clock.cur_time.get();
    let handles = compensated(lua);
    let records: Vec<(EntityHandle, Record)> = handles.into_iter().filter_map(|h| Some((h, record_of(lua, h, now)?))).collect();
    let host = host(lua);
    let mut history = host.lagcomp.records.borrow_mut();
    history.retain(|h, _| host.entities.is_valid(*h));
    for (handle, record) in records {
        let list = history.entry(handle).or_default();
        list.push_back(record);
        while list.front().is_some_and(|r| now - r.time > MAX_UNLAG + 0.1) {
            list.pop_front();
        }
    }
}

/// Where an entity was at `time`: between the ticks around it.
fn at(list: &VecDeque<Record>, time: f64) -> Option<Record> {
    let after = list.iter().position(|r| r.time >= time);
    match after {
        Some(0) => list.front().cloned(),
        Some(i) => {
            let (a, b) = (&list[i - 1], &list[i]);
            let t = if b.time > a.time { ((time - a.time) / (b.time - a.time)) as f32 } else { 1.0 };
            let lerp = |x: Vector, y: Vector| x + (y - x) * t;
            let turn = |x: Angle, y: Angle| {
                let one = |p: f32, q: f32| p + ((q - p + 180.0).rem_euclid(360.0) - 180.0) * t;
                Angle::new(one(x.p, y.p), one(x.y, y.y), one(x.r, y.r))
            };
            // a jump (a teleport, a respawn) is not drawn out
            let near = if t < 0.5 { a } else { b };
            if (b.pos - a.pos).length() > 64.0 {
                return Some(near.clone());
            }
            Some(Record {
                time,
                pos: lerp(a.pos, b.pos),
                angles: turn(a.angles, b.angles),
                render_angles: a.render_angles.zip(b.render_angles).map(|(x, y)| turn(x, y)),
                anim: near.anim.clone(),
                mins: near.mins,
                maxs: near.maxs,
            })
        }
        None => list.back().cloned(),
    }
}

/// A player's shot starts: everything else compensated goes back to where they saw it.
pub(crate) fn start(lua: &Lua, shooter: EntityHandle) {
    let Some(view) = host(lua).lagcomp.view_times.borrow().get(&shooter).copied() else { return };
    let now = host(lua).clock.cur_time.get();
    let time = view.clamp(now - MAX_UNLAG, now);
    if now - time < 1e-6 {
        return;
    }
    let mut moved = Vec::new();
    for handle in compensated(lua).into_iter().filter(|&h| h != shooter) {
        let past = host(lua).lagcomp.records.borrow().get(&handle).and_then(|list| at(list, time));
        let (Some(past), Some(present)) = (past, record_of(lua, handle, now)) else { continue };
        put(lua, handle, &past);
        moved.push((handle, present));
    }
    host(lua).lagcomp.moved.borrow_mut().push((shooter, moved));
}

/// The shot is done: what was moved back is where it is again.
pub(crate) fn finish(lua: &Lua, shooter: EntityHandle) {
    let moved = {
        let host = host(lua);
        let mut all = host.lagcomp.moved.borrow_mut();
        let Some(i) = all.iter().rposition(|(s, _)| *s == shooter) else { return };
        all.remove(i).1
    };
    for (handle, present) in moved {
        put(lua, handle, &present);
    }
}

/// Runs `f` with a player's shot compensated.
pub(crate) fn compensated_shot<R>(lua: &Lua, shooter: EntityHandle, f: impl FnOnce() -> R) -> R {
    start(lua, shooter);
    let out = f();
    finish(lua, shooter);
    out
}

/// The time of the game a player's client showed with its latest command.
pub(crate) fn set_view_time(lua: &Lua, player: EntityHandle, time: f64) {
    host(lua).lagcomp.view_times.borrow_mut().insert(player, time);
}

pub(crate) fn install_player(def: &entity::Def) -> mlua::Result<()> {
    def("LagCompensation", |a| {
        let player = crate::player::this(a)?;
        if a.boolean(2) {
            start(a.lua, player);
        } else {
            finish(a.lua, player);
        }
        Ok(Value::Nil)
    })?;
    Ok(())
}

pub(crate) fn install_methods(def: &entity::Def) -> mlua::Result<()> {
    def("SetLagCompensated", |a| {
        let handle = entity::this(a, 1)?;
        let on = a.boolean(2);
        let host = host(a.lua);
        let mut flagged = host.lagcomp.flagged.borrow_mut();
        flagged.retain(|&h| h != handle);
        if on {
            flagged.push(handle);
        }
        Ok(Value::Nil)
    })?;
    def("IsLagCompensated", |a| {
        let handle = entity::this(a, 1)?;
        let player = host(a.lua).entities.get(handle).is_some_and(|e| e.player.is_some());
        Ok(Value::Boolean(player || host(a.lua).lagcomp.flagged.borrow().contains(&handle)))
    })?;
    Ok(())
}
