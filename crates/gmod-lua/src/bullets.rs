//! Bullets: `Entity:FireBullets`, as the engine fires them for SWEPs (`weapon_base` shoots
//! with it), NPCs and its own weapons ([`crate::hl2weapons`]).
//!
//! A bullet table (`Src`, `Dir`, `Spread`, `Num`, `Damage`, `Force`, `Distance`, `HullSize`,
//! `Tracer`, `TracerName`, `AmmoType`, `Attacker`, `IgnoreEntity`, `Callback`) first goes
//! through `EntityFireBullets`, which may change it (returning true) or stop it (false). Each
//! of its bullets then flies along the direction turned within the spread, as far as the
//! distance, and hits what a trace for shots hits (`MASK_SHOT`: players and NPCs by their
//! hitboxes, [`crate::hitbox`]). What is hit takes damage of the ammo type's kind (`DMG_BULLET`
//! when none), pushed along the bullet's way; the bullet's `Callback` sees the hit first and
//! may keep the damage (`{ damage = false }`) or the effects from happening. A player's or an
//! NPC's damage goes through `ScalePlayerDamage` or `ScaleNPCDamage` with the hit group first
//! (returning true there spares them; the gamemode's doubles head shots). The surface hit
//! sounds its bullet impact (`bulletimpact` of its surface), and every `Tracer`-th bullet
//! leaves a tracer (`util.Effect`). `PostEntityFireBullets` hears of each.
//!
//! Damage 0 is the ammo type's: what `game.AddAmmoType` gave it, or for the engine's own the
//! skill settings (`sk_plr_dmg_pistol`...) - from the game's `cfg/skill.cfg` or, without one,
//! about what Half-Life 2 does on normal skill.

use mlua::{Function, Lua, MultiValue, Table, Value};

use crate::damage::{self, DamageInfo};
use crate::effect::EffectData;
use crate::entity::{self, EntityHandle, Kind};
use crate::trace::{self, Filter, Sweep};
use crate::vector::{Angle, Vector};
use crate::{files, host};

/// `DMG_BULLET`.
const DMG_BULLET: i64 = 2;
/// How far a bullet flies when nothing says: the engine's `MAX_TRACE_LENGTH`.
const MAX_DISTANCE: f32 = 56756.0;
/// The impulse a bullet gives what it hits, for each point of damage and of its `Force`: our
/// own choice - a pistol shot (8) nudges a crate of 20 kg at some 30 units a second.
const PUSH_PER_DAMAGE: f32 = 75.0;
/// Where `Tracer` 0 means none, and 1 every bullet.
const DEFAULT_TRACER: i64 = 1;

/// One `FireBullets` call.
#[derive(Clone)]
pub(crate) struct Bullets {
    pub src: Vector,
    pub dir: Vector,
    /// The spread across and up, as the tangent of the angle off the direction.
    pub spread: Vector,
    pub num: u32,
    /// 0: the ammo type's.
    pub damage: f32,
    pub force: f32,
    pub distance: f32,
    pub hull: f32,
    pub tracer: i64,
    pub tracer_name: String,
    pub ammo_type: String,
    pub attacker: EntityHandle,
    pub ignore: EntityHandle,
    pub callback: Option<Function>,
}

impl Bullets {
    /// Bullets from a shooter along `dir`, the rest as the engine has it when Lua says nothing.
    pub fn new(shooter: EntityHandle, src: Vector, dir: Vector) -> Bullets {
        Bullets {
            src,
            dir,
            spread: Vector::default(),
            num: 1,
            damage: 0.0,
            force: 1.0,
            distance: MAX_DISTANCE,
            hull: 0.0,
            tracer: DEFAULT_TRACER,
            tracer_name: "Tracer".into(),
            ammo_type: String::new(),
            attacker: shooter,
            ignore: EntityHandle::NULL,
            callback: None,
        }
    }
}

fn vector_of(t: &Table, key: &str) -> mlua::Result<Option<Vector>> {
    Ok(match t.get::<Value>(key)? {
        Value::UserData(ud) => ud.borrow::<Vector>().ok().map(|v| *v),
        _ => None,
    })
}

/// A bullet table as Lua gives it.
fn from_table(shooter: EntityHandle, t: &Table) -> mlua::Result<Bullets> {
    let number = |key: &str| -> mlua::Result<Option<f64>> { t.get::<Option<f64>>(key) };
    let mut b = Bullets::new(shooter, vector_of(t, "Src")?.unwrap_or_default(), vector_of(t, "Dir")?.unwrap_or(Vector::new(1.0, 0.0, 0.0)));
    b.spread = vector_of(t, "Spread")?.unwrap_or_default();
    b.num = number("Num")?.map_or(1, |n| n.clamp(0.0, 256.0) as u32);
    b.damage = number("Damage")?.unwrap_or(0.0) as f32;
    b.force = number("Force")?.unwrap_or(1.0) as f32;
    b.distance = number("Distance")?.map_or(MAX_DISTANCE, |d| d as f32);
    b.hull = number("HullSize")?.unwrap_or(0.0).max(0.0) as f32;
    b.tracer = number("Tracer")?.map_or(DEFAULT_TRACER, |n| n as i64);
    b.tracer_name = t.get::<Option<String>>("TracerName")?.unwrap_or_else(|| "Tracer".into());
    b.ammo_type = t.get::<Option<String>>("AmmoType")?.unwrap_or_default();
    b.attacker = entity::handle_of(Some(&t.get::<Value>("Attacker")?)).filter(|h| h.index.is_some()).unwrap_or(shooter);
    b.ignore = entity::handle_of(Some(&t.get::<Value>("IgnoreEntity")?)).unwrap_or(EntityHandle::NULL);
    b.callback = t.get::<Option<Function>>("Callback")?;
    Ok(b)
}

/// The bullet table Lua sees in `EntityFireBullets`.
fn to_table(lua: &Lua, b: &Bullets) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.raw_set("Src", entity::vector(lua, b.src)?)?;
    t.raw_set("Dir", entity::vector(lua, b.dir)?)?;
    t.raw_set("Spread", entity::vector(lua, b.spread)?)?;
    t.raw_set("Num", b.num)?;
    t.raw_set("Damage", b.damage)?;
    t.raw_set("Force", b.force)?;
    t.raw_set("Distance", b.distance)?;
    t.raw_set("HullSize", b.hull)?;
    t.raw_set("Tracer", b.tracer)?;
    t.raw_set("TracerName", b.tracer_name.as_str())?;
    t.raw_set("AmmoType", b.ammo_type.as_str())?;
    t.raw_set("Attacker", entity::push(lua, b.attacker)?)?;
    t.raw_set("IgnoreEntity", entity::push(lua, b.ignore)?)?;
    if let Some(f) = &b.callback {
        t.raw_set("Callback", f.clone())?;
    }
    Ok(t)
}

/// A number from -1 to 1, more often near 0: the sum of two even ones, as Source spreads
/// bullets.
fn spread_roll(lua: &Lua) -> f32 {
    let seed = &host(lua).bullets.seed;
    let next = || {
        let mut x = seed.get().max(1);
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        seed.set(x);
        (x >> 11) as f32 / (1u64 << 53) as f32 - 0.5
    };
    next() + next()
}

/// A number from -0.5 to 0.5, evenly: the dice of the spread, for what else is random about a
/// shot (the kick of the view).
pub(crate) fn dice(lua: &Lua) -> f32 {
    spread_roll(lua) * 0.5
}

/// The way one bullet goes: the direction turned off by up to the spread.
fn spread_dir(lua: &Lua, dir: Vector, spread: Vector) -> Vector {
    let dir = if dir.length() > 0.0 { dir.normalized() } else { Vector::new(1.0, 0.0, 0.0) };
    if spread.x == 0.0 && spread.y == 0.0 {
        return dir;
    }
    let (_, right, up) = dir.angle().vectors();
    let (x, y) = (spread_roll(lua), spread_roll(lua));
    (dir + right * (x * spread.x) + up * (y * spread.y)).normalized()
}

/// What the bullets keep between calls: the dice of the spread.
pub(crate) struct State {
    seed: std::cell::Cell<u64>,
}

impl Default for State {
    fn default() -> State {
        State { seed: std::cell::Cell::new(0x2545_F491_4F6C_DD1D) }
    }
}

/// The damage of a bullet of an ammo type when the bullet says none: a player's or an NPC's.
fn ammo_damage(lua: &Lua, ammo: &str, by_player: bool) -> f32 {
    let id = crate::weapon::ammo_id(lua, ammo);
    let given = crate::weapon::ammo_type_of(lua, id).map_or(0.0, |t| if by_player { t.player_damage } else { t.npc_damage });
    if given > 0.0 {
        return given as f32;
    }
    crate::weapon::skill_damage(lua, ammo, by_player)
}

/// `EntityFireBullets` and each bullet. The shooter is who fires; the attacker who is to blame.
pub(crate) fn fire(lua: &Lua, shooter: EntityHandle, bullets: Bullets) {
    let Ok(shooter_value) = entity::push(lua, shooter) else { return };
    // the hook may change the bullets, or keep them from flying
    let b = match to_table(lua, &bullets) {
        Ok(table) => {
            let answer =
                crate::call_hook(lua, "EntityFireBullets", MultiValue::from_iter([shooter_value.clone(), Value::Table(table.clone())]));
            match answer.front() {
                Some(Value::Boolean(false)) => return,
                Some(Value::Boolean(true)) => match from_table(shooter, &table) {
                    Ok(b) => b,
                    Err(e) => {
                        host(lua).report(crate::ScriptError::from_lua(&e));
                        return;
                    }
                },
                _ => bullets,
            }
        }
        Err(_) => bullets,
    };
    let by_player = host(lua).entities.get(b.attacker).is_some_and(|e| e.player.is_some());
    let ammo_name = if b.ammo_type.is_empty() { "Pistol".to_string() } else { b.ammo_type.clone() };
    let ammo_id = crate::weapon::ammo_id(lua, &ammo_name);
    let kind = crate::weapon::ammo_type_of(lua, ammo_id).map_or(DMG_BULLET, |t| t.dmg_type).max(DMG_BULLET);
    let damage = if b.damage > 0.0 { b.damage } else { ammo_damage(lua, &ammo_name, by_player) };
    let weapon = crate::player::active_weapon(lua, b.attacker);
    for i in 0..b.num {
        let dir = spread_dir(lua, b.dir, b.spread);
        let end = b.src + dir * b.distance;
        let half = b.hull * 0.5;
        let sweep = Sweep {
            mins: Vector::new(-half, -half, -half),
            maxs: Vector::new(half, half, half),
            mask: gmod_bsp::collision::mask::SHOT,
            ..Sweep::line(b.src, end, Filter::Entities(vec![shooter, b.ignore]))
        };
        let hit = trace::trace(lua, &sweep);
        let hit_something = hit.fraction < 1.0 || hit.start_solid;
        let Ok(tr) = trace::result(lua, &sweep, &hit, None) else { continue };
        let mut info = DamageInfo::new(damage, kind, b.attacker, shooter);
        info.ammo_type = ammo_id;
        info.weapon = weapon;
        info.position = hit.end;
        info.reported_position = b.src;
        info.force = dir * (damage * b.force * PUSH_PER_DAMAGE);
        let (mut hurts, mut effects) = (true, true);
        let info_ud = lua.create_userdata(info).ok();
        if let (Some(callback), Some(ud)) = (&b.callback, &info_ud) {
            let attacker = entity::push(lua, b.attacker).unwrap_or(Value::Nil);
            let args = MultiValue::from_iter([attacker, Value::Table(tr.clone()), Value::UserData(ud.clone())]);
            if let Ok(answer) = files::call_reporting(lua, callback.clone(), args)
                && let Some(Value::Table(t)) = answer.front()
            {
                hurts = !matches!(t.get::<Value>("damage"), Ok(Value::Boolean(false)));
                effects = !matches!(t.get::<Value>("effects"), Ok(Value::Boolean(false)));
            }
        }
        let info = info_ud.as_ref().and_then(|ud| ud.borrow::<DamageInfo>().ok().map(|i| *i)).unwrap_or(info);
        // a client's bullets, fired as it predicts its weapon, hurt no one: the server's do
        if hit_something && hurts && host(lua).realm == crate::Realm::Server && hit.entity.index.is_some_and(|i| i != 0) {
            strike(lua, hit.entity, hit.hit_group, hit.physics_bone, info);
        }
        if hit_something && effects {
            impact(lua, &sweep, &hit, kind);
        }
        if effects && b.tracer > 0 && (i64::from(i) % b.tracer) == 0 {
            let muzzle = muzzle(lua, shooter).unwrap_or(b.src);
            let data = EffectData { start: muzzle, origin: hit.end, entity: shooter, ..EffectData::default() };
            crate::effect::dispatch(lua, &b.tracer_name, data);
        }
        let report = lua.create_table().and_then(|t| {
            t.raw_set("Trace", tr.clone())?;
            t.raw_set("AmmoType", ammo_name.as_str())?;
            t.raw_set("Tracer", b.tracer)?;
            t.raw_set("TracerName", b.tracer_name.as_str())?;
            t.raw_set("Damage", damage)?;
            t.raw_set("Force", b.force)?;
            t.raw_set("Attacker", entity::push(lua, b.attacker)?)?;
            Ok(t)
        });
        if let Ok(report) = report {
            crate::call_hook(lua, "PostEntityFireBullets", MultiValue::from_iter([shooter_value.clone(), Value::Table(report)]));
        }
    }
}

/// A bullet hits an entity: a player or an NPC through the hook of its hit group first.
fn strike(lua: &Lua, target: EntityHandle, group: i32, part: usize, info: DamageInfo) {
    let kind = host(lua).entities.get(target).map(|e| (e.kind, e.player.is_some()));
    let Some((kind, player)) = kind else { return };
    let Ok(ud) = lua.create_userdata(info) else { return };
    let Ok(target_value) = entity::push(lua, target) else { return };
    let args = MultiValue::from_iter([target_value, Value::Integer(group.into()), Value::UserData(ud.clone())]);
    let spared = if player {
        crate::player::set_last_hit_group(lua, target, group.into());
        crate::call_hook(lua, "ScalePlayerDamage", args)
    } else if matches!(kind, Kind::Npc | Kind::NextBot) {
        crate::call_hook(lua, "ScaleNPCDamage", args)
    } else {
        MultiValue::new()
    };
    if !matches!(spared.front(), None | Some(Value::Nil) | Some(Value::Boolean(false))) {
        return;
    }
    let info = ud.borrow::<DamageInfo>().map(|i| *i).unwrap_or(info);
    // a ragdoll is pushed at the part hit
    if let Some(Some(body)) = crate::ragdoll::part_body(lua, target, part) {
        crate::breakable::push_body(lua, body, &info, true);
    }
    damage::take(lua, target, info);
}

/// The surface a bullet hit sounds its impact, and the clients are told of it.
fn impact(lua: &Lua, sweep: &Sweep, hit: &trace::Hit, kind: i64) {
    if hit.surface_flags & gmod_bsp::surf::SKY != 0 {
        return;
    }
    let (index, prop) = trace::surface_of(lua, hit);
    if !prop.bullet_impact.is_empty() {
        crate::sound::emit_at(lua, &prop.bullet_impact, hit.end);
    }
    let data = EffectData {
        origin: hit.end,
        start: sweep.start,
        normal: hit.normal,
        entity: hit.entity,
        surface_prop: index as i64,
        hit_box: hit.hitbox as i64,
        damage_type: kind,
        ..EffectData::default()
    };
    crate::effect::dispatch(lua, "Impact", data);
}

/// Where a shooter's bullets seem to come from for a tracer: its weapon's muzzle, or its own.
fn muzzle(lua: &Lua, shooter: EntityHandle) -> Option<Vector> {
    let weapon = crate::player::active_weapon(lua, shooter).or_else(|| crate::npc::weapon_of(lua, shooter)).unwrap_or(shooter);
    let rig = crate::anim::rig(lua, &host(lua).entities.get(weapon)?.model)?;
    let attachment = rig.attachments().iter().position(|a| a.name.eq_ignore_ascii_case("muzzle"))?;
    crate::anim::attachment_world(lua, weapon, attachment).map(|(pos, _)| pos)
}

pub(crate) fn install_methods(def: &entity::Def) -> mlua::Result<()> {
    def("FireBullets", |a| {
        let shooter = entity::this(a, 1)?;
        let t = a.table(2)?;
        let bullets = from_table(shooter, &t)?;
        fire(a.lua, shooter, bullets);
        Ok(Value::Nil)
    })?;
    // the force of damage on the body of an entity (a SENT's `OnTakeDamage` passes it on)
    def("TakePhysicsDamage", |a| {
        let handle = entity::this(a, 1)?;
        let info = a.userdata::<DamageInfo>(2, "CTakeDamageInfo")?;
        if let Some(body) = crate::physics::body(a.lua, handle) {
            crate::breakable::push_body(a.lua, body, &info, true);
        }
        Ok(Value::Nil)
    })?;
    Ok(())
}

/// Bullets a player fires from the eyes where they look, with the bullet table's way left out.
pub(crate) fn from_eyes(lua: &Lua, player: EntityHandle) -> Option<Bullets> {
    let (eye, aim) = crate::player::eye(lua, player)?;
    let angles: Angle = crate::player::eye_angles(lua, player)?;
    let punch = crate::player::state(lua, player, |_, p| p.view_punch).unwrap_or_default();
    // the eyes kicked by recoil aim where they look
    let aim = if punch != Angle::default() { Angle::new(angles.p + punch.p, angles.y + punch.y, angles.r).vectors().0 } else { aim };
    Some(Bullets::new(player, eye, aim))
}
