//! Touching: what is inside a trigger, and what a player walks over.
//!
//! Every tick the engine looks at what overlaps what, and tells the start, the going on and the
//! end of each touch:
//!
//! * The map's triggers are brush entities (`trigger_*`) whose brushes make a volume nothing
//!   collides with. What is inside is what the trigger lets in - its spawn flags: 1 players,
//!   2 NPCs, 8 physics objects, 64 everything but debris, 1024 physics debris - and its
//!   `filtername` passes (`filter_activator_name`, `filter_activator_class`, `filter_multi`,
//!   each `Negated` or not). They fire `OnStartTouch` and `OnEndTouch` with what came or went
//!   as the activator, `OnStartTouchAll` for the first one in and `OnEndTouchAll` for the last
//!   one out; `Enable`, `Disable` (everything inside leaves), `Toggle`, `StartDisabled`, and
//!   `TouchTest` (`OnTouching` or `OnNotTouching`). Each class does its part:
//!   * `trigger_once` fires `OnTrigger` and is gone; `trigger_multiple` fires it again after
//!     `wait` seconds (never with -1) while something is inside;
//!   * `trigger_teleport` puts what comes in where its `target` is, turned as the target is,
//!     still (or, with a `landmark`, where it stood from the landmark, keeping its speed);
//!   * `trigger_push` carries what is inside along `pushdir` at `speed`: a player at least that
//!     fast that way, so that a push upwards launches them, a body faster each tick;
//!   * `trigger_hurt` deals `damage` (of `damagetype`) every half second (`OnHurt`,
//!     `OnHurtPlayer`); `SetDamage`;
//!   * `trigger_remove` removes what comes in.
//! * A scripted entity that `SetTrigger(true)` is a trigger by its box: `ENT:StartTouch`,
//!   `ENT:Touch` every tick and `ENT:EndTouch`, with what touches it - players, NPCs, bodies of
//!   physics, solid scripted entities. A scripted entity touching a trigger hears it the same
//!   way.
//! * A player who walks over a weapon on the ground picks it up when `PlayerCanPickupWeapon`
//!   lets them - or, having one of the class already, takes the ammo in it; over an item of
//!   HL2, takes it ([`crate::items`]).

use std::cell::RefCell;

use mlua::{Lua, MultiValue};

use crate::entity::{self, EntityHandle, Kind};
use crate::host;
use crate::vector::{Angle, Vector};

/// What a trigger of the map keeps.
#[derive(Debug, Clone, PartialEq)]
pub struct Trigger {
    enabled: bool,
    /// A `trigger_multiple` fires again from then; a `trigger_hurt` hurts next then.
    next: f64,
}

/// What touched what last tick: (trigger, toucher).
#[derive(Default)]
pub(crate) struct Touching {
    pairs: RefCell<Vec<(EntityHandle, EntityHandle)>>,
}

/// How long `trigger_hurt` waits between hurts, and how near a weapon must be to be picked up.
const HURT_EVERY: f64 = 0.5;
const PICKUP_REACH: f32 = 8.0;

const SOLID_NONE: i64 = 0;

/// The brush model of a map's trigger, if the entity is one.
fn brush_trigger(lua: &Lua, handle: EntityHandle) -> Option<usize> {
    let host = host(lua);
    let ent = host.entities.get(handle)?;
    if ent.scripted || !ent.class.to_ascii_lowercase().starts_with("trigger_") {
        return None;
    }
    ent.model.strip_prefix('*')?.parse().ok()
}

/// A spawned trigger of the map takes up its part.
pub(crate) fn spawned(lua: &Lua, handle: EntityHandle) {
    if brush_trigger(lua, handle).is_none() {
        return;
    }
    let enabled =
        host(lua).entities.get(handle).is_some_and(|ent| ent.keyvalue("StartDisabled").is_none_or(|v| crate::convar::atof(v) == 0.0));
    host(lua).entities.with_mut(handle, |ent| {
        ent.trigger_state = Some(Box::new(Trigger { enabled, next: 0.0 }));
        ent.trigger = true;
    });
}

fn state(lua: &Lua, handle: EntityHandle) -> Option<Trigger> {
    host(lua).entities.get(handle).and_then(|ent| ent.trigger_state.as_deref().cloned())
}

fn set_state(lua: &Lua, handle: EntityHandle, state: Trigger) {
    host(lua).entities.with_mut(handle, |ent| ent.trigger_state = Some(Box::new(state)));
}

/// An input to a trigger of the map. Whether it took it.
pub(crate) fn input(lua: &Lua, handle: EntityHandle, input: &str, param: &str, activator: EntityHandle, caller: EntityHandle) -> bool {
    let Some(mut state) = state(lua, handle) else { return false };
    match input.to_ascii_lowercase().as_str() {
        "enable" => state.enabled = true,
        "disable" => state.enabled = false,
        "toggle" => state.enabled = !state.enabled,
        "touchtest" => {
            let touching = host(lua).touching.pairs.borrow().iter().any(|&(t, _)| t == handle);
            let output = if touching { "OnTouching" } else { "OnNotTouching" };
            crate::outputs::fire(lua, handle, output, activator, caller, None);
            return true;
        }
        "setdamage" => {
            entity_key(lua, handle, "damage", param);
            return true;
        }
        _ => return false,
    }
    set_state(lua, handle, state);
    true
}

fn entity_key(lua: &Lua, handle: EntityHandle, key: &str, value: &str) {
    host(lua).entities.with_mut(handle, |ent| match ent.keyvalues.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
        Some(pair) => pair.1 = value.to_string(),
        None => ent.keyvalues.push((key.to_string(), value.to_string())),
    });
}

fn key_number(lua: &Lua, handle: EntityHandle, key: &str, default: f64) -> f64 {
    host(lua).entities.get(handle).and_then(|ent| ent.keyvalue(key).map(crate::convar::atof)).unwrap_or(default)
}

fn key_text(lua: &Lua, handle: EntityHandle, key: &str) -> String {
    host(lua).entities.get(handle).and_then(|ent| ent.keyvalue(key).map(str::to_string)).unwrap_or_default()
}

/// Something that may touch: where its box is, and what it is.
struct Toucher {
    handle: EntityHandle,
    lo: Vector,
    hi: Vector,
    player: bool,
    npc: bool,
    body: bool,
    debris: bool,
    scripted: bool,
}

fn overlap(a: (Vector, Vector), b: (Vector, Vector)) -> bool {
    a.0.x <= b.1.x && a.1.x >= b.0.x && a.0.y <= b.1.y && a.1.y >= b.0.y && a.0.z <= b.1.z && a.1.z >= b.0.z
}

/// What may touch: players alive, NPCs, bodies of physics, solid scripted entities - not the
/// world, not triggers, not what a player carries.
fn touchers(lua: &Lua) -> Vec<Toucher> {
    let handles = host(lua).entities.handles();
    let mut out = Vec::new();
    for handle in handles {
        if handle.index.is_none_or(|i| i == 0) {
            continue;
        }
        let entities = &host(lua).entities;
        let Some(ent) = entities.get(handle) else { continue };
        if ent.removed || !ent.spawned || ent.trigger || ent.owner.index.is_some() && handle.kind == Kind::Weapon {
            continue;
        }
        let scripted = ent.scripted;
        let solid = ent.solid != SOLID_NONE;
        let debris = ent.collision_group == crate::trace::group::DEBRIS;
        drop(ent);
        let player = handle.kind == Kind::Player;
        if player && !crate::player::alive(lua, handle) {
            continue;
        }
        let npc = handle.kind == Kind::Npc || handle.kind == Kind::NextBot;
        let body = crate::physics::body(lua, handle).is_some();
        if !(player || npc || body || scripted && solid) {
            continue;
        }
        let Some((lo, hi)) =
            crate::ragdoll::world_bounds(lua, handle).or_else(|| entities.world_bounds(handle).map(|(_, _, lo, hi)| (lo, hi)))
        else {
            continue;
        };
        out.push(Toucher { handle, lo, hi, player, npc, body, debris, scripted });
    }
    out
}

/// Whether a trigger's spawn flags let something in.
fn lets_in(flags: i64, t: &Toucher) -> bool {
    (flags & 1 != 0 && t.player)
        || (flags & 2 != 0 && t.npc)
        || (flags & 8 != 0 && t.body && !t.debris && !t.player && !t.npc)
        || (flags & 1024 != 0 && t.body && t.debris)
        || (flags & 64 != 0 && !t.debris)
}

/// Whether the filter entity a trigger names lets something in; no such filter lets all in.
fn filter_passes(lua: &Lua, filter: &str, other: EntityHandle, depth: u32) -> bool {
    if filter.is_empty() || depth > 8 {
        return true;
    }
    let Some(found) = crate::ents::named(lua, filter) else { return true };
    let (class, negated) = {
        let host = host(lua);
        let Some(ent) = host.entities.get(found) else { return true };
        (ent.class.to_ascii_lowercase(), ent.keyvalue("Negated").is_some_and(|v| crate::convar::atof(v) != 0.0))
    };
    let other_ent = |f: &dyn Fn(&crate::entity::Ent) -> bool| host(lua).entities.get(other).is_some_and(|e| f(&e));
    let passes = match class.as_str() {
        "filter_activator_name" => {
            let name = key_text(lua, found, "filtername");
            other_ent(&|e| e.name.eq_ignore_ascii_case(&name))
        }
        "filter_activator_class" => {
            let wanted = key_text(lua, found, "filterclass");
            other_ent(&|e| e.class.eq_ignore_ascii_case(&wanted))
        }
        "filter_multi" => {
            let or = key_number(lua, found, "FilterType", 0.0) != 0.0;
            let names: Vec<String> = (1..=5).map(|i| key_text(lua, found, &format!("Filter{i:02}"))).filter(|n| !n.is_empty()).collect();
            let mut results = names.iter().map(|n| filter_passes(lua, n, other, depth + 1));
            if or { results.any(|r| r) } else { results.all(|r| r) }
        }
        _ => true,
    };
    passes != negated
}

/// One tick of touching, on the server.
pub(crate) fn tick(lua: &Lua) {
    let touchers = touchers(lua);
    let collision = host(lua).world.collision.borrow().clone();
    let mut now_touching = Vec::new();
    let triggers: Vec<EntityHandle> = {
        let entities = &host(lua).entities;
        entities.handles().into_iter().filter(|&h| entities.get(h).is_some_and(|e| e.trigger && e.spawned && !e.removed)).collect()
    };
    for trigger in triggers {
        let Some((pos, angles, lo, hi)) = host(lua).entities.world_bounds(trigger) else { continue };
        let brush = brush_trigger(lua, trigger);
        if brush.is_some() && !state(lua, trigger).is_some_and(|s| s.enabled) {
            continue;
        }
        let (flags, filter) = match brush {
            Some(_) => (host(lua).entities.get(trigger).map_or(0, |e| e.spawn_flags), key_text(lua, trigger, "filtername")),
            None => (1 | 2 | 64, String::new()),
        };
        for t in &touchers {
            if t.handle == trigger || !overlap((lo, hi), (t.lo, t.hi)) || !lets_in(flags, t) {
                continue;
            }
            let inside = match (brush, collision.as_deref()) {
                (Some(model), Some(collision)) => {
                    let placement = gmod_bsp::collision::Placement::new(model, None, array(pos), [angles.p, angles.y, angles.r]);
                    let (center, half) = ((t.lo + t.hi) * 0.5, (t.hi - t.lo) * 0.5);
                    let (c, h) = (array(center), array(half));
                    let trace = collision.trace_placed(&placement, c, c, [-h[0], -h[1], -h[2]], h, gmod_bsp::mask::ALL);
                    trace.start_solid
                }
                // a brush without the map's collision, or a scripted trigger: its box
                _ => true,
            };
            if inside && filter_passes(lua, &filter, t.handle, 0) {
                now_touching.push((trigger, t.handle, t.scripted));
            }
        }
    }
    let before = host(lua).touching.pairs.replace(now_touching.iter().map(|&(a, b, _)| (a, b)).collect());
    let valid = |h: EntityHandle| host(lua).entities.is_valid(h);
    // what left: the toucher went, or the trigger, or they no longer meet
    for &(trigger, other) in &before {
        if !now_touching.iter().any(|&(a, b, _)| a == trigger && b == other) {
            let others_left = !now_touching.iter().any(|&(a, _, _)| a == trigger);
            end_touch(lua, trigger, other, others_left && valid(trigger));
        }
    }
    for &(trigger, other, scripted) in &now_touching {
        if !valid(trigger) || !valid(other) {
            continue;
        }
        let new = !before.iter().any(|&(a, b)| a == trigger && b == other);
        if new {
            let first = !before.iter().any(|&(a, _)| a == trigger)
                && now_touching.iter().find(|&&(a, _, _)| a == trigger).is_some_and(|&(_, b, _)| b == other);
            start_touch(lua, trigger, other, scripted, first);
        }
        touch(lua, trigger, other, scripted);
    }
    pick_up(lua, &touchers);
    crate::items::touch(lua, &touchers.iter().filter(|t| t.player).map(|t| (t.handle, t.lo, t.hi)).collect::<Vec<_>>());
}

fn array(v: Vector) -> [f32; 3] {
    [v.x, v.y, v.z]
}

fn call(lua: &Lua, handle: EntityHandle, method: &str, other: EntityHandle) {
    let scripted = host(lua).entities.get(handle).is_some_and(|ent| ent.scripted);
    if !scripted {
        return;
    }
    let (Ok(mlua::Value::UserData(ud)), Ok(other)) = (entity::push(lua, handle), entity::push(lua, other)) else { return };
    crate::ents::call_method(lua, &ud, method, MultiValue::from_iter([other]));
}

fn start_touch(lua: &Lua, trigger: EntityHandle, other: EntityHandle, scripted: bool, first: bool) {
    call(lua, trigger, "StartTouch", other);
    if scripted {
        call(lua, other, "StartTouch", trigger);
    }
    if brush_trigger(lua, trigger).is_none() {
        return;
    }
    crate::outputs::fire(lua, trigger, "OnStartTouch", other, trigger, None);
    if first {
        crate::outputs::fire(lua, trigger, "OnStartTouchAll", other, trigger, None);
    }
    let class = host(lua).entities.get(trigger).map(|e| e.class.to_ascii_lowercase()).unwrap_or_default();
    match class.as_str() {
        "trigger_teleport" => teleport(lua, trigger, other),
        "trigger_remove" => crate::ents::remove(lua, other),
        _ => {}
    }
}

fn end_touch(lua: &Lua, trigger: EntityHandle, other: EntityHandle, last: bool) {
    let valid = |h: EntityHandle| host(lua).entities.is_valid(h);
    if valid(trigger) && valid(other) {
        call(lua, trigger, "EndTouch", other);
        call(lua, other, "EndTouch", trigger);
    }
    if !valid(trigger) || brush_trigger(lua, trigger).is_none() {
        return;
    }
    let activator = if valid(other) { other } else { EntityHandle::NULL };
    crate::outputs::fire(lua, trigger, "OnEndTouch", activator, trigger, None);
    if last {
        crate::outputs::fire(lua, trigger, "OnEndTouchAll", activator, trigger, None);
    }
}

/// Something is inside a trigger, this tick.
fn touch(lua: &Lua, trigger: EntityHandle, other: EntityHandle, scripted: bool) {
    call(lua, trigger, "Touch", other);
    if scripted {
        call(lua, other, "Touch", trigger);
    }
    let valid = |h: EntityHandle| host(lua).entities.is_valid(h);
    if !valid(trigger) || !valid(other) || brush_trigger(lua, trigger).is_none() {
        return;
    }
    let now = host(lua).clock.cur_time.get();
    let class = host(lua).entities.get(trigger).map(|e| e.class.to_ascii_lowercase()).unwrap_or_default();
    match class.as_str() {
        "trigger_once" | "trigger_multiple" => {
            let Some(mut state) = state(lua, trigger) else { return };
            if now < state.next {
                return;
            }
            crate::outputs::fire(lua, trigger, "OnTrigger", other, trigger, None);
            let wait = key_number(lua, trigger, "wait", 1.0);
            if class == "trigger_once" {
                crate::ents::remove(lua, trigger);
                return;
            }
            state.next = if wait < 0.0 { f64::INFINITY } else { now + wait.max(crate::timer::tick_interval()) };
            set_state(lua, trigger, state);
        }
        "trigger_push" => push(lua, trigger, other),
        "trigger_hurt" => {
            let Some(mut state) = state(lua, trigger) else { return };
            // every half second, all that is inside at once
            if now < state.next {
                return;
            }
            let others: Vec<EntityHandle> =
                host(lua).touching.pairs.borrow().iter().filter(|&&(t, _)| t == trigger).map(|&(_, o)| o).collect();
            state.next = now + HURT_EVERY;
            set_state(lua, trigger, state);
            let damage = key_number(lua, trigger, "damage", 10.0) as f32;
            let kind = key_number(lua, trigger, "damagetype", 0.0) as i64;
            for victim in others {
                if !valid(victim) {
                    continue;
                }
                crate::damage::take(lua, victim, crate::damage::DamageInfo::new(damage, kind, trigger, trigger));
                crate::outputs::fire(lua, trigger, "OnHurt", victim, trigger, None);
                if victim.kind == Kind::Player {
                    crate::outputs::fire(lua, trigger, "OnHurtPlayer", victim, trigger, None);
                }
            }
        }
        _ => {}
    }
}

/// `trigger_teleport`: to its `target`, still; with a `landmark`, kept where it was from the
/// landmark, at its speed.
fn teleport(lua: &Lua, trigger: EntityHandle, other: EntityHandle) {
    let target = key_text(lua, trigger, "target");
    let Some(destination) = crate::ents::named(lua, &target) else { return };
    let Some((to, turned)) = host(lua).entities.transform(destination) else { return };
    let landmark = key_text(lua, trigger, "landmark");
    let from_landmark = crate::ents::named(lua, &landmark).and_then(|l| host(lua).entities.transform(l));
    let Some((pos, _)) = host(lua).entities.transform(other) else { return };
    let (place, keep_speed) = match from_landmark {
        Some((mark, _)) => (to + (pos - mark), true),
        None => (to, false),
    };
    entity::set_world_pos(lua, other, place);
    if other.kind == Kind::Player {
        if !keep_speed {
            crate::player::set_eye_angles(lua, other, turned);
            host(lua).entities.with_mut(other, |ent| ent.velocity = Vector::default());
        }
        return;
    }
    if !keep_speed {
        entity::set_world_angles(lua, other, turned);
        host(lua).entities.with_mut(other, |ent| ent.velocity = Vector::default());
    }
    crate::physics::moved(lua, other);
    if !keep_speed && let Some(id) = crate::physics::body(lua, other) {
        host(lua).physics.physics.borrow_mut().set_velocity(id, [0.0; 3]);
    }
}

/// `trigger_push`, one tick of it.
fn push(lua: &Lua, trigger: EntityHandle, other: EntityHandle) {
    let [p, y, r] = crate::ents::three(&key_text(lua, trigger, "pushdir"));
    let (_, angles) = host(lua).entities.transform(trigger).unwrap_or_default();
    // the direction is in the trigger's own space
    let direction = Angle::new(p, y, r).vectors().0.rotated(angles);
    let speed = key_number(lua, trigger, "speed", 40.0) as f32;
    if other.kind == Kind::Player {
        host(lua).entities.with_mut(other, |ent| {
            let along = ent.velocity.dot(direction);
            if along < speed {
                ent.velocity = ent.velocity + direction * (speed - along);
            }
        });
        return;
    }
    if let Some(id) = crate::physics::body(lua, other) {
        let host = host(lua);
        let mut physics = host.physics.physics.borrow_mut();
        let v = physics.velocity(id);
        let push = direction * (speed * crate::timer::tick_interval() as f32);
        physics.set_velocity(id, [v[0] + push.x, v[1] + push.y, v[2] + push.z]);
        physics.wake(id);
    }
}

/// Players walking over weapons on the ground.
fn pick_up(lua: &Lua, touchers: &[Toucher]) {
    let weapons: Vec<(EntityHandle, Vector, Vector)> = {
        let entities = &host(lua).entities;
        entities
            .handles()
            .into_iter()
            .filter(|&h| h.kind == Kind::Weapon)
            .filter(|&h| entities.get(h).is_some_and(|e| !e.removed && e.spawned && e.owner.index.is_none() && e.parent.index.is_none()))
            .filter_map(|h| entities.world_bounds(h).map(|(_, _, lo, hi)| (h, lo, hi)))
            .collect()
    };
    let reach = Vector::new(PICKUP_REACH, PICKUP_REACH, PICKUP_REACH);
    for t in touchers.iter().filter(|t| t.player) {
        for &(weapon, lo, hi) in &weapons {
            if overlap((t.lo, t.hi), (lo - reach, hi + reach)) && host(lua).entities.get(weapon).is_some_and(|e| e.owner.index.is_none()) {
                crate::player::bump_weapon(lua, t.handle, weapon);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toucher(player: bool, npc: bool, body: bool, debris: bool) -> Toucher {
        let zero = Vector::default();
        Toucher { handle: EntityHandle::NULL, lo: zero, hi: zero, player, npc, body, debris, scripted: false }
    }

    #[test]
    fn what_a_trigger_lets_in() {
        let (player, npc, prop, debris) = (
            toucher(true, false, false, false),
            toucher(false, true, false, false),
            toucher(false, false, true, false),
            toucher(false, false, true, true),
        );
        assert!(lets_in(1, &player) && !lets_in(1, &npc) && !lets_in(1, &prop));
        assert!(lets_in(2, &npc) && !lets_in(2, &player));
        assert!(lets_in(8, &prop) && !lets_in(8, &debris) && !lets_in(8, &player));
        assert!(lets_in(1024, &debris) && !lets_in(1024, &prop));
        assert!(lets_in(64, &player) && lets_in(64, &npc) && lets_in(64, &prop) && !lets_in(64, &debris));
        assert!(!lets_in(0, &player));
    }
}
