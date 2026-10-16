//! Fire: `Entity:Ignite`, `Extinguish`, `IsOnFire`. What burns hurts itself - and, with a
//! radius, what is near - with burn damage five times a second, as Source's `entityflame`,
//! until its time is up or it is in water; clients see the flames (`FL_ONFIRE`).

use mlua::{Lua, MultiValue, Table, Value};

use crate::args::Args;
use crate::damage::DamageInfo;
use crate::entity::{self, EntityHandle};
use crate::host;
use crate::particles::{self, ParticleState, rand};
use crate::render3d::Material3d;
use crate::vector::Vector;

/// `FL_ONFIRE`.
pub const ON_FIRE: i64 = 268_435_456;
/// `DMG_BURN`.
const BURN: i64 = 8;
/// How often a flame hurts, and how much: to what burns, and to what is near.
const INTERVAL: f64 = 0.2;
const DIRECT: f32 = 1.0;
const NEAR: f32 = 0.8;

/// An entity on fire: until when, how far its flames reach, when they hurt next.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Burning {
    pub until: f64,
    pub radius: f32,
    pub next: f64,
}

fn def(lua: &Lua, table: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<Value>) -> mlua::Result<()> {
    table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
}

fn this(a: &Args) -> mlua::Result<EntityHandle> {
    entity::handle_of(a.get(1))
        .filter(|&h| host(a.lua).entities.is_valid(h))
        .ok_or_else(|| crate::args::error(a.lua, "Tried to use a NULL entity!"))
}

/// Sets an entity on fire for `seconds`, its flames reaching `radius`.
pub(crate) fn ignite(lua: &Lua, handle: EntityHandle, seconds: f64, radius: f32) {
    let now = host(lua).clock.cur_time.get();
    host(lua).entities.with_mut(handle, |ent| {
        ent.flags |= ON_FIRE;
        ent.burning = Some(Burning { until: now + seconds.max(0.0), radius: radius.max(0.0), next: now + INTERVAL });
    });
}

/// Puts an entity's fire out.
pub(crate) fn extinguish(lua: &Lua, handle: EntityHandle) {
    host(lua).entities.with_mut(handle, |ent| {
        ent.flags &= !ON_FIRE;
        ent.burning = None;
    });
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let ent = crate::meta::meta_table(lua, "Entity", crate::meta::type_id::ENTITY)?;
    // `Ignite(length, radius)`: the server's
    def(lua, &ent, "Ignite", |a| {
        let handle = this(a)?;
        let (seconds, radius) = (a.number(2)?, a.opt_number(3)?.unwrap_or(0.0) as f32);
        if host(a.lua).realm == crate::Realm::Server {
            ignite(a.lua, handle, seconds, radius);
        }
        Ok(Value::Nil)
    })?;
    def(lua, &ent, "Extinguish", |a| {
        let handle = this(a)?;
        if host(a.lua).realm == crate::Realm::Server {
            extinguish(a.lua, handle);
        }
        Ok(Value::Nil)
    })?;
    def(lua, &ent, "IsOnFire", |a| {
        let handle = this(a)?;
        Ok(Value::Boolean(host(a.lua).entities.get(handle).is_some_and(|e| e.flags & ON_FIRE != 0)))
    })?;
    Ok(())
}

/// A tick of the server: what burns is hurt, and goes out when its time is up or in water.
pub(crate) fn tick(lua: &Lua) {
    let now = host(lua).clock.cur_time.get();
    let burning: Vec<(EntityHandle, Burning)> = {
        let entities = &host(lua).entities;
        entities
            .handles()
            .into_iter()
            .filter_map(|h| entities.get(h).and_then(|e| e.burning.filter(|_| !e.removed).map(|b| (h, b))))
            .collect()
    };
    for (handle, burn) in burning {
        let wet = crate::player::water_level(lua, handle).is_some_and(|level| level >= 2);
        if now >= burn.until || wet {
            extinguish(lua, handle);
            continue;
        }
        if now < burn.next {
            continue;
        }
        host(lua).entities.with_mut(handle, |ent| {
            if let Some(b) = &mut ent.burning {
                b.next = now + INTERVAL;
            }
        });
        let world = EntityHandle::WORLD;
        crate::damage::take(lua, handle, DamageInfo::new(DIRECT, BURN, world, world));
        if burn.radius > 0.0
            && let Some((center, _)) = host(lua).entities.transform(handle)
        {
            let near: Vec<EntityHandle> = {
                let entities = &host(lua).entities;
                entities
                    .handles()
                    .into_iter()
                    .filter(|&h| h != handle && h != world)
                    .filter(|&h| entities.transform(h).is_some_and(|(pos, _)| (pos - center).length() <= burn.radius))
                    .collect()
            };
            for other in near {
                crate::damage::take(lua, other, DamageInfo::new(NEAR, BURN, world, world));
            }
        }
    }
}

/// A frame of a client: flames rise off what burns, as many as its size calls for.
pub(crate) fn flames(lua: &Lua, seconds: f32) {
    let burning: Vec<(Vector, Vector, Vector)> = {
        let entities = &host(lua).entities;
        entities
            .handles()
            .into_iter()
            .filter_map(|h| {
                let ent = entities.get(h)?;
                if ent.flags & ON_FIRE == 0 || ent.removed || ent.dormant {
                    return None;
                }
                let (pos, _) = entities.transform(h)?;
                Some((pos, ent.mins, ent.maxs))
            })
            .collect()
    };
    for (pos, mins, maxs) in burning {
        let size = maxs - mins;
        let extent = size.length().clamp(16.0, 256.0);
        // a flame for every 4 units across, a second
        let count = (extent / 4.0 * seconds * 6.0).max(1.0) as usize;
        let list = (0..count)
            .map(|_| {
                let at = pos + Vector::new(rand(lua, mins.x, maxs.x), rand(lua, mins.y, maxs.y), rand(lua, mins.z, mins.z + size.z * 0.7));
                let mut p = ParticleState::new(Material3d { name: "effects/fire_cloud1".into(), texture: None, additive: Some(true) }, at);
                p.velocity = Vector::new(rand(lua, -6.0, 6.0), rand(lua, -6.0, 6.0), rand(lua, 30.0, 60.0));
                p.die_time = rand(lua, 0.3, 0.6);
                p.start_size = extent * rand(lua, 0.06, 0.1);
                p.end_size = p.start_size * 0.3;
                p.start_alpha = 220.0;
                p.end_alpha = 0.0;
                p.roll = rand(lua, 0.0, std::f32::consts::TAU);
                p.roll_delta = rand(lua, -2.0, 2.0);
                p
            })
            .collect();
        particles::emit(lua, false, list);
    }
}
