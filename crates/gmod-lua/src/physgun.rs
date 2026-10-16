//! The physics gun, `weapon_physgun`, which GMod makes in C++.
//!
//! While `IN_ATTACK` is held, what the player aims at is taken - if it has a body and the
//! `PhysgunPickup` hook lets it (`OnPhysgunPickup` then) - and held where they aim, as far as it
//! was, by the point it was taken at, turning as they turn. The mouse wheel takes it farther or
//! nearer, and the mouse turns it while `IN_USE` is held (the player's view stays). Letting go
//! drops it moving as it moved; `IN_ATTACK2` freezes it where it is (`OnPhysgunFreeze`) and
//! lets go, and the gun takes nothing more until `IN_ATTACK` comes up; either way
//! `PhysgunDrop`. `IN_RELOAD` with nothing held asks `OnPhysgunReload`,
//! which unfreezes what the player looks at. A frozen body taken by the gun moves again. Of a
//! ragdoll the gun takes the part aimed at (the trace's `PhysicsBone`), and freezes that.
//!
//! Every tick before the bodies move, what is held is dragged where it should be
//! ([`gmod_physics::Physics::steer`]), no faster than `physgun_maxSpeed` and
//! `physgun_maxAngular` allow, in `physgun_timeToArrive` seconds; what is in its way stops it.
//! It is dropped when the player dies, puts the gun away or leaves, and when it loses its body.

use std::cell::RefCell;

use mlua::{Lua, MultiValue, Value};

use crate::entity::{self, EntityHandle};
use crate::movedata::UserCmd;
use crate::trace::{Filter, Sweep};
use crate::vector::{Angle, Vector};
use crate::{host, player, timer};

pub const CLASS: &str = "weapon_physgun";

/// The physics gun's console variables, as GMod has them.
pub(crate) const CONVARS: &[(&str, &str, &str)] = &[
    ("physgun_maxrange", "4096", "Maximum range of the physics gun"),
    ("physgun_timeToArrive", "0.05", ""),
    ("physgun_maxSpeed", "5000", ""),
    ("physgun_maxAngular", "5000", ""),
];

const IN_ATTACK: u32 = 1;
const IN_USE: u32 = 32;
const IN_ATTACK2: u32 = 2048;
const IN_RELOAD: u32 = 8192;

/// How much nearer or farther a notch of the mouse wheel takes what is held, in units.
const WHEEL_UNITS: f32 = 20.0;
/// How many degrees what is held turns for a count of the mouse.
const TURN_PER_COUNT: f32 = 0.2;
/// The nearest what is held comes.
const NEAREST: f32 = 24.0;

/// What a player holds with the gun.
#[derive(Debug, Clone, Copy)]
struct Held {
    entity: EntityHandle,
    /// Which of its bodies: a part of a ragdoll, 0 for others.
    part: usize,
    weapon: EntityHandle,
    /// The point it was taken at, in its own space.
    point: Vector,
    distance: f32,
    /// How it is turned when the player looks along `yaw`.
    angles: Angle,
    yaw: f32,
    /// Whether its body fell before it was taken: held, it does not.
    gravity: bool,
}

/// What each player holds.
#[derive(Default)]
pub(crate) struct Guns {
    held: RefCell<Vec<(EntityHandle, Held)>>,
    /// Players who froze what they held, whose `IN_ATTACK` must come up before the gun takes
    /// anything again.
    waiting: RefCell<Vec<EntityHandle>>,
}

fn held(lua: &Lua, player: EntityHandle) -> Option<Held> {
    host(lua).physgun.held.borrow().iter().find(|(p, _)| *p == player).map(|&(_, h)| h)
}

fn set_held(lua: &Lua, player: EntityHandle, held: Option<Held>) -> Option<Held> {
    // the gun's beam goes to what it holds, for the clients to draw
    let beam = held.map(|h| (h.entity, h.point));
    if let Some(gun) = held.map(|h| h.weapon).or_else(|| self::held(lua, player).map(|h| h.weapon)) {
        host(lua).entities.with_mut(gun, |ent| ent.beam = beam);
    }
    let host = host(lua);
    let mut all = host.physgun.held.borrow_mut();
    let was = all.iter().position(|(p, _)| *p == player).map(|i| all.remove(i).1);
    all.extend(held.map(|h| (player, h)));
    was
}

/// What a player holds with the gun, if anything.
pub(crate) fn holding(lua: &Lua, player: EntityHandle) -> Option<EntityHandle> {
    held(lua, player).map(|h| h.entity)
}

fn number(lua: &Lua, name: &str, default: f32) -> f32 {
    crate::convar::value(lua, name).and_then(|v| v.trim().parse().ok()).unwrap_or(default)
}

fn truthy(values: &MultiValue) -> bool {
    !matches!(values.front(), None | Some(Value::Nil) | Some(Value::Boolean(false)))
}

fn hook(lua: &Lua, name: &str, args: &[EntityHandle]) -> MultiValue {
    let args = args.iter().map(|&h| entity::push(lua, h).unwrap_or(Value::Nil));
    crate::call_hook(lua, name, MultiValue::from_iter(args))
}

/// A point of the world in the space of something turned by `angles` at `origin`.
fn local(point: Vector, origin: Vector, angles: Angle) -> Vector {
    let (forward, right, up) = angles.vectors();
    let d = point - origin;
    Vector::new(d.dot(forward), -d.dot(right), d.dot(up))
}

/// The command of a player whose gun is in their hands, as their tick runs it.
pub(crate) fn command(lua: &Lua, player: EntityHandle, weapon: EntityHandle, cmd: &UserCmd, pressed: u32) {
    let is_gun = host(lua).entities.get(weapon).is_some_and(|ent| !ent.scripted && ent.class.eq_ignore_ascii_case(CLASS));
    if !is_gun {
        return;
    }
    // the claws open while it holds something, and close again
    let open = if held(lua, player).is_some() { 1.0 } else { 0.0 };
    crate::anim::approach_view_model_pose(lua, player, "active", open, 4.0);
    if cmd.buttons & IN_ATTACK == 0 {
        host(lua).physgun.waiting.borrow_mut().retain(|&p| p != player);
    }
    // as Half-Life 2's physics gun: the key going down plays its firing, which stays at its
    // end while the key is held; let go, it goes back to idle ([`crate::anim::weapon_idle`])
    if pressed & IN_ATTACK != 0 {
        crate::anim::send_weapon_anim(lua, weapon, crate::anim::ACT_VM_PRIMARYATTACK);
    }
    match held(lua, player) {
        Some(_) if cmd.buttons & IN_ATTACK == 0 => drop(lua, player),
        Some(held) if pressed & IN_ATTACK2 != 0 => {
            let phys = crate::physics::push_part(lua, held.entity, held.part).unwrap_or(Value::Nil);
            let args = [entity::push(lua, weapon), Ok(phys), entity::push(lua, held.entity), entity::push(lua, player)];
            crate::call_hook(lua, "OnPhysgunFreeze", args.into_iter().map(|v| v.unwrap_or(Value::Nil)).collect());
            host(lua).physgun.waiting.borrow_mut().push(player);
            drop(lua, player);
        }
        Some(mut held) => {
            held.distance = (held.distance + cmd.mouse_wheel as f32 * WHEEL_UNITS).max(NEAREST);
            if cmd.buttons & IN_USE != 0 {
                let right = Angle::new(0.0, held.yaw, 0.0).vectors().1;
                held.angles = held
                    .angles
                    .rotated_about(Vector::new(0.0, 0.0, 1.0), -cmd.mouse_x as f32 * TURN_PER_COUNT)
                    .rotated_about(right, cmd.mouse_y as f32 * TURN_PER_COUNT);
            }
            set_held(lua, player, Some(held));
        }
        None if cmd.buttons & IN_ATTACK != 0 => {
            if !host(lua).physgun.waiting.borrow().contains(&player) {
                pick_up(lua, player, weapon);
            }
        }
        None => {
            if pressed & IN_RELOAD != 0 {
                hook(lua, "OnPhysgunReload", &[weapon, player]);
            }
        }
    }
    // the beam, for the clients to draw: to what it holds, else where it aims while the key is
    // down (`NULL`: the client follows the aim itself)
    let beam = match held(lua, player) {
        Some(h) => Some((h.entity, h.point)),
        None if cmd.buttons & IN_ATTACK != 0 => Some((EntityHandle::NULL, Vector::default())),
        None => None,
    };
    host(lua).entities.with_mut(weapon, |ent| ent.beam = beam);
}

/// Takes what the player aims at, if it can be taken.
fn pick_up(lua: &Lua, player: EntityHandle, weapon: EntityHandle) {
    let Some((eye, aim)) = player::eye(lua, player) else { return };
    let range = number(lua, "physgun_maxrange", 4096.0);
    let sweep = Sweep { mask: gmod_bsp::mask::SHOT, ..Sweep::line(eye, eye + aim * range, Filter::Entities(vec![player])) };
    let hit = crate::trace::trace(lua, &sweep);
    let target = hit.entity;
    if target.index.is_none_or(|i| i == 0) || crate::physics::body(lua, target).is_none() {
        return;
    }
    if !truthy(&hook(lua, "PhysgunPickup", &[player, target])) {
        return;
    }
    crate::pickup::release(lua, target);
    // the hook may have taken its body away
    let part = hit.physics_bone;
    let Some(body) = crate::physics::part_body(lua, target, part) else { return };
    let Some((origin, angles)) = host(lua).physics.physics.borrow().pose(body) else { return };
    let (origin, angles) = (crate::physics::vector(origin), crate::physics::angle(angles));
    let yaw = player::eye_angles(lua, player).map_or(0.0, |a| a.y);
    let gravity = {
        let host = host(lua);
        let mut physics = host.physics.physics.borrow_mut();
        let gravity = physics.gravity_enabled(body);
        physics.set_motion(body, true);
        physics.set_gravity_enabled(body, false);
        physics.wake(body);
        gravity
    };
    let held = Held {
        entity: target,
        part,
        weapon,
        point: local(hit.end, origin, angles),
        distance: (hit.end - eye).length().max(NEAREST),
        angles,
        yaw,
        gravity,
    };
    set_held(lua, player, Some(held));
    hook(lua, "OnPhysgunPickup", &[player, target]);
}

/// Whoever holds an entity with the gun lets go of it: a hand picks it up.
pub(crate) fn release(lua: &Lua, entity: EntityHandle) {
    let holders: Vec<EntityHandle> = host(lua).physgun.held.borrow().iter().filter(|(_, h)| h.entity == entity).map(|(p, _)| *p).collect();
    for player in holders {
        drop(lua, player);
    }
}

/// Lets go of what a player holds: it moves on as it moved. `PhysgunDrop`.
pub(crate) fn drop(lua: &Lua, player: EntityHandle) {
    let Some(held) = set_held(lua, player, None) else { return };
    if let Some(body) = crate::physics::part_body(lua, held.entity, held.part) {
        host(lua).physics.physics.borrow_mut().set_gravity_enabled(body, held.gravity);
    }
    if host(lua).entities.is_valid(held.entity) {
        hook(lua, "PhysgunDrop", &[player, held.entity]);
    }
}

/// One tick, before the bodies move: what is held is dragged where its player aims; what can
/// no longer be held is dropped.
pub(crate) fn think(lua: &Lua) {
    let all: Vec<(EntityHandle, Held)> = host(lua).physgun.held.borrow().clone();
    for (player, held) in all {
        let in_hands = player::active_weapon(lua, player) == Some(held.weapon) && player::alive(lua, player);
        // frozen while held (by Lua), it is let go
        let body = crate::physics::part_body(lua, held.entity, held.part)
            .filter(|&b| host(lua).entities.is_valid(held.entity) && host(lua).physics.physics.borrow().motion(b));
        let (Some(body), true) = (body, in_hands) else {
            drop(lua, player);
            continue;
        };
        let Some((eye, aim)) = player::eye(lua, player) else { continue };
        let yaw = player::eye_angles(lua, player).map_or(held.yaw, |a| a.y);
        let angles = held.angles.rotated_about(Vector::new(0.0, 0.0, 1.0), yaw - held.yaw);
        let origin = eye + aim * held.distance - held.point.rotated(angles);
        let seconds = number(lua, "physgun_timeToArrive", 0.05).max(timer::tick_interval() as f32);
        let (speed, turn) = (number(lua, "physgun_maxSpeed", 5000.0), number(lua, "physgun_maxAngular", 5000.0));
        host(lua).physics.physics.borrow_mut().steer(
            body,
            [origin.x, origin.y, origin.z],
            [angles.p, angles.y, angles.r],
            seconds,
            speed,
            turn,
        );
    }
}
