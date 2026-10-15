//! What Half-Life 2's weapons fire besides bullets: the frag grenade (`npc_grenade_frag`), which
//! bounces as a body of physics and goes off when its fuse has burnt; the RPG's rocket
//! (`rpg_missile`), which speeds up and turns to where its owner aims; the crossbow's bolt
//! (`crossbow_bolt`), fast and hardly falling, which sticks where it hits the world; the
//! SMG's grenade (`grenade_ar2`), which falls and goes off at what it hits; and the AR2's ball
//! (`prop_combine_ball`), which bounces off the world and dissolves the living it meets.
//!
//! All but the grenade fly here, tick by tick, along a sweep from where they were to where they
//! go: what that meets is what they hit. Their damage is the game's skill settings'.

use std::cell::RefCell;

use mlua::Lua;

use crate::damage::{self, DamageInfo};
use crate::entity::{self, EntityHandle};
use crate::host;
use crate::trace::{self, Filter, Sweep};
use crate::vector::{Angle, Vector};

const DMG_BULLET: i64 = 2;
const DMG_NEVERGIB: i64 = 4096;
const DMG_DISSOLVE: i64 = 67108864;
const GRAVITY: f32 = 600.0;

/// What a projectile is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Kind {
    Frag,
    Rocket,
    Bolt,
    SmgGrenade,
    Ball,
}

/// A projectile in flight.
#[derive(Debug, Clone, Copy)]
struct Flying {
    entity: EntityHandle,
    owner: EntityHandle,
    weapon: EntityHandle,
    kind: Kind,
    velocity: Vector,
    /// When it goes off or goes away.
    ends: f64,
    /// Stuck in the world (a bolt): it only waits to go.
    stuck: bool,
}

/// The projectiles in flight.
#[derive(Default)]
pub(crate) struct Projectiles {
    flying: RefCell<Vec<Flying>>,
}

fn now(lua: &Lua) -> f64 {
    host(lua).clock.cur_time.get()
}

fn skill(lua: &Lua, name: &str, default: f32) -> f32 {
    crate::weapon::skill(lua, name).unwrap_or(default)
}

/// Makes the entity of a projectile, the owner's, where and how it starts.
fn make(lua: &Lua, class: &str, model: &str, owner: EntityHandle, pos: Vector, angles: Angle) -> Option<EntityHandle> {
    let ent = crate::ents::create(lua, class, None).ok()??;
    entity::set_model(lua, ent, model);
    entity::set_world_pos(lua, ent, pos);
    entity::set_world_angles(lua, ent, angles);
    host(lua).entities.with_mut(ent, |e| {
        e.owner = owner;
        e.collision_group = crate::trace::group::PROJECTILE;
    });
    crate::ents::spawn(lua, ent);
    host(lua).entities.is_valid(ent).then_some(ent)
}

/// Fires a projectile of a kind from `pos` at `velocity`: its entity, if it could be made.
pub(crate) fn launch(
    lua: &Lua,
    kind: Kind,
    owner: EntityHandle,
    weapon: EntityHandle,
    pos: Vector,
    velocity: Vector,
) -> Option<EntityHandle> {
    let angles = velocity.angle();
    let (class, model) = match kind {
        Kind::Frag => ("npc_grenade_frag", "models/weapons/w_grenade.mdl"),
        Kind::Rocket => ("rpg_missile", "models/weapons/w_missile_launch.mdl"),
        Kind::Bolt => ("crossbow_bolt", "models/crossbow_bolt.mdl"),
        Kind::SmgGrenade => ("grenade_ar2", "models/items/ar2_grenade.mdl"),
        Kind::Ball => ("prop_combine_ball", "models/effects/combineball.mdl"),
    };
    let ent = make(lua, class, model, owner, pos, angles)?;
    let lasts = match kind {
        // the fuse
        Kind::Frag => 3.0,
        Kind::Ball => 4.0,
        _ => 10.0,
    };
    if kind == Kind::Frag {
        // a body of physics that bounces: thrown, spinning
        if let Some(body) = crate::physics::body(lua, ent) {
            let host = host(lua);
            let mut physics = host.physics.physics.borrow_mut();
            physics.set_velocity(body, crate::physics::array(velocity));
            physics.set_angular_velocity(body, [600.0, 0.0, 0.0]);
            physics.wake(body);
        }
    } else {
        host(lua).entities.with_mut(ent, |e| e.velocity = velocity);
    }
    if kind == Kind::Rocket {
        crate::sound::emit_scaled(lua, "Missile.Ignite", ent, 1.0);
    }
    host(lua).projectiles.flying.borrow_mut().push(Flying {
        entity: ent,
        owner,
        weapon,
        kind,
        velocity,
        ends: now(lua) + lasts,
        stuck: false,
    });
    Some(ent)
}

/// Whether a player's rocket is still in flight (an RPG fires one at a time).
pub(crate) fn rocket_flying(lua: &Lua, owner: EntityHandle) -> bool {
    host(lua)
        .projectiles
        .flying
        .borrow()
        .iter()
        .any(|f| f.owner == owner && f.kind == Kind::Rocket && host(lua).entities.is_valid(f.entity))
}

/// A tick of everything in flight.
pub(crate) fn think(lua: &Lua) {
    let all: Vec<Flying> = std::mem::take(&mut *host(lua).projectiles.flying.borrow_mut());
    let mut kept = Vec::with_capacity(all.len());
    for f in all {
        if !host(lua).entities.is_valid(f.entity) {
            continue;
        }
        if let Some(f) = step(lua, f) {
            kept.push(f);
        }
    }
    host(lua).projectiles.flying.borrow_mut().extend(kept);
}

/// One tick of a projectile: where it is still, or none once it is gone.
fn step(lua: &Lua, mut f: Flying) -> Option<Flying> {
    let dt = crate::timer::TICK_INTERVAL as f32;
    let (pos, _) = host(lua).entities.transform(f.entity)?;
    if now(lua) >= f.ends {
        match f.kind {
            Kind::Frag => {
                let damage = skill(lua, "sk_plr_dmg_fraggrenade", 125.0);
                let radius = skill(lua, "sk_fraggrenade_radius", 250.0);
                crate::breakable::explode(lua, pos, damage, radius, f.owner, f.entity);
            }
            Kind::Ball => {
                crate::sound::emit_at(lua, "NPC_CombineBall.Explosion", pos);
            }
            _ => {}
        }
        crate::ents::remove(lua, f.entity);
        return None;
    }
    if f.kind == Kind::Frag || f.stuck {
        return Some(f);
    }
    match f.kind {
        Kind::Rocket => {
            // it speeds up, and turns to where its owner aims (the laser's dot)
            let speed = (f.velocity.length() + 1500.0 * dt).min(1500.0);
            let mut heading = f.velocity.normalized();
            if let Some((eye, aim)) = crate::player::eye(lua, f.owner).filter(|_| crate::player::alive(lua, f.owner)) {
                let sweep =
                    Sweep { mask: gmod_bsp::mask::SHOT, ..Sweep::line(eye, eye + aim * 8192.0, Filter::Entities(vec![f.owner, f.entity])) };
                let dot = trace::trace(lua, &sweep).end;
                let wanted = (dot - pos).normalized();
                heading = (heading + (wanted - heading) * (4.0 * dt).min(1.0)).normalized();
            }
            f.velocity = heading * speed;
        }
        Kind::SmgGrenade => f.velocity.z -= GRAVITY * dt,
        Kind::Bolt => f.velocity.z -= GRAVITY * 0.05 * dt,
        _ => {}
    }
    let next = pos + f.velocity * dt;
    let size = if f.kind == Kind::Bolt { 0.0 } else { 3.0 };
    let half = Vector::new(size, size, size);
    let sweep = Sweep {
        mins: half * -1.0,
        maxs: half,
        mask: gmod_bsp::mask::SHOT,
        ..Sweep::line(pos, next, Filter::Entities(vec![f.owner, f.entity]))
    };
    let hit = trace::trace(lua, &sweep);
    if hit.fraction >= 1.0 {
        entity::set_world_pos(lua, f.entity, next);
        if f.kind != Kind::Ball {
            entity::set_world_angles(lua, f.entity, f.velocity.angle());
        }
        host(lua).entities.with_mut(f.entity, |e| e.velocity = f.velocity);
        return Some(f);
    }
    struck(lua, f, &hit)
}

/// A projectile meets something: what it does to it.
fn struck(lua: &Lua, mut f: Flying, hit: &trace::Hit) -> Option<Flying> {
    let world = hit.entity == EntityHandle::WORLD || hit.entity.index.is_none();
    let direction = f.velocity.normalized();
    match f.kind {
        Kind::Rocket | Kind::SmgGrenade => {
            let (damage, radius) = if f.kind == Kind::Rocket {
                (skill(lua, "sk_plr_dmg_rpg_round", 100.0), 250.0)
            } else {
                (skill(lua, "sk_plr_dmg_smg1_grenade", 100.0), 250.0)
            };
            // just before what it hit, so that the world does not hide the blast from it
            crate::breakable::explode(lua, hit.end - direction * 4.0, damage, radius, f.owner, f.entity);
            crate::ents::remove(lua, f.entity);
            None
        }
        Kind::Bolt => {
            if world {
                // it sticks where it hit, and goes in a while
                entity::set_world_pos(lua, f.entity, hit.end);
                host(lua).entities.with_mut(f.entity, |e| e.velocity = Vector::default());
                crate::sound::emit_at(lua, "Weapon_Crossbow.BoltHitWorld", hit.end);
                f.stuck = true;
                f.ends = now(lua) + 10.0;
                return Some(f);
            }
            let damage = crate::weapon::skill_damage(lua, "XBowBolt", true);
            let mut info = DamageInfo::new(damage, DMG_BULLET | DMG_NEVERGIB, f.owner, f.entity);
            info.weapon = Some(f.weapon).filter(|w| host(lua).entities.is_valid(*w));
            info.position = hit.end;
            info.force = direction * (damage * 50.0);
            damage::take(lua, hit.entity, info);
            crate::sound::emit_at(lua, "Weapon_Crossbow.BoltHitBody", hit.end);
            crate::ents::remove(lua, f.entity);
            None
        }
        Kind::Ball => {
            let living = host(lua).entities.get(hit.entity).is_some_and(|e| e.player.is_some() || e.npc.is_some());
            if living {
                let mut info = DamageInfo::new(1000.0, DMG_DISSOLVE, f.owner, f.entity);
                info.position = hit.end;
                damage::take(lua, hit.entity, info);
            } else if let Some(body) = crate::physics::body(lua, hit.entity) {
                let host = host(lua);
                let mut physics = host.physics.physics.borrow_mut();
                let mass = physics.mass(body);
                physics.apply_impulse(body, crate::physics::array(direction * (mass * 600.0)));
                physics.wake(body);
            }
            // off the surface, as fast
            let n = hit.normal;
            f.velocity = f.velocity - n * (2.0 * f.velocity.dot(n));
            crate::sound::emit_at(lua, "NPC_CombineBall.Impact", hit.end);
            entity::set_world_pos(lua, f.entity, hit.end + n * 4.0);
            Some(f)
        }
        Kind::Frag => Some(f),
    }
}
