//! What moves in a map: doors, buttons, platforms - the engine's brush movers and the doors
//! that are models - and the brushes that may move, vanish or go.
//!
//! A brush entity that may move ([`gmod_bsp::Entity::moves`]: the movers, and any `func_`
//! brush with a name or a parent) is not part of the world baked into the map: it is drawn
//! where its entity is (`ents::drawn`), and every tick what is solid follows it - its
//! brushes for traces and players ([`gmod_bsp::Collision::place_model`]) and a body that
//! pushes props ([`gmod_physics::Physics::add_mover`]). A removed one, or a `func_brush`
//! turned off (`Disable`, `Toggle`), is neither seen nor solid.
//!
//! The movers go between a closed and an open placement at their `speed`:
//!
//! * `func_door` slides along `movedir` by its size that way less `lip`; `func_door_rotating`
//!   turns `distance` degrees about its origin - its yaw, or its roll with flag 64, its pitch
//!   with 128, the other way with 2 - away from whoever opens it unless flag 16 makes it
//!   one-way. `Open`, `Close`, `Toggle`, `Lock`, `Unlock`; the use key opens it with flag 256,
//!   touching it with 1024; it closes by itself `wait` seconds after it opened, unless `wait`
//!   is -1 or it toggles (flag 32). `spawnpos 1` (or flag 1) starts it open; flag 2048 locked.
//!   `OnOpen`, `OnClose`, `OnFullyOpen`, `OnFullyClosed`, `OnLockedUse`; `noise1` sounds as it
//!   goes, `noise2` when it stops.
//! * `prop_door_rotating`, a model, turns its yaw by `distance` (90) at `speed` away from who
//!   opens it (`opendir` 1 and 2 hold it to one way), and back after `returndelay` unless that
//!   is -1; the use key opens it, and closes it when open; `OpenAwayFrom`.
//! * `func_button`: `Press` and the use key (flag 1024, set on most) or touching it (256) fire
//!   `OnPressed` and push it in along `movedir` (unless flag 1 holds it still), `OnIn`; it comes
//!   back after `wait` (`OnOut`), or with the next press when it toggles (32). `Lock`, `Unlock`
//!   (`OnUseLocked`), `PressIn`, `PressOut`.
//! * `func_movelinear` slides along `movedir` by `movedistance`: `Open`, `Close`,
//!   `SetPosition` (0 to 1), `SetSpeed`; `OnFullyOpen`, `OnFullyClosed`.
//! * `func_rotating` spins at `maxspeed` about its yaw (its roll with flag 4, its pitch with 8,
//!   backwards with 2): `Start`, `Stop`, `Toggle`, `Reverse`, `StartForward`, `StartBackward`,
//!   `SetSpeed` (a share of `maxspeed`); flag 1 starts it.
//!
//! Not done: what blocks a mover (it goes through players), players carried by what they
//! stand on, `func_tracktrain` and `func_train`, the spin-up of `func_rotating`.

use std::cell::RefCell;

use gmod_physics::BodyId;
use mlua::Lua;

use crate::entity::{Ent, EntityHandle};
use crate::host;
use crate::vector::{Angle, Vector};

/// What a mover keeps.
#[derive(Debug, Clone, PartialEq)]
pub struct Mover {
    class: Class,
    /// Its placement, relative to its parent: closed and open.
    closed: (Vector, Angle),
    open: (Vector, Angle),
    /// How far it is, 0 closed to 1 open, and where it goes.
    at: f32,
    target: f32,
    /// Of the way, each second.
    rate: f32,
    locked: bool,
    /// When it goes back by itself.
    back_at: f64,
    /// `func_rotating`: degrees a second now, and its full speed.
    spin: f32,
    max_spin: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Class {
    Door,
    RotatingDoor,
    PropDoor,
    Button,
    Linear,
    Rotating,
}

/// How a brush that may move stands in what is solid now.
#[derive(Debug, Clone, PartialEq)]
struct Synced {
    model: Option<usize>,
    at: Option<(Vector, Angle)>,
    body: Option<BodyId>,
}

/// The brushes that may move and the model doors, as what is solid has them.
#[derive(Default)]
pub(crate) struct Moving {
    synced: RefCell<Vec<(EntityHandle, Synced)>>,
}

const SOLID_NONE: i64 = 0;

/// The brush model of an entity that may move, by the rule the map's drawing follows.
pub(crate) fn moving_brush(ent: &Ent) -> Option<usize> {
    if !ent.model.starts_with('*') {
        return None;
    }
    let mut pairs = vec![("classname".to_string(), ent.class.clone()), ("model".to_string(), ent.model.clone())];
    pairs.push(("targetname".to_string(), ent.name.clone()));
    if let Some(parent) = ent.keyvalue("parentname") {
        pairs.push(("parentname".to_string(), parent.to_string()));
    }
    let entity = gmod_bsp::Entity { pairs };
    if entity.moves() { entity.brush_model() } else { None }
}

fn number(lua: &Lua, handle: EntityHandle, key: &str, default: f32) -> f32 {
    host(lua).entities.get(handle).and_then(|ent| ent.keyvalue(key).map(|v| crate::convar::atof(v) as f32)).unwrap_or(default)
}

fn text(lua: &Lua, handle: EntityHandle, key: &str) -> String {
    host(lua).entities.get(handle).and_then(|ent| ent.keyvalue(key).map(str::to_string)).unwrap_or_default()
}

fn flags(lua: &Lua, handle: EntityHandle) -> i64 {
    host(lua).entities.get(handle).map_or(0, |ent| ent.spawn_flags)
}

fn class_of(lua: &Lua, handle: EntityHandle) -> String {
    host(lua).entities.get(handle).map(|ent| ent.class.to_ascii_lowercase()).unwrap_or_default()
}

fn mover(lua: &Lua, handle: EntityHandle) -> Option<Mover> {
    host(lua).entities.get(handle).and_then(|ent| ent.mover.as_deref().cloned())
}

fn set_mover(lua: &Lua, handle: EntityHandle, m: Mover) {
    host(lua).entities.with_mut(handle, |ent| ent.mover = Some(Box::new(m)));
}

/// A direction from `movedir`'s pitch and yaw.
fn move_direction(lua: &Lua, handle: EntityHandle) -> Vector {
    let [p, y, r] = crate::ents::three(&text(lua, handle, "movedir"));
    Angle::new(p, y, r).vectors().0
}

/// How far a brush slides along a direction: its size that way, less `lip`.
fn travel(lua: &Lua, handle: EntityHandle, direction: Vector) -> f32 {
    let (mins, maxs) = host(lua).entities.get(handle).map(|ent| (ent.mins, ent.maxs)).unwrap_or_default();
    let size = maxs - mins;
    let along = direction.x.abs() * size.x + direction.y.abs() * size.y + direction.z.abs() * size.z;
    (along - number(lua, handle, "lip", 0.0)).max(0.0)
}

fn add_axis(angles: Angle, axis: usize, degrees: f32) -> Angle {
    match axis {
        0 => Angle::new(angles.p, angles.y, angles.r + degrees),
        1 => Angle::new(angles.p + degrees, angles.y, angles.r),
        _ => Angle::new(angles.p, angles.y + degrees, angles.r),
    }
}

/// A spawned entity of the engine takes up its part, if it moves - or starts off, for a brush
/// that may be turned on (`func_brush` with `StartDisabled`, `func_wall_toggle` with flag 1).
pub(crate) fn spawned(lua: &Lua, handle: EntityHandle) {
    let starts_off = match class_of(lua, handle).as_str() {
        "func_brush" => number(lua, handle, "StartDisabled", 0.0) != 0.0,
        "func_wall_toggle" => flags(lua, handle) & 1 != 0,
        _ => false,
    };
    if starts_off {
        host(lua).entities.with_mut(handle, |ent| {
            ent.no_draw = true;
            ent.solid = SOLID_NONE;
        });
    }
    let class = match class_of(lua, handle).as_str() {
        "func_door" => Class::Door,
        "func_door_rotating" => Class::RotatingDoor,
        "prop_door_rotating" => Class::PropDoor,
        "func_button" => Class::Button,
        "func_movelinear" => Class::Linear,
        "func_rotating" => Class::Rotating,
        _ => return,
    };
    let Some(closed) = host(lua).entities.get(handle).map(|ent| (ent.local_pos, ent.local_angles)) else { return };
    let flags = flags(lua, handle);
    let speed = number(lua, handle, "speed", 100.0).abs();
    let (open, distance) = match class {
        Class::Door | Class::Button => {
            let direction = move_direction(lua, handle);
            let far = if class == Class::Button && flags & 1 != 0 { 0.0 } else { travel(lua, handle, direction) };
            ((closed.0 + direction * far, closed.1), far)
        }
        Class::Linear => {
            let far = number(lua, handle, "movedistance", 100.0);
            ((closed.0 + move_direction(lua, handle) * far, closed.1), far.abs())
        }
        Class::RotatingDoor => {
            let axis = if flags & 64 != 0 {
                0
            } else if flags & 128 != 0 {
                1
            } else {
                2
            };
            let degrees = number(lua, handle, "distance", 90.0) * if flags & 2 != 0 { -1.0 } else { 1.0 };
            ((closed.0, add_axis(closed.1, axis, degrees)), degrees.abs())
        }
        Class::PropDoor => {
            let degrees = number(lua, handle, "distance", 90.0);
            ((closed.0, add_axis(closed.1, 2, degrees)), degrees.abs())
        }
        Class::Rotating => ((closed.0, closed.1), 0.0),
    };
    let rate = if distance > 0.0 { speed / distance } else { f32::INFINITY };
    let starts_open = match class {
        Class::Door | Class::RotatingDoor => number(lua, handle, "spawnpos", 0.0) == 1.0 || flags & 1 != 0,
        Class::PropDoor => number(lua, handle, "spawnpos", 0.0) != 0.0,
        Class::Linear => number(lua, handle, "startposition", 0.0) >= 1.0,
        _ => false,
    };
    if class == Class::PropDoor {
        // solid by its box, so that the use key finds it
        host(lua).entities.with_mut(handle, |ent| ent.solid = 6);
    }
    let start =
        if class == Class::Linear { number(lua, handle, "startposition", 0.0).clamp(0.0, 1.0) } else { f32::from(u8::from(starts_open)) };
    let max_spin = number(lua, handle, "maxspeed", 100.0) * if flags & 2 != 0 { -1.0 } else { 1.0 };
    let m = Mover {
        class,
        closed,
        open,
        at: start,
        target: start,
        rate,
        locked: flags & 2048 != 0,
        back_at: f64::INFINITY,
        spin: if class == Class::Rotating && flags & 1 != 0 { max_spin } else { 0.0 },
        max_spin,
    };
    place(lua, handle, &m);
    set_mover(lua, handle, m);
}

/// Puts a mover where it is along its way.
fn place(lua: &Lua, handle: EntityHandle, m: &Mover) {
    if m.class == Class::Rotating {
        return;
    }
    let t = m.at;
    let pos = m.closed.0 + (m.open.0 - m.closed.0) * t;
    let lerp = |a: f32, b: f32| a + (b - a) * t;
    let angles = Angle::new(lerp(m.closed.1.p, m.open.1.p), lerp(m.closed.1.y, m.open.1.y), lerp(m.closed.1.r, m.open.1.r));
    host(lua).entities.with_mut(handle, |ent| {
        ent.local_pos = pos;
        ent.local_angles = angles;
    });
}

/// Which way a door that turns about its yaw opens away from `activator`: +1 or -1.
fn away_from(lua: &Lua, handle: EntityHandle, activator: EntityHandle, m: &Mover) -> f32 {
    let entities = &host(lua).entities;
    let (Some((origin, angles)), Some((who, _))) = (entities.transform(handle), entities.transform(activator)) else { return 1.0 };
    let (mins, maxs) = entities.get(handle).map(|ent| (ent.mins, ent.maxs)).unwrap_or_default();
    // the middle of the door, and which way it goes as the door starts to turn the positive way
    let middle = origin + ((mins + maxs) * 0.5).rotated(angles);
    let arm = middle - origin;
    let opening_side = (m.open.1.y - m.closed.1.y).signum();
    let going = Vector::new(-arm.y, arm.x, 0.0) * opening_side;
    if going.dot(who - middle) > 0.0 { -1.0 } else { 1.0 }
}

/// Starts a mover towards open (1) or closed (0), firing what it fires as it starts.
fn go(lua: &Lua, handle: EntityHandle, mut m: Mover, target: f32, activator: EntityHandle, caller: EntityHandle) {
    if m.target == target && (m.at == target || m.class == Class::Linear) {
        set_mover(lua, handle, m);
        return;
    }
    let opening = target > m.at;
    m.target = target;
    m.back_at = f64::INFINITY;
    set_mover(lua, handle, m.clone());
    let output = match (m.class, opening) {
        (Class::Door | Class::RotatingDoor | Class::PropDoor, true) => Some("OnOpen"),
        (Class::Door | Class::RotatingDoor | Class::PropDoor, false) => Some("OnClose"),
        _ => None,
    };
    if let Some(output) = output {
        crate::outputs::fire(lua, handle, output, activator, caller, None);
    }
    let sound = text(lua, handle, "noise1");
    if !sound.is_empty() {
        crate::sound::emit_scaled(lua, &sound, handle, 1.0);
    }
}

/// Opens a door: one that turns, away from `activator` unless it opens one way only.
fn open_door(lua: &Lua, handle: EntityHandle, mut m: Mover, activator: EntityHandle, caller: EntityHandle) {
    if m.at == 0.0 && m.target == 0.0 {
        let free = match m.class {
            Class::RotatingDoor => flags(lua, handle) & 16 == 0 && m.closed.1.p == m.open.1.p && m.closed.1.r == m.open.1.r,
            Class::PropDoor => number(lua, handle, "opendir", 0.0) == 0.0,
            _ => false,
        };
        let way = match m.class {
            Class::PropDoor if number(lua, handle, "opendir", 0.0) == 2.0 => -1.0,
            _ if free && activator.index.is_some() => away_from(lua, handle, activator, &m),
            _ => 1.0,
        };
        let turn = (m.open.1.y - m.closed.1.y).abs() * way;
        if m.class != Class::Door && turn != 0.0 {
            m.open.1 = Angle::new(m.open.1.p, m.closed.1.y + turn, m.open.1.r);
        }
    }
    go(lua, handle, m, 1.0, activator, caller);
}

fn press(lua: &Lua, handle: EntityHandle, m: Mover, activator: EntityHandle, caller: EntityHandle) {
    if m.locked {
        crate::outputs::fire(lua, handle, "OnUseLocked", activator, caller, None);
        return;
    }
    let toggles = flags(lua, handle) & 32 != 0;
    let pressed = m.target == 1.0;
    if pressed && !toggles {
        return;
    }
    crate::outputs::fire(lua, handle, "OnPressed", activator, caller, None);
    let target = if toggles && pressed { 0.0 } else { 1.0 };
    go(lua, handle, m, target, activator, caller);
}

/// An input to a mover, or to a brush that may be turned off. Whether it took it.
pub(crate) fn input(lua: &Lua, handle: EntityHandle, input: &str, param: &str, activator: EntityHandle, caller: EntityHandle) -> bool {
    let lower = input.to_ascii_lowercase();
    let class = class_of(lua, handle);
    if matches!(class.as_str(), "func_brush" | "func_wall_toggle") {
        let on = match lower.as_str() {
            "enable" | "turnon" => true,
            "disable" | "turnoff" => false,
            "toggle" => host(lua).entities.get(handle).is_some_and(|ent| ent.no_draw),
            _ => return false,
        };
        host(lua).entities.with_mut(handle, |ent| {
            ent.no_draw = !on;
            ent.solid = if on { 6 } else { SOLID_NONE };
        });
        return true;
    }
    let Some(mut m) = mover(lua, handle) else { return false };
    let value = crate::convar::atof(param) as f32;
    match (m.class, lower.as_str()) {
        (_, "lock") => m.locked = true,
        (_, "unlock") => m.locked = false,
        (Class::Door | Class::RotatingDoor | Class::PropDoor, "open") => {
            open_door(lua, handle, m, activator, caller);
            return true;
        }
        (Class::PropDoor, "openawayfrom") => {
            let from = crate::ents::named(lua, param.trim()).unwrap_or(activator);
            open_door(lua, handle, m, from, caller);
            return true;
        }
        (Class::Door | Class::RotatingDoor | Class::PropDoor | Class::Linear, "close") => {
            go(lua, handle, m, 0.0, activator, caller);
            return true;
        }
        (Class::Door | Class::RotatingDoor | Class::PropDoor, "toggle") => {
            if m.target > 0.0 {
                go(lua, handle, m, 0.0, activator, caller);
            } else {
                open_door(lua, handle, m, activator, caller);
            }
            return true;
        }
        (Class::Door | Class::RotatingDoor | Class::PropDoor, "use") => {
            if m.locked {
                crate::outputs::fire(lua, handle, "OnLockedUse", activator, caller, None);
                return true;
            }
            let flags = flags(lua, handle);
            let usable = m.class == Class::PropDoor || flags & 256 != 0;
            if !usable {
                return false;
            }
            if m.target == 0.0 {
                open_door(lua, handle, m, activator, caller);
            } else if m.class == Class::PropDoor || flags & 32 != 0 || number(lua, handle, "wait", 4.0) < 0.0 {
                go(lua, handle, m, 0.0, activator, caller);
            }
            return true;
        }
        (Class::Button, "press") => {
            press(lua, handle, m, activator, caller);
            return true;
        }
        (Class::Button, "use") => {
            if flags(lua, handle) & 1024 == 0 {
                return false;
            }
            press(lua, handle, m, activator, caller);
            return true;
        }
        (Class::Button, "pressin") => {
            go(lua, handle, m, 1.0, activator, caller);
            return true;
        }
        (Class::Button, "pressout") => {
            go(lua, handle, m, 0.0, activator, caller);
            return true;
        }
        (Class::Linear, "open") => {
            go(lua, handle, m, 1.0, activator, caller);
            return true;
        }
        (Class::Linear, "setposition") => {
            go(lua, handle, m, value.clamp(0.0, 1.0), activator, caller);
            return true;
        }
        (Class::Door | Class::RotatingDoor | Class::PropDoor | Class::Linear | Class::Button, "setspeed") => {
            let distance = match m.class {
                Class::Door | Class::Button | Class::Linear => (m.open.0 - m.closed.0).length(),
                _ => (m.open.1.y - m.closed.1.y).abs().max((m.open.1.p - m.closed.1.p).abs()).max((m.open.1.r - m.closed.1.r).abs()),
            };
            m.rate = if distance > 0.0 { value.abs() / distance } else { f32::INFINITY };
        }
        (Class::Rotating, "start" | "startforward") => m.spin = m.max_spin.abs() * if lower == "start" { m.max_spin.signum() } else { 1.0 },
        (Class::Rotating, "startbackward") => m.spin = -m.max_spin.abs(),
        (Class::Rotating, "stop") => m.spin = 0.0,
        (Class::Rotating, "toggle") => m.spin = if m.spin == 0.0 { m.max_spin } else { 0.0 },
        (Class::Rotating, "reverse") => {
            m.max_spin = -m.max_spin;
            m.spin = -m.spin;
        }
        (Class::Rotating, "setspeed") => m.spin = m.max_spin * value.clamp(0.0, 1.0),
        _ => return false,
    }
    set_mover(lua, handle, m);
    true
}

/// One tick: the movers go on their ways, then what is solid follows what may move.
pub(crate) fn tick(lua: &Lua) {
    let now = host(lua).clock.cur_time.get();
    let dt = crate::timer::TICK_INTERVAL as f32;
    let moving: Vec<EntityHandle> = {
        let entities = &host(lua).entities;
        entities.handles().into_iter().filter(|&h| entities.get(h).is_some_and(|e| e.mover.is_some() && !e.removed && e.spawned)).collect()
    };
    let before: Vec<(EntityHandle, Option<(Vector, Angle)>)> = moving.iter().map(|&h| (h, host(lua).entities.transform(h))).collect();
    for handle in moving {
        let Some(mut m) = mover(lua, handle) else { continue };
        if m.class == Class::Rotating {
            if m.spin != 0.0 {
                let axis = match flags(lua, handle) {
                    f if f & 4 != 0 => 0,
                    f if f & 8 != 0 => 1,
                    _ => 2,
                };
                host(lua).entities.with_mut(handle, |ent| {
                    let a = add_axis(ent.local_angles, axis, m.spin * dt);
                    ent.local_angles = Angle::new(a.p.rem_euclid(360.0), a.y.rem_euclid(360.0), a.r.rem_euclid(360.0));
                });
            }
            continue;
        }
        if m.at != m.target {
            let step = m.rate * dt;
            m.at = if m.target > m.at { (m.at + step).min(m.target) } else { (m.at - step).max(m.target) };
            place(lua, handle, &m);
            set_mover(lua, handle, m.clone());
            if m.at == m.target {
                arrived(lua, handle, m, now);
            }
        } else if m.back_at <= now {
            m.back_at = f64::INFINITY;
            go(lua, handle, m, 0.0, EntityHandle::NULL, handle);
        }
    }
    touch_open(lua);
    sync(lua);
    for (handle, was) in before {
        let now = host(lua).entities.transform(handle);
        if let (Some(was), Some(now)) = (was, now)
            && was != now
        {
            push_players(lua, handle, was, now);
        }
    }
}

/// Whether a player's box is inside a mover: its brushes where it stands now, or the box of a
/// model door.
fn inside(lua: &Lua, mover: EntityHandle, player: EntityHandle) -> bool {
    let entities = &host(lua).entities;
    let (Some((_, _, lo, hi)), Some((pos, angles))) = (entities.world_bounds(player), entities.transform(mover)) else { return false };
    let model = entities.get(mover).and_then(|ent| ent.model.strip_prefix('*').and_then(|m| m.parse::<usize>().ok()));
    match model {
        Some(model) => {
            let Some(collision) = host(lua).world.collision.borrow().clone() else { return false };
            let placement = gmod_bsp::collision::Placement::new(model, None, array(pos), [angles.p, angles.y, angles.r]);
            // a little smaller than the player: touching is not being inside
            let (center, half) = ((lo + hi) * 0.5, (hi - lo) * 0.5 - Vector::new(0.1, 0.1, 0.1));
            let c = array(center);
            collision.trace_placed(&placement, c, c, array(half * -1.0), array(half), gmod_bsp::mask::ALL).start_solid
        }
        None => entities
            .world_bounds(mover)
            .is_some_and(|(_, _, mlo, mhi)| lo.x < mhi.x && hi.x > mlo.x && lo.y < mhi.y && hi.y > mlo.y && lo.z < mhi.z && hi.z > mlo.z),
    }
}

/// A mover that moved into players pushes them along as far as it moved where they are. One
/// that cannot push a player out of its way is blocked: it hurts them by its `dmg`, fires
/// `OnBlockedOpening` or `OnBlockedClosing`, and a door goes back the other way.
fn push_players(lua: &Lua, handle: EntityHandle, was: (Vector, Angle), now: (Vector, Angle)) {
    for player in crate::player::all(lua) {
        if !crate::player::alive(lua, player) || !inside(lua, handle, player) {
            continue;
        }
        let Some((_, _, lo, hi)) = host(lua).entities.world_bounds(player) else { continue };
        let Some((feet, _)) = host(lua).entities.transform(player) else { continue };
        // where the middle of the player was on the mover, now
        let middle = (lo + hi) * 0.5;
        let on = crate::entity::unrotated(middle - was.0, was.1);
        let delta = now.0 + on.rotated(now.1) - middle;
        let length = delta.length();
        let to = if length > 0.0 { feet + delta * ((length + 0.5) / length) } else { feet };
        let (mins, maxs) = (lo - feet, hi - feet);
        let sweep = crate::trace::Sweep {
            mins,
            maxs,
            mask: gmod_bsp::mask::PLAYERSOLID,
            group: crate::trace::group::PLAYER_MOVEMENT,
            ..crate::trace::Sweep::line(feet, to, crate::trace::Filter::Entities(vec![player, handle]))
        };
        let hit = crate::trace::trace(lua, &sweep);
        if !hit.start_solid {
            crate::entity::set_world_pos(lua, player, hit.end);
            // pushed by what they stand on: not carried along by it again next tick
            crate::player::with_player(lua, player, |_, p| {
                if p.ground == handle {
                    p.ground_at = Some(now);
                }
            });
        }
        if hit.start_solid || hit.fraction < 1.0 || inside(lua, handle, player) {
            blocked(lua, handle, player);
        }
    }
}

/// A mover stopped by a player it cannot push away.
fn blocked(lua: &Lua, handle: EntityHandle, player: EntityHandle) {
    let damage = number(lua, handle, "dmg", 0.0);
    if damage > 0.0 {
        crate::damage::take(lua, player, crate::damage::DamageInfo::new(damage, 1, handle, handle));
    }
    let Some(m) = mover(lua, handle) else { return };
    let opening = m.target > m.at;
    let output = if opening { "OnBlockedOpening" } else { "OnBlockedClosing" };
    crate::outputs::fire(lua, handle, output, player, handle, None);
    // a door goes back unless it is to close whatever is in the way (`forceclosed`)
    let force = number(lua, handle, "forceclosed", 0.0) != 0.0;
    if matches!(m.class, Class::Door | Class::RotatingDoor | Class::PropDoor) && !(force && !opening) && m.at != m.target {
        let back = if opening { 0.0 } else { 1.0 };
        go(lua, handle, m, back, player, handle);
    }
}

/// A mover got where it was going.
fn arrived(lua: &Lua, handle: EntityHandle, mut m: Mover, now: f64) {
    let open = m.at >= 1.0;
    let output = match (m.class, open) {
        (Class::Button, true) => "OnIn",
        (Class::Button, false) => "OnOut",
        (_, true) => "OnFullyOpen",
        (_, false) => "OnFullyClosed",
    };
    let flags = flags(lua, handle);
    let wait = match m.class {
        Class::PropDoor => number(lua, handle, "returndelay", -1.0),
        Class::Button => number(lua, handle, "wait", 3.0),
        Class::Door | Class::RotatingDoor => number(lua, handle, "wait", 4.0),
        _ => -1.0,
    };
    let toggles = matches!(m.class, Class::Door | Class::RotatingDoor | Class::Button) && flags & 32 != 0;
    if open && wait >= 0.0 && !toggles && m.target == 1.0 {
        m.back_at = now + f64::from(wait);
    }
    set_mover(lua, handle, m.clone());
    crate::outputs::fire(lua, handle, output, EntityHandle::NULL, handle, None);
    let sound = text(lua, handle, "noise2");
    if !sound.is_empty() {
        crate::sound::emit_scaled(lua, &sound, handle, 1.0);
    }
}

/// Doors and buttons that open when touched (flag 1024 for a door, 256 for a button): a
/// player against one opens or presses it.
fn touch_open(lua: &Lua) {
    let Some(collision) = host(lua).world.collision.borrow().clone() else { return };
    let touchy: Vec<(EntityHandle, usize, Class)> = {
        let entities = &host(lua).entities;
        entities
            .handles()
            .into_iter()
            .filter_map(|h| {
                let ent = entities.get(h)?;
                let m = ent.mover.as_deref()?;
                let flag = match m.class {
                    Class::Door | Class::RotatingDoor => 1024,
                    Class::Button => 256,
                    _ => return None,
                };
                let model = ent.model.strip_prefix('*')?.parse().ok()?;
                (ent.spawn_flags & flag != 0 && m.target == 0.0 && m.at == 0.0).then_some((h, model, m.class))
            })
            .collect()
    };
    if touchy.is_empty() {
        return;
    }
    for player in crate::player::all(lua) {
        if !crate::player::alive(lua, player) {
            continue;
        }
        let Some((_, _, lo, hi)) = host(lua).entities.world_bounds(player) else { continue };
        // a little bigger than the player: against the door is touching it
        let (center, half) = ((lo + hi) * 0.5, (hi - lo) * 0.5 + Vector::new(1.0, 1.0, 1.0));
        for &(door, model, class) in &touchy {
            let Some((pos, angles)) = host(lua).entities.transform(door) else { continue };
            let placement = gmod_bsp::collision::Placement::new(model, None, array(pos), [angles.p, angles.y, angles.r]);
            let c = array(center);
            let trace = collision.trace_placed(&placement, c, c, array(half * -1.0), array(half), gmod_bsp::mask::ALL);
            if !trace.start_solid {
                continue;
            }
            let Some(m) = mover(lua, door) else { continue };
            if class == Class::Button {
                press(lua, door, m, player, player);
            } else if !m.locked {
                open_door(lua, door, m, player, player);
            }
        }
    }
}

fn array(v: Vector) -> [f32; 3] {
    [v.x, v.y, v.z]
}

/// What is solid follows what may move: the brushes of those that moved, turned off or went,
/// and the bodies of model doors.
fn sync(lua: &Lua) {
    let found: Vec<(EntityHandle, Option<usize>, bool)> = {
        let entities = &host(lua).entities;
        entities
            .handles()
            .into_iter()
            .filter_map(|h| {
                let ent = entities.get(h)?;
                let brush = moving_brush(&ent);
                let prop_door = ent.mover.as_deref().is_some_and(|m| m.class == Class::PropDoor);
                (brush.is_some() || prop_door)
                    .then(|| (h, brush, ent.removed || !ent.spawned || ent.solid == SOLID_NONE && brush.is_none()))
            })
            .collect()
    };
    let mut seen = Vec::new();
    for (handle, brush, gone) in found {
        seen.push(handle);
        // a brush turned off is neither seen nor solid
        let off = gone || host(lua).entities.get(handle).is_some_and(|ent| brush.is_some() && ent.no_draw && ent.solid == SOLID_NONE);
        let now = if off { None } else { host(lua).entities.transform(handle) };
        let before = host(lua).movers.synced.borrow().iter().find(|(h, _)| *h == handle).map(|(_, s)| s.clone());
        if before.as_ref().is_some_and(|b| b.at == now) {
            continue;
        }
        let mut synced = before.unwrap_or(Synced { model: brush, at: None, body: None });
        if let Some(model) = brush {
            let host = host(lua);
            let mut collision = host.world.collision.borrow_mut();
            if let Some(c) = collision.as_mut() {
                std::sync::Arc::make_mut(c).place_model(model, now.map(|(p, a)| (array(p), [a.p, a.y, a.r])));
            }
        }
        match (now, synced.body) {
            (None, Some(body)) => {
                crate::physics::remove_body(lua, body);
                synced.body = None;
            }
            (Some((pos, angles)), Some(body)) => {
                host(lua).physics.physics.borrow_mut().move_mover(body, array(pos), [angles.p, angles.y, angles.r]);
            }
            (Some((pos, angles)), None) if synced.at.is_none() => synced.body = add_body(lua, handle, brush, pos, angles),
            _ => {}
        }
        synced.at = now;
        let host = host(lua);
        let mut all = host.movers.synced.borrow_mut();
        match all.iter_mut().find(|(h, _)| *h == handle) {
            Some(entry) => entry.1 = synced,
            None => all.push((handle, synced)),
        }
    }
    // entities gone from the list altogether
    let gone: Vec<(EntityHandle, Synced)> = host(lua).movers.synced.borrow().iter().filter(|(h, _)| !seen.contains(h)).cloned().collect();
    for (handle, synced) in gone {
        if let Some(model) = synced.model {
            let host = host(lua);
            let mut collision = host.world.collision.borrow_mut();
            if let Some(c) = collision.as_mut() {
                std::sync::Arc::make_mut(c).place_model(model, None);
            }
        }
        if let Some(body) = synced.body {
            crate::physics::remove_body(lua, body);
        }
        host(lua).movers.synced.borrow_mut().retain(|(h, _)| *h != handle);
    }
}

/// The body of something that may move: its brushes, or a model door's physics model.
fn add_body(lua: &Lua, handle: EntityHandle, brush: Option<usize>, pos: Vector, angles: Angle) -> Option<BodyId> {
    let hulls: Vec<Vec<[f32; 3]>> = match brush {
        Some(model) => host(lua).world.collision.borrow().as_ref()?.model_hulls(model, gmod_bsp::mask::SOLID_BRUSHONLY),
        None => {
            let model = host(lua).entities.get(handle)?.model.clone();
            let phy = crate::physics::phy(lua, &model)?;
            phy.solids.first()?.convexes.iter().map(|c| c.points.clone()).collect()
        }
    };
    let user = u64::from(handle.index?);
    let id = host(lua).physics.physics.borrow_mut().add_mover(&hulls, array(pos), [angles.p, angles.y, angles.r], user)?;
    crate::physics::keep(lua, id, handle, 0, &gmod_physics::Shape::Hulls(hulls), "default");
    Some(id)
}

/// A new map: nothing is synced yet.
pub(crate) fn clear(lua: &Lua) {
    host(lua).movers.synced.borrow_mut().clear();
}
