//! The gravity gun, `weapon_physcannon`, as Half-Life 2 has it: the attack key punts what is
//! before it (`GravGunPunt` may say no) or launches what it holds; the second attack key pulls
//! things to it - up to 250 kg (`physcannon_maxmass`) - takes them when they are near
//! (`GravGunPickupAllowed`, `GravGunOnPickedUp`), and lets go of what it holds
//! (`GravGunOnDropped`). What it holds goes where the player looks ([`crate::pickup`]). Its
//! claws open while it aims at something it could take or holds it.

use mlua::Lua;

use crate::entity::{self, EntityHandle};
use crate::host;
use crate::trace::{self, Filter, Sweep};

pub const CLASS: &str = "weapon_physcannon";

const IN_ATTACK: u32 = 1;
const IN_ATTACK2: u32 = 2048;

/// How far it punts and takes, how far it pulls from.
const REACH: f32 = 250.0;
const PULL_REACH: f32 = 1024.0;
/// How fast a punt sends what weighs up to `LIGHT` kilograms; heavier, slower.
const PUNT_SPEED: f32 = 1500.0;
const LIGHT: f32 = 100.0;
/// How fast something pulled comes.
const PULL_SPEED: f32 = 600.0;

fn number(lua: &Lua, name: &str, default: f32) -> f32 {
    crate::convar::value(lua, name).and_then(|v| v.trim().parse().ok()).unwrap_or(default)
}

fn now(lua: &Lua) -> f64 {
    host(lua).clock.cur_time.get()
}

fn value(lua: &Lua, handle: EntityHandle) -> mlua::Value {
    entity::push(lua, handle).unwrap_or(mlua::Value::Nil)
}

/// What the gun aims at within `reach` that has a body that moves, and where it is hit.
fn aimed(lua: &Lua, player: EntityHandle, reach: f32) -> Option<(EntityHandle, crate::vector::Vector)> {
    let (eye, aim) = crate::player::eye(lua, player)?;
    let sweep = Sweep { mask: gmod_bsp::mask::SHOT, ..Sweep::line(eye, eye + aim * reach, Filter::Entities(vec![player])) };
    let hit = trace::trace(lua, &sweep);
    let target = hit.entity;
    (target.index.is_some_and(|i| i != 0) && crate::pickup::can_carry(lua, target)).then_some((target, hit.end))
}

fn mass(lua: &Lua, target: EntityHandle) -> f32 {
    crate::physics::body(lua, target).map_or(0.0, |b| host(lua).physics.physics.borrow().mass(b))
}

fn next_fire(lua: &Lua, weapon: EntityHandle, slot: usize) -> f64 {
    host(lua).entities.get(weapon).and_then(|e| e.weapon.as_ref().map(|w| w.next_fire[slot])).unwrap_or(0.0)
}

fn set_next_fire(lua: &Lua, weapon: EntityHandle, slot: usize, at: f64) {
    host(lua).entities.with_mut(weapon, |ent| {
        if let Some(w) = ent.weapon.as_mut() {
            w.next_fire[slot] = at;
        }
    });
}

fn act(lua: &Lua, name: &str) -> i64 {
    match lua.globals().raw_get::<mlua::Value>(name) {
        Ok(mlua::Value::Integer(n)) => n,
        Ok(mlua::Value::Number(n)) => n as i64,
        _ => -1,
    }
}

/// A tick of the gun in its owner's hands that holds nothing, the keys held and pressed.
pub(crate) fn frame(lua: &Lua, player: EntityHandle, weapon: EntityHandle, buttons: u32, pressed: u32) {
    let max_mass = number(lua, "physcannon_maxmass", 250.0);
    let reach = number(lua, "physcannon_tracelength", REACH);
    let in_reach = aimed(lua, player, reach).filter(|&(t, _)| mass(lua, t) <= max_mass);
    let open = if in_reach.is_some() { 1.0 } else { 0.0 };
    crate::anim::approach_view_model_pose(lua, player, "active", open, 4.0);
    if pressed & IN_ATTACK != 0 && next_fire(lua, weapon, 0) <= now(lua) {
        punt(lua, player, weapon, reach);
        set_next_fire(lua, weapon, 0, now(lua) + 0.5);
        return;
    }
    if buttons & IN_ATTACK2 == 0 || next_fire(lua, weapon, 1) > now(lua) {
        return;
    }
    // near enough: taken; farther: pulled
    if let Some((target, _)) = in_reach {
        if crate::pickup::pick_up_by_gun(lua, player, target, max_mass) {
            crate::sound::emit_scaled(lua, "Weapon_PhysCannon.Pickup", weapon, 1.0);
            crate::anim::send_weapon_anim(lua, weapon, act(lua, "ACT_VM_PRIMARYATTACK"));
            set_next_fire(lua, weapon, 1, now(lua) + 0.5);
        }
        return;
    }
    let Some((target, _)) = aimed(lua, player, PULL_REACH) else {
        if pressed & IN_ATTACK2 != 0 {
            crate::sound::emit_scaled(lua, "Weapon_PhysCannon.TooHeavy", weapon, 1.0);
        }
        return;
    };
    if mass(lua, target) > max_mass {
        return;
    }
    let (Some((eye, _)), Some((pos, _)), Some(body)) =
        (crate::player::eye(lua, player), host(lua).entities.transform(target), crate::physics::body(lua, target))
    else {
        return;
    };
    let towards = (eye - pos).normalized() * PULL_SPEED;
    let host = host(lua);
    let mut physics = host.physics.physics.borrow_mut();
    physics.set_velocity(body, crate::physics::array(towards));
    physics.wake(body);
}

/// The attack key: what is before the gun is punted, if anything can be.
fn punt(lua: &Lua, player: EntityHandle, weapon: EntityHandle, reach: f32) {
    crate::anim::send_weapon_anim(lua, weapon, act(lua, "ACT_VM_PRIMARYATTACK"));
    crate::anim::animation_event(lua, player, crate::anim::EVENT_ATTACK_PRIMARY, 0);
    let Some((target, point)) = aimed(lua, player, reach) else {
        crate::sound::emit_scaled(lua, "Weapon_PhysCannon.DryFire", weapon, 1.0);
        return;
    };
    let said = crate::call_hook(lua, "GravGunPunt", mlua::MultiValue::from_iter([value(lua, player), value(lua, target)]));
    if matches!(said.front(), Some(mlua::Value::Boolean(false))) {
        crate::sound::emit_scaled(lua, "Weapon_PhysCannon.DryFire", weapon, 1.0);
        return;
    }
    let Some((_, aim)) = crate::player::eye(lua, player) else { return };
    if let Some(body) = crate::physics::body(lua, target) {
        let host = host(lua);
        let mut physics = host.physics.physics.borrow_mut();
        let mass = physics.mass(body).max(1.0);
        let speed = PUNT_SPEED * (LIGHT / mass).min(1.0);
        physics.apply_impulse_at(body, crate::physics::array(aim * (speed * mass)), crate::physics::array(point));
        physics.wake(body);
    }
    crate::sound::emit_scaled(lua, "Weapon_PhysCannon.Launch", weapon, 1.0);
}

/// The keys while the gun holds something: the attack key launches it, the second lets go.
/// Whether the gun in hand is one (and so had the keys).
pub(crate) fn holding(lua: &Lua, player: EntityHandle, weapon: EntityHandle, pressed: u32) -> bool {
    if !is_gun(lua, weapon) || !crate::pickup::carried_by_gun(lua, player) {
        return false;
    }
    crate::anim::approach_view_model_pose(lua, player, "active", 1.0, 4.0);
    if pressed & IN_ATTACK != 0 {
        crate::pickup::let_go(lua, player, Some(|mass: f32| PUNT_SPEED * (LIGHT / mass.max(1.0)).min(1.0)));
        crate::sound::emit_scaled(lua, "Weapon_PhysCannon.Launch", weapon, 1.0);
        crate::anim::send_weapon_anim(lua, weapon, act(lua, "ACT_VM_PRIMARYATTACK"));
        set_next_fire(lua, weapon, 0, now(lua) + 0.5);
        set_next_fire(lua, weapon, 1, now(lua) + 0.5);
    } else if pressed & IN_ATTACK2 != 0 {
        crate::pickup::let_go(lua, player, None::<fn(f32) -> f32>);
        crate::sound::emit_scaled(lua, "Weapon_PhysCannon.Drop", weapon, 1.0);
        set_next_fire(lua, weapon, 1, now(lua) + 0.5);
    }
    true
}

pub(crate) fn is_gun(lua: &Lua, weapon: EntityHandle) -> bool {
    host(lua).entities.get(weapon).is_some_and(|e| !e.scripted && e.class.eq_ignore_ascii_case(CLASS))
}
