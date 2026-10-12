//! `CTakeDamageInfo`: what a hit does - how much, of what type, by whom and with what -
//! made by `DamageInfo()` and handed to the hooks of damage and death (`DoPlayerDeath`); and
//! taking damage (`Entity:TakeDamageInfo`): the `EntityTakeDamage` hook may stop it or change
//! it, then a player is hurt ([`crate::player`]), a SENT gets `OnTakeDamage`, an engine NPC
//! is hurt ([`crate::npc`]), and the engine's other entities are pushed and broken
//! ([`crate::breakable`]).

use mlua::{AnyUserData, Lua, MultiValue, ObjectLike, UserData, UserDataFields, UserDataMethods, Value};

use crate::args::Args;
use crate::entity::{self, EntityHandle};
use crate::meta::{self, type_id};
use crate::vector::Vector;
use crate::{files, host};

/// `DMG_` types the checks of `CTakeDamageInfo` look for.
pub mod dmg {
    pub const GENERIC: i64 = 0;
    pub const BULLET: i64 = 2;
    pub const FALL: i64 = 32;
    pub const BLAST: i64 = 64;
    pub const BUCKSHOT: i64 = 536870912;
    pub const SNIPER: i64 = 1073741824;
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DamageInfo {
    pub damage: f32,
    pub base_damage: f32,
    pub max_damage: f32,
    pub bonus: f32,
    pub custom: i64,
    pub kind: i64,
    pub ammo_type: i64,
    pub attacker: Option<EntityHandle>,
    pub inflictor: Option<EntityHandle>,
    pub weapon: Option<EntityHandle>,
    pub force: Vector,
    pub position: Vector,
    pub reported_position: Vector,
}

impl DamageInfo {
    /// Damage of `amount` and `kind` by `attacker` with `inflictor`.
    pub fn new(amount: f32, kind: i64, attacker: EntityHandle, inflictor: EntityHandle) -> DamageInfo {
        DamageInfo {
            damage: amount,
            base_damage: amount,
            kind,
            ammo_type: -1,
            attacker: Some(attacker),
            inflictor: Some(inflictor),
            ..DamageInfo::default()
        }
    }
}

impl UserData for DamageInfo {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("MetaName", "CTakeDamageInfo");
        fields.add_meta_field("MetaID", type_id::DAMAGEINFO);
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function("__index", |lua, (_, key): (AnyUserData, Value)| meta::lookup(lua, "CTakeDamageInfo", key));
        methods.add_meta_function("__tostring", |_, _: AnyUserData| Ok("CTakeDamageInfo"));
    }
}

fn info(a: &Args) -> mlua::Result<DamageInfo> {
    a.userdata::<DamageInfo>(1, "CTakeDamageInfo")
}

/// Changes the `CTakeDamageInfo` in argument 1.
fn change(a: &Args, f: impl FnOnce(&mut DamageInfo)) -> mlua::Result<Value> {
    a.change(1, "CTakeDamageInfo", f)
}

fn number(n: f32) -> mlua::Result<Value> {
    Ok(Value::Number(f64::from(n)))
}

fn entity_or_null(lua: &Lua, handle: Option<EntityHandle>) -> mlua::Result<Value> {
    entity::push(lua, handle.unwrap_or(EntityHandle::NULL))
}

/// An entity argument, `NULL` or nil as none.
fn opt_entity(a: &Args, position: usize) -> mlua::Result<Option<EntityHandle>> {
    match a.get(position) {
        None => Ok(None),
        Some(value) => entity::handle_of(Some(value)).map(Some).ok_or_else(|| a.bad(position, "Entity")),
    }
}

pub(crate) fn push(lua: &Lua, info: DamageInfo) -> mlua::Result<Value> {
    Ok(Value::UserData(lua.create_userdata(info)?))
}

/// Damage to an entity.
pub(crate) fn take(lua: &Lua, target: EntityHandle, info: DamageInfo) {
    let Ok(Value::UserData(ent)) = entity::push(lua, target) else { return };
    if !host(lua).entities.is_valid(target) {
        return;
    }
    let Ok(ud) = lua.create_userdata(info) else { return };
    let stopped =
        crate::call_hook(lua, "EntityTakeDamage", MultiValue::from_iter([Value::UserData(ent.clone()), Value::UserData(ud.clone())]));
    if !matches!(stopped.front(), None | Some(Value::Nil) | Some(Value::Boolean(false))) {
        return;
    }
    let info = ud.borrow::<DamageInfo>().map(|i| *i).unwrap_or(info);
    let (player, scripted) = host(lua).entities.get(target).map(|e| (e.player.is_some(), e.scripted)).unwrap_or_default();
    if player {
        crate::player::hurt(lua, target, info, &ud);
    } else if scripted {
        if let Ok(Value::Function(f)) = ent.get::<Value>("OnTakeDamage") {
            let _ = files::call_reporting(lua, f, MultiValue::from_iter([Value::UserData(ent), Value::UserData(ud)]));
        }
    } else if target.kind == entity::Kind::Npc {
        crate::npc::hurt(lua, target, info);
    } else {
        crate::breakable::damaged(lua, target, &info);
    }
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let meta = meta::meta_table(lua, "CTakeDamageInfo", type_id::DAMAGEINFO)?;
    let def = |name: &'static str, f: fn(&Args) -> mlua::Result<Value>| -> mlua::Result<()> {
        meta.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
    };
    lua.globals().raw_set(
        "DamageInfo",
        lua.create_function(|lua, _: MultiValue| push(lua, DamageInfo { ammo_type: -1, ..DamageInfo::default() }))?,
    )?;

    def("GetDamage", |a| number(info(a)?.damage))?;
    def("SetDamage", |a| {
        let n = a.number(2)? as f32;
        change(a, |i| i.damage = n)
    })?;
    def("AddDamage", |a| {
        let n = a.number(2)? as f32;
        change(a, |i| i.damage += n)
    })?;
    def("SubtractDamage", |a| {
        let n = a.number(2)? as f32;
        change(a, |i| i.damage -= n)
    })?;
    def("ScaleDamage", |a| {
        let n = a.number(2)? as f32;
        change(a, |i| i.damage *= n)
    })?;
    def("GetBaseDamage", |a| number(info(a)?.base_damage))?;
    def("SetBaseDamage", |a| {
        let n = a.number(2)? as f32;
        change(a, |i| i.base_damage = n)
    })?;
    def("GetMaxDamage", |a| number(info(a)?.max_damage))?;
    def("SetMaxDamage", |a| {
        let n = a.number(2)? as f32;
        change(a, |i| i.max_damage = n)
    })?;
    def("GetDamageBonus", |a| number(info(a)?.bonus))?;
    def("SetDamageBonus", |a| {
        let n = a.number(2)? as f32;
        change(a, |i| i.bonus = n)
    })?;
    def("GetDamageCustom", |a| Ok(Value::Integer(info(a)?.custom)))?;
    def("SetDamageCustom", |a| {
        let n = a.number(2)? as i64;
        change(a, |i| i.custom = n)
    })?;
    def("GetDamageType", |a| Ok(Value::Integer(info(a)?.kind)))?;
    def("SetDamageType", |a| {
        let n = a.number(2)? as i64;
        change(a, |i| i.kind = n)
    })?;
    def("IsDamageType", |a| {
        let kind = a.number(2)? as i64;
        Ok(Value::Boolean(info(a)?.kind & kind != 0))
    })?;
    def("IsBulletDamage", |a| Ok(Value::Boolean(info(a)?.kind & (dmg::BULLET | dmg::BUCKSHOT | dmg::SNIPER) != 0)))?;
    def("IsExplosionDamage", |a| Ok(Value::Boolean(info(a)?.kind & dmg::BLAST != 0)))?;
    def("IsFallDamage", |a| Ok(Value::Boolean(info(a)?.kind & dmg::FALL != 0)))?;
    def("GetAmmoType", |a| Ok(Value::Integer(info(a)?.ammo_type)))?;
    def("SetAmmoType", |a| {
        let n = a.number(2)? as i64;
        change(a, |i| i.ammo_type = n)
    })?;
    def("GetAttacker", |a| entity_or_null(a.lua, info(a)?.attacker))?;
    def("SetAttacker", |a| {
        let e = opt_entity(a, 2)?;
        change(a, |i| i.attacker = e)
    })?;
    def("GetInflictor", |a| entity_or_null(a.lua, info(a)?.inflictor))?;
    def("SetInflictor", |a| {
        let e = opt_entity(a, 2)?;
        change(a, |i| i.inflictor = e)
    })?;
    def("GetWeapon", |a| entity_or_null(a.lua, info(a)?.weapon))?;
    def("SetWeapon", |a| {
        let e = opt_entity(a, 2)?;
        change(a, |i| i.weapon = e)
    })?;
    def("GetDamageForce", |a| entity::vector(a.lua, info(a)?.force))?;
    def("SetDamageForce", |a| {
        let v = a.userdata::<Vector>(2, "Vector")?;
        change(a, |i| i.force = v)
    })?;
    def("GetDamagePosition", |a| entity::vector(a.lua, info(a)?.position))?;
    def("SetDamagePosition", |a| {
        let v = a.userdata::<Vector>(2, "Vector")?;
        change(a, |i| i.position = v)
    })?;
    def("GetReportedPosition", |a| entity::vector(a.lua, info(a)?.reported_position))?;
    def("SetReportedPosition", |a| {
        let v = a.userdata::<Vector>(2, "Vector")?;
        change(a, |i| i.reported_position = v)
    })?;

    let entity_meta = meta::meta_table(lua, "Entity", type_id::ENTITY)?;
    let def = |name: &'static str, f: fn(&Args) -> mlua::Result<Value>| -> mlua::Result<()> {
        entity_meta.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
    };
    def("TakeDamageInfo", |a| {
        let target = entity::this(a, 1)?;
        let info = a.userdata::<DamageInfo>(2, "CTakeDamageInfo")?;
        take(a.lua, target, info);
        Ok(Value::Nil)
    })?;
    // by the attacker with the inflictor, both the entity itself when not said
    def("TakeDamage", |a| {
        let target = entity::this(a, 1)?;
        let amount = a.number(2)? as f32;
        let attacker = opt_entity(a, 3)?.filter(|h| h.index.is_some()).unwrap_or(target);
        let inflictor = opt_entity(a, 4)?.filter(|h| h.index.is_some()).unwrap_or(attacker);
        take(a.lua, target, DamageInfo::new(amount, dmg::GENERIC, attacker, inflictor));
        Ok(Value::Nil)
    })?;
    Ok(())
}
