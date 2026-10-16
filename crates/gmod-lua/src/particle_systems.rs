//! The particle systems of `.pcf` files (`ParticleEffect`, `ParticleEffectAttach`,
//! `CreateParticleSystem`, `CNewParticleEffect`). The files themselves are not read: a system
//! is drawn as the engine's own effect its name says it is - an explosion, fire, smoke, sparks,
//! blood, water, a gun's flash - and one whose name says none of these shows nothing. The
//! server's go to its players, as GMod sends them.

use std::cell::Cell;

use mlua::{AnyUserData, Lua, MultiValue, Table, UserData, UserDataFields, UserDataMethods, Value};

use crate::args::Args;
use crate::effect::EffectData;
use crate::entity::{self, EntityHandle};
use crate::meta::{self, type_id};
use crate::vector::{Angle, Vector};
use crate::{Realm, host};

/// What the engine's effects show for a system of this name, if anything.
fn look(name: &str) -> Option<&'static str> {
    let name = name.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| name.contains(w));
    Some(if has(&["explo", "boom", "blast", "nitro", "nuke", "detonat", "grenade", "rocket_impact"]) {
        "Explosion"
    } else if has(&["muzzle", "flash"]) {
        "MuzzleFlash"
    } else if has(&["spark", "electric", "zap"]) {
        "Sparks"
    } else if has(&["blood", "gore"]) {
        "BloodImpact"
    } else if has(&["water", "splash", "rain"]) {
        "watersplash"
    } else if has(&["fire", "flame", "burn", "ember"]) {
        "fire"
    } else if has(&["smoke", "dust", "debris", "steam", "fog"]) {
        "WheelDust"
    } else if has(&["impact", "hit"]) {
        "Impact"
    } else {
        return None;
    })
}

/// A client shows a system at a place.
pub(crate) fn show(lua: &Lua, name: &str, data: EffectData) {
    match look(name) {
        Some("fire") => {
            let burst = EffectData { magnitude: 2.0, normal: Vector::new(0.0, 0.0, 1.0), ..data };
            crate::engine_effects::make(lua, "Explosion", EffectData { scale: 0.4, ..burst });
        }
        Some(effect) => {
            let data = EffectData { normal: if data.normal.length() > 0.1 { data.normal } else { Vector::new(0.0, 0.0, 1.0) }, ..data };
            crate::engine_effects::make(lua, effect, data);
        }
        None => {}
    }
}

/// A system the server starts goes to its players; a client's shows now.
fn start(lua: &Lua, name: &str, data: EffectData) {
    if host(lua).realm == Realm::Server {
        for player in crate::player::all(lua) {
            crate::player::to_client(lua, player, crate::player::ToClient::ParticleSystem(name.to_string(), data));
        }
    } else {
        show(lua, name, data);
    }
}

/// Where an entity's attachment is, or the entity itself (`PATTACH_*` aside).
fn at_entity(lua: &Lua, handle: EntityHandle, attachment: i64) -> Option<(Vector, Angle)> {
    usize::try_from(attachment - 1)
        .ok()
        .filter(|_| attachment > 0)
        .and_then(|i| crate::anim::attachment_world(lua, handle, i))
        .or_else(|| host(lua).entities.transform(handle))
}

const NAME: &str = "CNewParticleEffect";

/// `CNewParticleEffect`: a system made to be held on to.
struct System {
    name: String,
    owner: EntityHandle,
    position: Cell<Vector>,
    emitting: Cell<bool>,
    valid: Cell<bool>,
    should_draw: Cell<bool>,
    view_model: Cell<bool>,
}

impl UserData for System {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("MetaName", NAME);
        fields.add_meta_field("MetaID", type_id::PARTICLESYSTEM);
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function("__index", |lua, (ud, key): (AnyUserData, Value)| meta::index(lua, NAME, ud, key));
        methods.add_meta_function("__newindex", |lua, (ud, key, value): (AnyUserData, Value, Value)| {
            meta::new_index(lua, NAME, ud, key, value)
        });
        methods.add_meta_method("__tostring", |_, s, ()| Ok(format!("CNewParticleEffect [{}]", s.name)));
    }
}

fn with<R>(a: &Args, f: impl FnOnce(&System) -> R) -> mlua::Result<R> {
    match a.get(1) {
        Some(Value::UserData(ud)) => Ok(f(&*ud.borrow::<System>().map_err(|_| a.bad(1, NAME))?)),
        _ => Err(a.bad(1, NAME)),
    }
}

fn def(lua: &Lua, table: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<Value>) -> mlua::Result<()> {
    table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
}

/// A held system: shown once now, at its place.
fn make_system(lua: &Lua, name: &str, owner: EntityHandle, position: Vector) -> mlua::Result<Value> {
    let system = System {
        name: name.to_string(),
        owner,
        position: Cell::new(position),
        emitting: Cell::new(true),
        valid: Cell::new(true),
        should_draw: Cell::new(true),
        view_model: Cell::new(false),
    };
    show(lua, name, EffectData { origin: position, entity: owner, ..EffectData::default() });
    Ok(Value::UserData(lua.create_userdata(system)?))
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    // `ParticleEffect(name, pos, angles, parent)`
    def(lua, &globals, "ParticleEffect", |a| {
        let name = a.string(1)?;
        let (pos, angles) = (a.userdata::<Vector>(2, "Vector")?, a.userdata::<Angle>(3, "Angle")?);
        let parent = entity::handle_of(a.get(4)).unwrap_or(EntityHandle::NULL);
        start(a.lua, &name, EffectData { origin: pos, angles, normal: angles.vectors().0, entity: parent, ..EffectData::default() });
        Ok(Value::Nil)
    })?;
    // `ParticleEffectAttach(name, attachType, entity, attachmentID)`
    def(lua, &globals, "ParticleEffectAttach", |a| {
        let name = a.string(1)?;
        let handle = entity::handle_of(a.get(3)).ok_or_else(|| a.bad(3, "Entity"))?;
        let attachment = a.opt_number(4)?.unwrap_or(0.0) as i64;
        if let Some((pos, angles)) = at_entity(a.lua, handle, attachment) {
            start(a.lua, &name, EffectData { origin: pos, angles, entity: handle, attachment, ..EffectData::default() });
        }
        Ok(Value::Nil)
    })?;
    // `util.ParticleTracer(name, start, end, doWhiz)` and `ParticleTracerEx`: a tracer
    let util: Table = globals.get("util")?;
    let tracer = |a: &Args| {
        let (from, to) = (a.userdata::<Vector>(2, "Vector")?, a.userdata::<Vector>(3, "Vector")?);
        let data = EffectData { start: from, origin: to, ..EffectData::default() };
        if host(a.lua).realm == Realm::Server {
            crate::effect::send(a.lua, "Tracer", data, Some(&Value::Boolean(true)));
        } else {
            crate::effect::make(a.lua, "Tracer", data);
        }
        Ok(Value::Nil)
    };
    def(lua, &util, "ParticleTracer", tracer)?;
    def(lua, &util, "ParticleTracerEx", tracer)?;

    let ent = meta::meta_table(lua, "Entity", type_id::ENTITY)?;
    // the systems an entity has go with it: stopping them is all there is to do
    for name in
        ["StopParticles", "StopParticleEmission", "StopParticlesNamed", "StopAndDestroyParticles", "StopParticlesWithNameAndAttachment"]
    {
        def(lua, &ent, name, |a| entity::handle_of(a.get(1)).map(|_| Value::Nil).ok_or_else(|| a.bad(1, "Entity")))?;
    }
    if host(lua).realm == Realm::Server {
        return Ok(());
    }
    // `CreateParticleSystem(entity, name, attachType, attachment, offset)`
    def(lua, &globals, "CreateParticleSystem", |a| {
        let handle = entity::handle_of(a.get(1)).ok_or_else(|| a.bad(1, "Entity"))?;
        let name = a.string(2)?;
        let attachment = a.opt_number(4)?.unwrap_or(0.0) as i64;
        let offset = a.get(5).and_then(|v| match v {
            Value::UserData(ud) => ud.borrow::<Vector>().ok().map(|v| *v),
            _ => None,
        });
        let pos = at_entity(a.lua, handle, attachment).map_or(Vector::default(), |(p, _)| p) + offset.unwrap_or_default();
        make_system(a.lua, &name, handle, pos)
    })?;
    def(lua, &globals, "CreateParticleSystemNoEntity", |a| {
        let name = a.string(1)?;
        let pos = a.userdata::<Vector>(2, "Vector")?;
        make_system(a.lua, &name, EntityHandle::NULL, pos)
    })?;
    // `Entity:CreateParticleEffect(name, attachment or control points, options)`
    def(lua, &ent, "CreateParticleEffect", |a| {
        let handle = entity::handle_of(a.get(1)).ok_or_else(|| a.bad(1, "Entity"))?;
        let name = a.string(2)?;
        let attachment = a.opt_number(3).ok().flatten().unwrap_or(0.0) as i64;
        let pos = at_entity(a.lua, handle, attachment).map_or(Vector::default(), |(p, _)| p);
        make_system(a.lua, &name, handle, pos)
    })?;

    let t = meta::meta_table(lua, NAME, type_id::PARTICLESYSTEM)?;
    def(lua, &t, "IsValid", |a| Ok(Value::Boolean(with(a, |s| s.valid.get())?)))?;
    def(lua, &t, "GetEffectName", |a| Ok(Value::String(a.lua.create_string(with(a, |s| s.name.clone())?)?)))?;
    def(lua, &t, "GetOwner", |a| entity::push(a.lua, with(a, |s| s.owner)?))?;
    def(lua, &t, "IsFinished", |a| Ok(Value::Boolean(with(a, |s| !s.emitting.get())?)))?;
    def(lua, &t, "GetShouldSimulate", |a| Ok(Value::Boolean(with(a, |s| s.emitting.get())?)))?;
    def(lua, &t, "SetShouldSimulate", |a| {
        let on = a.boolean(2);
        with(a, |s| s.emitting.set(on))?;
        Ok(Value::Nil)
    })?;
    def(lua, &t, "SetShouldDraw", |a| {
        let on = a.boolean(2);
        with(a, |s| s.should_draw.set(on))?;
        Ok(Value::Nil)
    })?;
    def(lua, &t, "IsViewModelEffect", |a| Ok(Value::Boolean(with(a, |s| s.view_model.get())?)))?;
    def(lua, &t, "SetIsViewModelEffect", |a| {
        let on = a.boolean(2);
        with(a, |s| s.view_model.set(on))?;
        Ok(Value::Nil)
    })?;
    def(lua, &t, "StopEmission", |a| {
        with(a, |s| s.emitting.set(false))?;
        Ok(Value::Nil)
    })?;
    def(lua, &t, "StopEmissionAndDestroyImmediately", |a| {
        with(a, |s| {
            s.emitting.set(false);
            s.valid.set(false);
        })?;
        Ok(Value::Nil)
    })?;
    def(lua, &t, "StartEmission", |a| {
        let (name, pos) = with(a, |s| {
            s.emitting.set(true);
            (s.name.clone(), s.position.get())
        })?;
        show(a.lua, &name, EffectData { origin: pos, ..EffectData::default() });
        Ok(Value::Nil)
    })?;
    def(lua, &t, "Restart", |a| {
        let (name, pos) = with(a, |s| (s.name.clone(), s.position.get()))?;
        show(a.lua, &name, EffectData { origin: pos, ..EffectData::default() });
        Ok(Value::Nil)
    })?;
    // control point 0 is where it is
    def(lua, &t, "SetControlPoint", |a| {
        let (point, pos) = (a.number(2)? as i64, a.userdata::<Vector>(3, "Vector")?);
        if point == 0 {
            with(a, |s| s.position.set(pos))?;
        }
        Ok(Value::Nil)
    })?;
    def(lua, &t, "SetSortOrigin", |a| {
        let pos = a.userdata::<Vector>(2, "Vector")?;
        with(a, |s| s.position.set(pos))?;
        Ok(Value::Nil)
    })?;
    def(lua, &t, "GetHighestControlPoint", |a| with(a, |_| Value::Integer(0)))?;
    def(lua, &t, "GetAutoUpdateBBox", |a| with(a, |_| Value::Boolean(true)))?;
    t.raw_set(
        "GetRenderBounds",
        lua.create_function(|lua, values: MultiValue| {
            let a = Args::new(lua, values, "GetRenderBounds");
            let pos = with(&a, |s| s.position.get())?;
            let half = Vector::new(16.0, 16.0, 16.0);
            Ok(MultiValue::from_iter([entity::vector(lua, pos - half)?, entity::vector(lua, pos + half)?]))
        })?,
    )?;
    // drawn as the engine's effect when made; there is nothing more to draw
    for name in [
        "Render",
        "AddControlPoint",
        "SetControlPointEntity",
        "SetControlPointForwardVector",
        "SetControlPointOrientation",
        "SetControlPointParent",
        "SetControlPointRightVector",
        "SetControlPointUpVector",
    ] {
        def(lua, &t, name, |a| with(a, |_| Value::Nil))?;
    }
    Ok(())
}
