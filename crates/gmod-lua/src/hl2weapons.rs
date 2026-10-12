//! The engine's own weapons, which GMod has in C++ from Half-Life 2: the guns fire bullets
//! ([`crate::bullets`]) and the melee weapons swing.
//!
//! A gun fires while its key is down and its next shot has come - the pistol, the .357 and the
//! shotgun once a press - a round from the clip (from the ammo carried for one without a clip):
//! the view model plays `ACT_VM_PRIMARYATTACK`, the player's model its attack, the gun's sound
//! (`single_shot` of its script's `SoundData`) goes off, the bullets fly from the eyes within
//! the gun's cone, of its ammo type's damage, and the view kicks up. An empty clip clicks
//! (`empty`) and reloads when there are rounds; the reload key reloads (`ACT_VM_RELOAD`, the
//! clip filling when that has played). A melee weapon swings at what is before the eyes, a
//! line and then a box: what it hits takes club damage (`ACT_VM_HITCENTER`, `melee_hit`),
//! a miss plays `ACT_VM_MISSCENTER` and `single_shot`.
//!
//! How often each fires, how far its bullets spread and how hard it kicks are our own
//! choices, near what the game does. The secondary fire of the SMG, the AR2 and the shotgun,
//! the crossbow, the RPG, grenades and the gravity gun are not here yet.

use mlua::{Lua, Value};

use crate::bullets;
use crate::damage::{self, DamageInfo};
use crate::entity::EntityHandle;
use crate::host;
use crate::trace::{self, Filter, Sweep};
use crate::vector::{Angle, Vector};

const IN_ATTACK: u32 = 1;
const IN_RELOAD: u32 = 8192;
const DMG_CLUB: i64 = 128;

/// A gun of the engine.
struct Gun {
    class: &'static str,
    /// Seconds between shots.
    interval: f64,
    /// Fires while the key is held.
    automatic: bool,
    /// The cone its bullets go in, in degrees across.
    cone: f32,
    pellets: u32,
    /// How far the view kicks up, and at most to the side, in degrees.
    kick: (f32, f32),
    /// Every how many bullets leave a tracer.
    tracer: i64,
}

const GUNS: &[Gun] = &[
    Gun { class: "weapon_pistol", interval: 0.1, automatic: false, cone: 1.0, pellets: 1, kick: (0.5, 0.6), tracer: 2 },
    Gun { class: "weapon_smg1", interval: 0.075, automatic: true, cone: 5.0, pellets: 1, kick: (0.4, 0.2), tracer: 2 },
    Gun { class: "weapon_ar2", interval: 0.1, automatic: true, cone: 3.0, pellets: 1, kick: (0.8, 0.4), tracer: 1 },
    Gun { class: "weapon_357", interval: 0.75, automatic: false, cone: 0.0, pellets: 1, kick: (8.0, 2.0), tracer: 1 },
    Gun { class: "weapon_shotgun", interval: 0.7, automatic: false, cone: 10.0, pellets: 7, kick: (4.0, 2.0), tracer: 2 },
    Gun { class: "weapon_alyxgun", interval: 0.1, automatic: true, cone: 2.0, pellets: 1, kick: (0.5, 0.4), tracer: 2 },
    Gun { class: "weapon_annabelle", interval: 0.75, automatic: false, cone: 0.0, pellets: 1, kick: (6.0, 1.0), tracer: 1 },
];

/// A melee weapon of the engine.
struct Melee {
    class: &'static str,
    interval: f64,
    /// How far it reaches from the eyes.
    range: f32,
    /// How hard a hit kicks the view, in degrees.
    kick: f32,
}

const MELEE: &[Melee] = &[
    Melee { class: "weapon_crowbar", interval: 0.4, range: 75.0, kick: 1.0 },
    Melee { class: "weapon_stunstick", interval: 0.8, range: 75.0, kick: 2.0 },
];

/// The box a swing that missed with a line sweeps, half its size.
const SWING_HULL: f32 = 16.0;

/// An activity by its `ACT_` name, as the game's enums have it (-1 without).
fn act(lua: &Lua, name: &str) -> i64 {
    match lua.globals().raw_get::<Value>(name) {
        Ok(Value::Integer(n)) => n,
        Ok(Value::Number(n)) => n as i64,
        _ => -1,
    }
}

fn now(lua: &Lua) -> f64 {
    host(lua).clock.cur_time.get()
}

/// A roll from -1 to 1 for the view's kick, from the bullets' dice.
fn roll(lua: &Lua) -> f32 {
    bullets::dice(lua) * 2.0
}

/// The weapon's state: clip, next fire, reload.
fn state(lua: &Lua, weapon: EntityHandle) -> Option<crate::weapon::WeaponState> {
    host(lua).entities.get(weapon).and_then(|ent| ent.weapon.clone())
}

fn set_next_fire(lua: &Lua, weapon: EntityHandle, at: f64) {
    host(lua).entities.with_mut(weapon, |ent| {
        if let Some(w) = ent.weapon.as_mut() {
            w.next_fire[0] = at;
            w.last_shoot = now(lua);
        }
    });
}

/// The view of a player kicks: pitch up, yaw to either side.
fn kick(lua: &Lua, player: EntityHandle, up: f32, side: f32) {
    let punch = Angle::new(-up, side, 0.0);
    crate::player::view_punch(lua, player, punch);
}

/// A sound of a weapon's script, from the weapon.
fn sound(lua: &Lua, weapon: EntityHandle, data: &crate::weapon::WeaponData, what: &str) {
    if let Some(name) = data.sound(what) {
        crate::sound::emit_scaled(lua, name, weapon, 1.0);
    }
}

/// A tick of an engine weapon in its owner's hands, the keys held and pressed.
pub(crate) fn frame(lua: &Lua, weapon: EntityHandle, buttons: u32, pressed: u32) {
    let Some(class) = host(lua).entities.get(weapon).map(|e| e.class.to_ascii_lowercase()) else { return };
    let owner = host(lua).entities.get(weapon).map_or(EntityHandle::NULL, |e| e.owner);
    if !host(lua).entities.is_valid(owner) {
        return;
    }
    if let Some(gun) = GUNS.iter().find(|g| g.class == class) {
        if buttons & IN_RELOAD != 0 {
            crate::weapon::default_reload(lua, weapon, act(lua, "ACT_VM_RELOAD"));
        }
        let wants = if gun.automatic { buttons & IN_ATTACK != 0 } else { pressed & IN_ATTACK != 0 };
        if wants && state(lua, weapon).is_some_and(|s| s.next_fire[0] <= now(lua) && s.reload.is_none()) {
            crate::lagcomp::compensated_shot(lua, owner, || fire(lua, owner, weapon, gun));
        }
    } else if let Some(melee) = MELEE.iter().find(|m| m.class == class)
        && buttons & IN_ATTACK != 0
        && state(lua, weapon).is_some_and(|s| s.next_fire[0] <= now(lua))
    {
        crate::lagcomp::compensated_shot(lua, owner, || swing(lua, owner, weapon, melee));
    }
}

/// A gun fires once, if it has a round.
fn fire(lua: &Lua, owner: EntityHandle, weapon: EntityHandle, gun: &Gun) {
    let Some(data) = crate::weapon::data(lua, weapon) else { return };
    let Some(state) = state(lua, weapon) else { return };
    let ammo = crate::weapon::ammo_id(lua, &data.ammo[0]);
    let carried = crate::player::ammo_count(lua, owner, ammo);
    let has_round = if data.clip_size[0] >= 0 { state.clip[0] > 0 } else { carried > 0 };
    if !has_round {
        // a click, and the clip refilled if there is anything to fill it with
        sound(lua, weapon, &data, "empty");
        set_next_fire(lua, weapon, now(lua) + 0.3);
        if data.clip_size[0] >= 0 && carried > 0 {
            crate::weapon::default_reload(lua, weapon, act(lua, "ACT_VM_RELOAD"));
        }
        return;
    }
    if data.clip_size[0] >= 0 {
        host(lua).entities.with_mut(weapon, |ent| {
            if let Some(w) = ent.weapon.as_mut() {
                w.clip[0] -= 1;
            }
        });
    } else {
        crate::player::remove_ammo(lua, owner, ammo, 1);
    }
    crate::anim::send_weapon_anim(lua, weapon, act(lua, "ACT_VM_PRIMARYATTACK"));
    crate::anim::animation_event(lua, owner, crate::anim::EVENT_ATTACK_PRIMARY, 0);
    sound(lua, weapon, &data, "single_shot");
    if let Some(mut b) = bullets::from_eyes(lua, owner) {
        let spread = (gun.cone.to_radians() * 0.5).sin();
        b.spread = Vector::new(spread, spread, 0.0);
        b.num = gun.pellets;
        b.ammo_type = data.ammo[0].clone();
        b.tracer = gun.tracer;
        bullets::fire(lua, owner, b);
    }
    kick(lua, owner, gun.kick.0 * (0.5 + 0.5 * roll(lua).abs()), gun.kick.1 * roll(lua));
    set_next_fire(lua, weapon, now(lua) + gun.interval);
}

/// A melee weapon swings at what is before the eyes.
fn swing(lua: &Lua, owner: EntityHandle, weapon: EntityHandle, melee: &Melee) {
    let Some(data) = crate::weapon::data(lua, weapon) else { return };
    let Some((eye, aim)) = crate::player::eye(lua, owner) else { return };
    let end = eye + aim * melee.range;
    let line = Sweep { mask: gmod_bsp::collision::mask::SHOT_HULL, ..Sweep::line(eye, end, Filter::Entities(vec![owner])) };
    let mut hit = trace::trace(lua, &line);
    if hit.fraction >= 1.0 {
        // a line that missed: a box the size of the swing, which finds what is beside it
        let h = Vector::new(SWING_HULL, SWING_HULL, SWING_HULL);
        let hull = Sweep {
            mins: h * -1.0,
            maxs: h,
            mask: gmod_bsp::collision::mask::SHOT_HULL,
            ..Sweep::line(eye, end, Filter::Entities(vec![owner]))
        };
        hit = trace::trace(lua, &hull);
    }
    crate::anim::animation_event(lua, owner, crate::anim::EVENT_ATTACK_PRIMARY, 0);
    if hit.fraction < 1.0 {
        crate::anim::send_weapon_anim(lua, weapon, act(lua, "ACT_VM_HITCENTER"));
        let world = hit.entity == EntityHandle::WORLD;
        sound(lua, weapon, &data, if world { "melee_hit_world" } else { "melee_hit" });
        if world && data.sound("melee_hit_world").is_none() {
            sound(lua, weapon, &data, "melee_hit");
        }
        if hit.entity.index.is_some_and(|i| i != 0) {
            let damage = crate::weapon::skill_damage(lua, &data_class(lua, weapon), true);
            let mut info = DamageInfo::new(damage, DMG_CLUB, owner, weapon);
            info.weapon = Some(weapon);
            info.position = hit.end;
            info.force = aim * (damage * 75.0);
            damage::take(lua, hit.entity, info);
        }
        let (_, prop) = trace::surface_of(lua, &hit);
        if !prop.impact_hard.is_empty() && hit.entity == EntityHandle::WORLD {
            crate::sound::emit_at(lua, &prop.impact_hard, hit.end);
        }
        kick(lua, owner, melee.kick, melee.kick * roll(lua));
    } else {
        crate::anim::send_weapon_anim(lua, weapon, act(lua, "ACT_VM_MISSCENTER"));
        sound(lua, weapon, &data, "single_shot");
    }
    set_next_fire(lua, weapon, now(lua) + melee.interval);
}

/// A weapon's class.
fn data_class(lua: &Lua, weapon: EntityHandle) -> String {
    host(lua).entities.get(weapon).map(|e| e.class.to_ascii_lowercase()).unwrap_or_default()
}
