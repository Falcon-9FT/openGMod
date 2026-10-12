//! What breaks: props whose models say they do, the map's breakable brushes, and the
//! explosions of those that blow up.
//!
//! * A prop of physics (`prop_physics` and its kin) is as strong as its model's `prop_data`
//!   says (its `$keyvalues`): its own `health`, or that of the class of the game's
//!   `scripts/propdata.txt` its `base` names (`Wooden.Medium` ...), with what bullets, clubs
//!   and blasts do to it (`dmg.bullets`, `dmg.club`, `dmg.explosive`) and what it does when it
//!   breaks (`explosive_damage`, `explosive_radius`). A model without `prop_data` takes the
//!   map's `health`; a `prop_physics_override` with one keeps it. A prop of no health does not
//!   break. Damage takes its health (`Entity:Health`), fires `OnHealthChanged` (what is left,
//!   0 to 1), and at nothing it breaks: the `PropBreak(attacker, prop)` hook, its `OnBreak`,
//!   the break sound of what its body is made of (`Wood.Break` ...), the explosion if it has
//!   one (`explosive_damage`, or the map's `ExplodeDamage` and `ExplodeRadius`), and it is gone.
//! * `func_breakable` and `func_breakable_surf` are brushes of the map with `health` (0: not
//!   broken by damage), a `material` (0 glass, 1 wood, 2 metal, 3 flesh, 4 cinder block,
//!   5 ceiling tile, 6 computer, 7 unbreakable glass, 8 rocks) whose break sound they make,
//!   `minhealthdmg` (less damage does nothing), flag 1 (only an input breaks it) and
//!   `explodemagnitude`. Broken, they fire `OnBreak` and go, out of sight and of the way.
//! * Both take `Break`, `SetHealth`, `AddHealth` and `RemoveHealth` (broken at nothing).
//! * An explosion makes the `Explosion` effect (`util.Effect`) and `BaseExplosionEffect.Sound`,
//!   and hurts (`DMG_BLAST`) what is within its radius and not behind the world, less the
//!   farther from its middle, pushing it away: players, NPCs, bodies of physics, solid
//!   scripted entities, other breakables - which may break and blow up in turn.
//!
//! Damage with a force pushes the body of an engine's entity, at the point hit, no faster
//! than 2000 units a second.
//!
//! Not done: gibs (the `.phy`'s `break` blocks, `func_breakable`'s pieces), props that catch
//! fire before they blow up, breaking by touch or pressure (flags 2 and 4), breaking on
//! impacts, `spawnobject`, `prop_dynamic` that break.

use std::cell::OnceCell;

use mlua::{Lua, MultiValue, Value};

use crate::damage::{DamageInfo, dmg};
use crate::entity::{self, EntityHandle, Kind};
use crate::host;
use crate::vector::Vector;

/// The classes of the game's `scripts/propdata.txt`, read when first asked for.
#[derive(Default)]
pub(crate) struct Props {
    classes: OnceCell<gmod_kv::Block>,
}

/// What a model says of breaking it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct PropData {
    health: f32,
    bullets: f32,
    club: f32,
    explosive: f32,
    explode_damage: f32,
    explode_radius: f32,
}

impl Default for PropData {
    fn default() -> PropData {
        PropData { health: 0.0, bullets: 1.0, club: 1.0, explosive: 1.0, explode_damage: 0.0, explode_radius: 0.0 }
    }
}

const DMG_CLUB: i64 = 128;
const DMG_BULLETS: i64 = dmg::BULLET | dmg::BUCKSHOT | dmg::SNIPER;
/// `func_breakable`'s flag: only an input breaks it.
const ONLY_TRIGGER: i64 = 1;
const UNBREAKABLE_GLASS: i64 = 7;
/// How far an explosion reaches when nothing says, for each point of its damage: our own
/// choice, a few times as far as it hurts most.
const REACH_PER_DAMAGE: f32 = 2.5;
/// The impulse a point of blast damage gives what it hits: our own choice, such that a blast
/// of 100 throws a crate of 20 kg away at some 1000 units a second.
const PUSH: f32 = 200.0;
/// The most speed a push of damage gives a body: about as fast as the game lets one go, so
/// that a light one is not shot through walls.
const MAX_SPEED: f32 = 2000.0;

fn is_prop(class: &str) -> bool {
    class.to_ascii_lowercase().starts_with("prop_physics")
}

fn is_brush(class: &str) -> bool {
    class.eq_ignore_ascii_case("func_breakable") || class.eq_ignore_ascii_case("func_breakable_surf")
}

/// The class of an engine's entity, "" for a scripted one.
fn class_of(lua: &Lua, handle: EntityHandle) -> String {
    host(lua).entities.get(handle).filter(|ent| !ent.scripted).map(|ent| ent.class.clone()).unwrap_or_default()
}

/// Removed, it has broken already (or is about to go): it takes no more damage, and does not
/// break again - as when barrels in a pile blow each other up.
fn gone(lua: &Lua, handle: EntityHandle) -> bool {
    host(lua).entities.get(handle).is_none_or(|ent| ent.removed)
}

fn key(lua: &Lua, handle: EntityHandle, key: &str) -> f32 {
    host(lua).entities.get(handle).and_then(|ent| ent.keyvalue(key).map(|v| crate::convar::atof(v) as f32)).unwrap_or(0.0)
}

/// A class of `propdata.txt`: at its top, or in the block that holds them all.
fn prop_class(lua: &Lua, name: &str) -> Option<gmod_kv::Block> {
    let host = host(lua);
    let vfs = host.vfs.clone();
    let root = host.props.classes.get_or_init(|| {
        let Ok(bytes) = vfs.read("scripts/propdata.txt", "GAME") else { return gmod_kv::Block::default() };
        gmod_kv::parse_lenient(&gmod_kv::decode_text(&bytes), gmod_kv::Options::default()).0.root
    });
    root.block(name).or_else(|| root.iter().filter_map(|p| p.value.as_block()).find_map(|b| b.block(name))).cloned()
}

/// A model's `prop_data`, over the class it names. The compiler wraps a model's `$keyvalues`
/// in a block `mdlkeyvalue`; one written at the top is taken too.
fn prop_data(lua: &Lua, model: &str) -> Option<PropData> {
    let text = crate::ents::studio(lua, model)?.key_values;
    if text.is_empty() {
        return None;
    }
    let (document, _) = gmod_kv::parse_lenient(&text, gmod_kv::Options::default());
    let root = &document.root;
    let own = root.block("mdlkeyvalue").and_then(|b| b.block("prop_data")).or_else(|| root.block("prop_data"))?;
    let base = own.str("base").and_then(|name| prop_class(lua, name));
    let get = |key: &str, default: f32| {
        own.str(key).or_else(|| base.as_ref().and_then(|b| b.str(key))).map_or(default, |v| crate::convar::atof(v) as f32)
    };
    let d = PropData::default();
    Some(PropData {
        health: get("health", d.health),
        bullets: get("dmg.bullets", d.bullets),
        club: get("dmg.club", d.club),
        explosive: get("dmg.explosive", d.explosive),
        explode_damage: get("explosive_damage", d.explode_damage),
        explode_radius: get("explosive_radius", d.explode_radius),
    })
}

/// How much a prop takes before it breaks (0: it does not), and what its model says.
fn prop_health(lua: &Lua, handle: EntityHandle) -> (i64, PropData) {
    let Some((model, class)) = host(lua).entities.get(handle).map(|ent| (ent.model.clone(), ent.class.clone())) else {
        return (0, PropData::default());
    };
    let from_map = key(lua, handle, "health") as i64;
    match prop_data(lua, &model) {
        Some(data) if !(class.eq_ignore_ascii_case("prop_physics_override") && from_map > 0) => (data.health as i64, data),
        data => (from_map, data.unwrap_or_default()),
    }
}

/// A spawned prop takes the health its model gives it; a breakable brush keeps the map's as
/// its most.
pub(crate) fn spawned(lua: &Lua, handle: EntityHandle) {
    let class = class_of(lua, handle);
    if is_prop(&class) {
        let (health, _) = prop_health(lua, handle);
        host(lua).entities.with_mut(handle, |ent| (ent.health, ent.max_health) = (health, health));
    } else if is_brush(&class) {
        host(lua).entities.with_mut(handle, |ent| ent.max_health = ent.max_health.max(ent.health));
    }
}

/// Damage to an engine's entity: it is pushed, and a breakable loses health.
pub(crate) fn damaged(lua: &Lua, handle: EntityHandle, info: &DamageInfo) {
    if gone(lua, handle) {
        return;
    }
    push(lua, handle, info);
    let class = class_of(lua, handle);
    let amount = if is_prop(&class) {
        let (most, data) = prop_health(lua, handle);
        if most <= 0 {
            return;
        }
        let scale = if info.kind & dmg::BLAST != 0 {
            data.explosive
        } else if info.kind & DMG_BULLETS != 0 {
            data.bullets
        } else if info.kind & DMG_CLUB != 0 {
            data.club
        } else {
            1.0
        };
        info.damage * scale
    } else if is_brush(&class) {
        let (flags, health) = host(lua).entities.get(handle).map_or((0, 0), |ent| (ent.spawn_flags, ent.health));
        let material = key(lua, handle, "material") as i64;
        if flags & ONLY_TRIGGER != 0 || material == UNBREAKABLE_GLASS || health <= 0 || info.damage < key(lua, handle, "minhealthdmg") {
            return;
        }
        info.damage
    } else {
        return;
    };
    hurt(lua, handle, amount, info.attacker.unwrap_or(EntityHandle::NULL));
}

/// The force of damage pushes a body, at the point hit when it is on it.
fn push(lua: &Lua, handle: EntityHandle, info: &DamageInfo) {
    if info.force.length() <= 0.0 {
        return;
    }
    let Some(id) = crate::physics::body(lua, handle) else { return };
    let on = host(lua).entities.world_bounds(handle).is_some_and(|(_, _, lo, hi)| inside(info.position, lo, hi));
    let host = host(lua);
    let mut physics = host.physics.physics.borrow_mut();
    let most = physics.mass(id) * MAX_SPEED;
    let force = if info.force.length() > most { info.force.normalized() * most } else { info.force };
    let force = crate::physics::array(force);
    if on {
        physics.apply_impulse_at(id, force, crate::physics::array(info.position));
    } else {
        physics.apply_impulse(id, force);
    }
    physics.wake(id);
}

fn inside(p: Vector, lo: Vector, hi: Vector) -> bool {
    p.x >= lo.x && p.x <= hi.x && p.y >= lo.y && p.y <= hi.y && p.z >= lo.z && p.z <= hi.z
}

/// Takes health; at nothing it breaks.
fn hurt(lua: &Lua, handle: EntityHandle, amount: f32, attacker: EntityHandle) {
    if amount <= 0.0 {
        return;
    }
    let Some(health) = host(lua).entities.with_mut(handle, |ent| {
        ent.health = (ent.health as f32 - amount) as i64;
        ent.health
    }) else {
        return;
    };
    health_changed(lua, handle, attacker);
    if health <= 0 {
        shatter(lua, handle, attacker);
    }
}

/// `OnHealthChanged`, with what is left of its most.
fn health_changed(lua: &Lua, handle: EntityHandle, activator: EntityHandle) {
    let (health, most) = host(lua).entities.get(handle).map_or((0, 0), |ent| (ent.health, ent.max_health));
    let left = if most > 0 { health.clamp(0, most) as f64 / most as f64 } else { 0.0 };
    crate::outputs::fire(lua, handle, "OnHealthChanged", activator, handle, Some(&left.to_string()));
}

/// An input to a breakable. Whether it took it.
pub(crate) fn input(lua: &Lua, handle: EntityHandle, input: &str, param: &str, activator: EntityHandle) -> bool {
    let class = class_of(lua, handle);
    if !is_prop(&class) && !is_brush(&class) {
        return false;
    }
    if gone(lua, handle) {
        return true;
    }
    let n = crate::convar::atof(param) as i64;
    let change = |f: &dyn Fn(i64) -> i64| {
        let Some(health) = host(lua).entities.with_mut(handle, |ent| {
            ent.health = f(ent.health);
            ent.max_health = ent.max_health.max(ent.health);
            ent.health
        }) else {
            return;
        };
        health_changed(lua, handle, activator);
        if health <= 0 {
            shatter(lua, handle, activator);
        }
    };
    match input.to_ascii_lowercase().as_str() {
        "break" => shatter(lua, handle, activator),
        "sethealth" => change(&|_| n),
        "addhealth" => change(&|h| h.saturating_add(n)),
        "removehealth" => change(&|h| h.saturating_sub(n)),
        _ => return false,
    }
    true
}

/// The middle of an entity's box in the world.
fn middle(lua: &Lua, handle: EntityHandle) -> Option<Vector> {
    let (lo, hi) = bounds(lua, handle)?;
    Some((lo + hi) * 0.5)
}

fn bounds(lua: &Lua, handle: EntityHandle) -> Option<(Vector, Vector)> {
    crate::ragdoll::world_bounds(lua, handle).or_else(|| host(lua).entities.world_bounds(handle).map(|(_, _, lo, hi)| (lo, hi)))
}

/// The surface a breakable brush's `material` is.
fn brush_surface(material: i64) -> &'static str {
    match material {
        0 | 7 => "glass",
        1 => "wood",
        2 => "metal",
        3 => "flesh",
        4 => "concrete",
        5 => "ceiling_tile",
        6 => "computer",
        8 => "rock",
        _ => "default",
    }
}

/// It breaks: told, heard, gone - and blown up, if it does that.
fn shatter(lua: &Lua, handle: EntityHandle, attacker: EntityHandle) {
    if gone(lua, handle) {
        return;
    }
    let class = class_of(lua, handle);
    let prop = is_prop(&class);
    let center = middle(lua, handle).unwrap_or_default();
    host(lua).entities.with_mut(handle, |ent| ent.health = 0);
    if prop {
        let push = |h: EntityHandle| entity::push(lua, h).unwrap_or(Value::Nil);
        crate::call_hook(lua, "PropBreak", MultiValue::from_iter([push(attacker), push(handle)]));
    }
    crate::outputs::fire(lua, handle, "OnBreak", attacker, handle, None);
    let surface = if prop {
        crate::physics::material_of(lua, handle).unwrap_or_default()
    } else if class.eq_ignore_ascii_case("func_breakable_surf") {
        "glass".to_string()
    } else {
        brush_surface(key(lua, handle, "material") as i64).to_string()
    };
    let sound = crate::surfaceprops::named(lua, &surface).1.break_sound;
    if !sound.is_empty() {
        crate::sound::emit_at(lua, &sound, center);
    }
    let (damage, radius) = if prop {
        let (_, data) = prop_health(lua, handle);
        let from_map = key(lua, handle, "ExplodeDamage");
        if from_map > 0.0 { (from_map, key(lua, handle, "ExplodeRadius")) } else { (data.explode_damage, data.explode_radius) }
    } else {
        (key(lua, handle, "explodemagnitude"), 0.0)
    };
    crate::ents::remove(lua, handle);
    if damage > 0.0 {
        let radius = if radius > 0.0 { radius } else { damage * REACH_PER_DAMAGE };
        explode(lua, center, damage, radius, attacker, handle);
    }
}

/// The brush model an entity has, if it is a brush.
fn brush_model(lua: &Lua, handle: EntityHandle) -> Option<usize> {
    host(lua).entities.get(handle)?.model.strip_prefix('*')?.parse().ok()
}

/// What an explosion may hurt: players alive, NPCs, bodies of physics, solid scripted entities
/// and breakables - each with its box.
fn victims(lua: &Lua, inflictor: EntityHandle) -> Vec<(EntityHandle, Vector, Vector)> {
    let handles = host(lua).entities.handles();
    let mut out = Vec::new();
    for handle in handles {
        if handle.index.is_none_or(|i| i == 0) || handle == inflictor {
            continue;
        }
        let Some((scripted, solid, class, carried)) = host(lua).entities.get(handle).and_then(|ent| {
            let gone = ent.removed || !ent.spawned || ent.trigger;
            (!gone).then(|| (ent.scripted, ent.solid != 0, ent.class.clone(), ent.owner.index.is_some() && handle.kind == Kind::Weapon))
        }) else {
            continue;
        };
        let hurts = match handle.kind {
            Kind::Player => crate::player::alive(lua, handle),
            Kind::Npc | Kind::NextBot => true,
            _ => {
                !carried
                    && (crate::physics::body(lua, handle).is_some()
                        || crate::ragdoll::is_ragdoll(lua, handle)
                        || scripted && solid
                        || !scripted && (is_prop(&class) || is_brush(&class)))
            }
        };
        if let Some((lo, hi)) = bounds(lua, handle).filter(|_| hurts) {
            out.push((handle, lo, hi));
        }
    }
    out
}

/// An explosion at `center`: seen, heard, and felt within `radius` - `damage` at its middle,
/// nothing at its edge.
pub(crate) fn explode(lua: &Lua, center: Vector, damage: f32, radius: f32, attacker: EntityHandle, inflictor: EntityHandle) {
    let effect = crate::effect::EffectData {
        origin: center,
        start: center,
        magnitude: damage,
        radius,
        scale: 1.0,
        ..crate::effect::EffectData::default()
    };
    crate::effect::dispatch(lua, "Explosion", effect);
    crate::sound::emit_at(lua, "BaseExplosionEffect.Sound", center);
    let attacker = if attacker.index.is_some() { attacker } else { inflictor };
    let collision = host(lua).world.collision.borrow().clone();
    // the world between the middle and a victim shields it; the brushes of what blows up and
    // of the victim itself do not
    let own = brush_model(lua, inflictor);
    for (victim, lo, hi) in victims(lua, inflictor) {
        let nearest = Vector::new(center.x.clamp(lo.x, hi.x), center.y.clamp(lo.y, hi.y), center.z.clamp(lo.z, hi.z));
        let distance = (nearest - center).length();
        // what an explosion before broke is gone
        if distance >= radius || gone(lua, victim) {
            continue;
        }
        let target = (lo + hi) * 0.5;
        if let Some(collision) = collision.as_deref() {
            let skip: Vec<usize> = own.into_iter().chain(brush_model(lua, victim)).collect();
            let (a, b) = (crate::physics::array(center), crate::physics::array(target));
            let t = collision.trace_skipping(a, b, [0.0; 3], [0.0; 3], gmod_bsp::mask::SOLID_BRUSHONLY, &skip);
            if t.fraction < 1.0 && !t.start_solid {
                continue;
            }
        }
        let amount = damage * (1.0 - distance / radius);
        let away = target - center;
        let direction = if away.length() > 0.001 { away.normalized() } else { Vector::new(0.0, 0.0, 1.0) };
        let mut info = DamageInfo::new(amount, dmg::BLAST, attacker, inflictor);
        info.force = direction * (amount * PUSH);
        info.position = center;
        info.reported_position = center;
        crate::damage::take(lua, victim, info);
    }
}
