//! Carrying things with the use key, as Half-Life 2's player does: the use key on a prop that
//! may move and is light enough (35 kg at most) picks it up when `AllowPlayerPickup` lets it
//! (`OnPlayerPhysicsPickup`); it is held before the player's eyes as they look about, turned as
//! it was taken with the player's yaw, and the player walks through it. The use key again
//! lets it go, the attack key throws it (and fires no weapon) - `OnPlayerPhysicsDrop` with
//! whether it was thrown. Something held too far from where it should be - caught behind a
//! wall - falls, as does what the physics gun takes, what Lua freezes, and what a dead player
//! held. `Player:PickupObject` takes something whatever it weighs, `Player:DropObject` lets go,
//! `Entity:IsPlayerHolding` tells.

use std::cell::RefCell;

use mlua::{Lua, MultiValue, Value};

use crate::entity::{self, EntityHandle};
use crate::host;
use crate::vector::{Angle, Vector};

/// The heaviest a player picks up, in kilograms.
const MAX_MASS: f32 = 35.0;
/// How far from the eyes what is held is, beyond its size.
const GAP: f32 = 24.0;
/// How far from where it should be what is held may be before it falls.
const TOO_FAR: f32 = 64.0;
/// How hard a throw is: the speed of what weighs a kilogram, slower as it is heavier.
const THROW: f32 = 5000.0;
const MAX_THROW: f32 = 1000.0;

/// What a player carries.
#[derive(Debug, Clone, Copy)]
struct Carried {
    entity: EntityHandle,
    /// How it is turned when the player looks along `yaw`.
    angles: Angle,
    yaw: f32,
    /// From its eyes to its middle.
    distance: f32,
    /// Its middle, in its own space.
    middle: Vector,
    /// Whether its body fell before it was taken.
    gravity: bool,
    /// Held with the gravity gun ([`crate::physcannon`]) rather than by hand.
    gun: bool,
}

/// What each player carries.
#[derive(Default)]
pub(crate) struct Hands {
    carried: RefCell<Vec<(EntityHandle, Carried)>>,
}

fn carried(lua: &Lua, player: EntityHandle) -> Option<Carried> {
    host(lua).hands.carried.borrow().iter().find(|(p, _)| *p == player).map(|(_, c)| *c)
}

/// What a player carries, if anything.
pub(crate) fn carrying(lua: &Lua, player: EntityHandle) -> Option<EntityHandle> {
    carried(lua, player).map(|c| c.entity)
}

/// Whether a player carries an entity (`Entity:IsPlayerHolding`).
pub(crate) fn held(lua: &Lua, entity: EntityHandle) -> bool {
    host(lua).hands.carried.borrow().iter().any(|(_, c)| c.entity == entity)
}

fn hook(lua: &Lua, name: &str, args: impl IntoIterator<Item = Value>) -> MultiValue {
    crate::call_hook(lua, name, MultiValue::from_iter(args))
}

fn value(lua: &Lua, handle: EntityHandle) -> Value {
    entity::push(lua, handle).unwrap_or(Value::Nil)
}

/// Whether the use key may pick an entity up: a prop that may move, not a ragdoll, not what
/// another hand or the physics gun holds.
pub(crate) fn can_carry(lua: &Lua, target: EntityHandle) -> bool {
    let Some(body) = crate::physics::body(lua, target) else { return false };
    !crate::ragdoll::is_ragdoll(lua, target) && host(lua).physics.physics.borrow().motion(body) && !held(lua, target)
}

/// A player picks an entity up, if it can be carried and is light enough (unless `forced`) and
/// `AllowPlayerPickup` lets them. Whether they did.
pub(crate) fn pick_up(lua: &Lua, player: EntityHandle, target: EntityHandle, forced: bool) -> bool {
    take(lua, player, target, (!forced).then_some(MAX_MASS), false)
}

/// The gravity gun takes an entity, up to `max_mass` kilograms, if `GravGunPickupAllowed`
/// lets it (`GravGunOnPickedUp`). Whether it did.
pub(crate) fn pick_up_by_gun(lua: &Lua, player: EntityHandle, target: EntityHandle, max_mass: f32) -> bool {
    take(lua, player, target, Some(max_mass), true)
}

fn take(lua: &Lua, player: EntityHandle, target: EntityHandle, max_mass: Option<f32>, gun: bool) -> bool {
    if carrying(lua, player).is_some() || !can_carry(lua, target) {
        return false;
    }
    let Some(body) = crate::physics::body(lua, target) else { return false };
    let mass = host(lua).physics.physics.borrow().mass(body);
    if max_mass.is_some_and(|most| mass > most) {
        return false;
    }
    let asked = if gun { "GravGunPickupAllowed" } else { "AllowPlayerPickup" };
    let allowed = hook(lua, asked, [value(lua, player), value(lua, target)]);
    if matches!(allowed.front(), Some(Value::Boolean(false))) {
        return false;
    }
    crate::physgun::release(lua, target);
    let (Some((_, angles)), Some((mins, maxs))) =
        (host(lua).entities.transform(target), host(lua).entities.get(target).map(|ent| (ent.mins, ent.maxs)))
    else {
        return false;
    };
    let gravity = {
        let host = host(lua);
        let mut physics = host.physics.physics.borrow_mut();
        let gravity = physics.gravity_enabled(body);
        physics.set_gravity_enabled(body, false);
        gravity
    };
    let yaw = crate::player::eye_angles(lua, player).map_or(0.0, |a| a.y);
    let size = (maxs - mins).length() * 0.5;
    let carried = Carried { entity: target, angles, yaw, distance: size + GAP, middle: (mins + maxs) * 0.5, gravity, gun };
    host(lua).hands.carried.borrow_mut().push((player, carried));
    let told = if gun { "GravGunOnPickedUp" } else { "OnPlayerPhysicsPickup" };
    hook(lua, told, [value(lua, player), value(lua, target)]);
    true
}

/// Whether what a player carries the gravity gun holds.
pub(crate) fn carried_by_gun(lua: &Lua, player: EntityHandle) -> bool {
    carried(lua, player).is_some_and(|c| c.gun)
}

/// A player lets go of what they carry: it falls, or flies where they look if `thrown`.
pub(crate) fn drop(lua: &Lua, player: EntityHandle, thrown: bool) {
    let_go(lua, player, thrown.then_some(|mass: f32| (THROW / mass.max(1.0)).min(MAX_THROW)));
}

/// A player lets go of what they carry, thrown at the speed `throw` gives for its mass.
pub(crate) fn let_go(lua: &Lua, player: EntityHandle, throw: Option<impl Fn(f32) -> f32>) {
    let Some(c) = carried(lua, player) else { return };
    let thrown = throw.is_some();
    host(lua).hands.carried.borrow_mut().retain(|(p, _)| *p != player);
    if let Some(body) = crate::physics::body(lua, c.entity) {
        let host = host(lua);
        let mut physics = host.physics.physics.borrow_mut();
        physics.set_gravity_enabled(body, c.gravity);
        if let Some(throw) = throw
            && let Some((_, aim)) = crate::player::eye(lua, player)
        {
            let speed = throw(physics.mass(body));
            let own = host.entities.get(player).map(|ent| ent.velocity).unwrap_or_default();
            let v = own + aim * speed;
            physics.set_velocity(body, [v.x, v.y, v.z]);
            physics.wake(body);
        }
    }
    if host(lua).entities.is_valid(c.entity) {
        if c.gun {
            hook(lua, "GravGunOnDropped", [value(lua, player), value(lua, c.entity)]);
        } else {
            hook(lua, "OnPlayerPhysicsDrop", [value(lua, player), value(lua, c.entity), Value::Boolean(thrown)]);
        }
    }
}

/// Whoever carries an entity lets go of it: the physics gun takes it, or it goes.
pub(crate) fn release(lua: &Lua, entity: EntityHandle) {
    let holders: Vec<EntityHandle> = host(lua).hands.carried.borrow().iter().filter(|(_, c)| c.entity == entity).map(|(p, _)| *p).collect();
    for player in holders {
        drop(lua, player, false);
    }
}

/// One tick, before the bodies move: what is carried goes where its player looks; what can no
/// longer be carried falls.
pub(crate) fn think(lua: &Lua) {
    let all: Vec<(EntityHandle, Carried)> = host(lua).hands.carried.borrow().clone();
    for (player, c) in all {
        let body = crate::physics::body(lua, c.entity).filter(|&b| host(lua).physics.physics.borrow().motion(b));
        let alive = crate::player::alive(lua, player);
        let (Some(body), true, Some((eye, aim))) = (body, alive, crate::player::eye(lua, player)) else {
            drop(lua, player, false);
            continue;
        };
        let yaw = crate::player::eye_angles(lua, player).map_or(c.yaw, |a| a.y);
        let angles = c.angles.rotated_about(Vector::new(0.0, 0.0, 1.0), yaw - c.yaw);
        let target = eye + aim * c.distance;
        // caught behind something, it falls
        let now = host(lua).entities.transform(c.entity).map(|(pos, turned)| pos + c.middle.rotated(turned));
        if now.is_some_and(|m| (m - target).length() > TOO_FAR + c.distance) {
            drop(lua, player, false);
            continue;
        }
        let origin = target - c.middle.rotated(angles);
        let seconds = (crate::timer::TICK_INTERVAL as f32) * 4.0;
        host(lua).physics.physics.borrow_mut().steer(
            body,
            [origin.x, origin.y, origin.z],
            [angles.p, angles.y, angles.r],
            seconds,
            2000.0,
            1000.0,
        );
    }
}

pub(crate) fn install_player(def: &entity::Def) -> mlua::Result<()> {
    def("PickupObject", |a| {
        let player = crate::player::this(a)?;
        let target = entity::handle_of(a.get(2)).unwrap_or(EntityHandle::NULL);
        pick_up(a.lua, player, target, true);
        Ok(Value::Nil)
    })?;
    def("DropObject", |a| {
        let player = crate::player::this(a)?;
        drop(a.lua, player, false);
        Ok(Value::Nil)
    })?;
    Ok(())
}

pub(crate) fn install_methods(def: &entity::Def) -> mlua::Result<()> {
    def("IsPlayerHolding", |a| {
        let handle = entity::this(a, 1)?;
        Ok(Value::Boolean(held(a.lua, handle)))
    })?;
    Ok(())
}
