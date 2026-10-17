//! Effects: `EffectData` (`CEffectData`) and `util.Effect`.
//!
//! An effect - sparks, the glow of a frozen prop, a tracer - is drawn by the clients that see
//! it, so on the server `util.Effect` only takes what it is given. There are no clients yet,
//! and so nothing to send it to.

use mlua::{AnyUserData, Lua, MultiValue, Table, UserData, UserDataFields, UserDataMethods, Value};

use std::cell::RefCell;

use crate::args::Args;
use crate::entity::{self, EntityHandle, Kind};
use crate::meta::{self, type_id};
use crate::vector::{Angle, Vector};
use crate::{Realm, host};

/// What an effect is made with.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EffectData {
    pub origin: Vector,
    pub start: Vector,
    pub normal: Vector,
    pub angles: Angle,
    pub entity: EntityHandle,
    pub attachment: i64,
    pub color: i64,
    pub damage_type: i64,
    pub flags: i64,
    pub hit_box: i64,
    pub material_index: i64,
    pub surface_prop: i64,
    pub magnitude: f32,
    pub radius: f32,
    pub scale: f32,
}

impl Default for EffectData {
    fn default() -> EffectData {
        EffectData {
            origin: Vector::default(),
            start: Vector::default(),
            normal: Vector::default(),
            angles: Angle::default(),
            entity: EntityHandle::NULL,
            attachment: 0,
            color: 0,
            damage_type: 0,
            flags: 0,
            hit_box: 0,
            material_index: 0,
            surface_prop: 0,
            magnitude: 0.0,
            radius: 0.0,
            scale: 0.0,
        }
    }
}

impl UserData for EffectData {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("MetaName", "CEffectData");
        fields.add_meta_field("MetaID", type_id::EFFECTDATA);
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function("__index", |lua, (ud, key): (AnyUserData, Value)| meta::index(lua, "CEffectData", ud, key));
        methods.add_meta_function("__newindex", |lua, (ud, key, value): (AnyUserData, Value, Value)| {
            meta::new_index(lua, "CEffectData", ud, key, value)
        });
    }
}

const NAME: &str = "CEffectData";

fn data(a: &Args) -> mlua::Result<EffectData> {
    a.userdata::<EffectData>(1, NAME)
}

/// Where the engine keeps its own `util.Effect`, as it was made.
pub(crate) const ENGINE: &str = "opengmod.util.Effect";

/// An effect the engine makes itself (an explosion's). It goes straight to the engine's
/// `util.Effect`: as in the game, what Lua puts in its place (an addon's detour that logs or
/// blocks effects) does not see it.
pub(crate) fn dispatch(lua: &Lua, name: &str, data: EffectData) {
    let effect = lua.named_registry_value::<mlua::Function>(ENGINE);
    let (Ok(effect), Ok(name), Ok(data)) = (effect, lua.create_string(name), lua.create_userdata(data)) else { return };
    let _ = crate::files::call_reporting(lua, effect, MultiValue::from_iter([Value::String(name), Value::UserData(data)]));
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    lua.globals().raw_set("EffectData", lua.create_function(|_, _: MultiValue| Ok(EffectData::default()))?)?;
    let t = meta::meta_table(lua, NAME, type_id::EFFECTDATA)?;
    let def = |t: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<Value>| -> mlua::Result<()> {
        t.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
    };
    macro_rules! vector {
        ($get:literal, $set:literal, $field:ident) => {
            def(&t, $get, |a| entity::vector(a.lua, data(a)?.$field))?;
            def(&t, $set, |a| {
                let v = a.userdata::<Vector>(2, "Vector")?;
                a.change::<EffectData>(1, NAME, |d| d.$field = v)
            })?;
        };
    }
    macro_rules! number {
        ($get:literal, $set:literal, $field:ident, $ty:ty) => {
            def(&t, $get, |a| Ok(Value::Number(data(a)?.$field as f64)))?;
            def(&t, $set, |a| {
                let n = a.number(2)? as $ty;
                a.change::<EffectData>(1, NAME, |d| d.$field = n)
            })?;
        };
    }
    vector!("GetOrigin", "SetOrigin", origin);
    vector!("GetStart", "SetStart", start);
    vector!("GetNormal", "SetNormal", normal);
    number!("GetAttachment", "SetAttachment", attachment, i64);
    number!("GetColor", "SetColor", color, i64);
    number!("GetDamageType", "SetDamageType", damage_type, i64);
    number!("GetFlags", "SetFlags", flags, i64);
    number!("GetHitBox", "SetHitBox", hit_box, i64);
    number!("GetMaterialIndex", "SetMaterialIndex", material_index, i64);
    number!("GetSurfaceProp", "SetSurfaceProp", surface_prop, i64);
    number!("GetMagnitude", "SetMagnitude", magnitude, f32);
    number!("GetRadius", "SetRadius", radius, f32);
    number!("GetScale", "SetScale", scale, f32);
    def(&t, "GetAngles", |a| Ok(Value::UserData(a.lua.create_userdata(data(a)?.angles)?)))?;
    def(&t, "SetAngles", |a| {
        let angles = a.userdata::<Angle>(2, "Angle")?;
        a.change::<EffectData>(1, NAME, |d| d.angles = angles)
    })?;
    def(&t, "GetEntity", |a| entity::push(a.lua, data(a)?.entity))?;
    def(&t, "SetEntity", |a| {
        let handle = entity::handle_of(a.get(2)).unwrap_or(EntityHandle::NULL);
        a.change::<EffectData>(1, NAME, |d| d.entity = handle)
    })?;
    def(&t, "GetEntIndex", |a| Ok(Value::Integer(data(a)?.entity.index.map_or(0, i64::from))))?;
    def(&t, "SetEntIndex", |a| {
        let index = a.number(2)? as i64;
        let handle = u32::try_from(index).ok().and_then(|i| host(a.lua).entities.at(i)).unwrap_or(EntityHandle::NULL);
        a.change::<EffectData>(1, NAME, |d| d.entity = handle)
    })?;

    let util: Table = lua.globals().get("util")?;
    // `util.Effect(name, data, allowOverride, ignorePredictionOrRecipientFilter)`
    let effect = lua.create_function(|lua, values: MultiValue| {
        let a = Args::new(lua, values, "Effect");
        let name = a.string(1)?;
        let data = a.userdata::<EffectData>(2, NAME)?;
        if host(lua).realm == Realm::Server {
            send(lua, &name, data, a.get(4));
        } else if crate::timer::first_time_predicted(lua) {
            make(lua, &name, data);
        }
        Ok(())
    })?;
    lua.set_named_registry_value(ENGINE, &effect)?;
    util.raw_set("Effect", effect)?;
    Ok(())
}

/// The effects of Lua a client is making, oldest first.
#[derive(Default)]
pub(crate) struct Effects {
    live: RefCell<Vec<EntityHandle>>,
}

/// The server's effect goes to its players: those of a filter given, or all but the one whose
/// command makes it - its client made it already, predicting - unless told to ignore that.
pub(crate) fn send(lua: &Lua, name: &str, data: EffectData, filter: Option<&Value>) {
    let players = match filter {
        Some(value @ Value::UserData(_)) => crate::recipients::players_of(lua, Some(value)),
        _ => {
            let ignore_prediction = matches!(filter, Some(Value::Boolean(true)));
            let commanding = if ignore_prediction { None } else { host(lua).players.commanding.get() };
            crate::player::all(lua).into_iter().filter(|&p| Some(p) != commanding).collect()
        }
    };
    for player in players {
        crate::player::to_client(lua, player, crate::player::ToClient::Effect(name.to_string(), data));
    }
}

/// A client makes an effect: one of Lua's (`effects.Register`) as an entity of its own that
/// thinks and draws until it is done, else one of the engine's.
pub(crate) fn make(lua: &Lua, name: &str, data: EffectData) {
    let made = make_lua(lua, name, data).unwrap_or_else(|e| {
        host(lua).report(crate::ScriptError::from_lua(&e));
        true
    });
    if !made {
        crate::engine_effects::make(lua, name, data);
    }
}

/// An effect of Lua, if there is one by the name: `EFFECT:Init(data)`.
fn make_lua(lua: &Lua, name: &str, data: EffectData) -> mlua::Result<bool> {
    let Some(effects) = lua.globals().get::<Option<Table>>("effects")? else { return Ok(false) };
    let Some(create) = effects.get::<Option<mlua::Function>>("Create")? else { return Ok(false) };
    if create.call::<Value>(name)?.is_nil() {
        return Ok(false);
    }
    let handle = {
        let host = host(lua);
        let mut ent =
            entity::Ent::new(&name.to_ascii_lowercase(), Kind::Entity, host.entities.next_creation_id(), host.clock.cur_time.get());
        ent.effect = true;
        ent.spawned = true;
        match host.entities.insert_client(ent) {
            Some(handle) => handle,
            None => return Ok(true),
        }
    };
    let Value::UserData(ud) = entity::push(lua, handle)? else { return Ok(true) };
    let table = entity::table_of(&ud)?.expect("every entity has a table");
    create.call::<Value>((name, table.clone()))?;
    table.raw_set("Entity", &ud)?;
    entity::set_world_pos(lua, handle, data.origin);
    host(lua).effects.live.borrow_mut().push(handle);
    let data = lua.create_userdata(data)?;
    crate::ents::call_method(lua, &ud, "Init", MultiValue::from_iter([Value::UserData(data)]));
    Ok(true)
}

/// A client's frame: each effect of Lua thinks, and goes when it says it is done.
pub(crate) fn think(lua: &Lua) {
    let live: Vec<EntityHandle> = host(lua).effects.live.borrow().clone();
    for handle in live {
        if !host(lua).entities.is_valid(handle) {
            continue;
        }
        let Ok(Value::UserData(ud)) = entity::push(lua, handle) else { continue };
        let result = crate::ents::call_method(lua, &ud, "Think", MultiValue::new());
        let alive = matches!(result.as_ref().and_then(|r| r.front()), Some(v) if !matches!(v, Value::Nil | Value::Boolean(false)));
        if !alive {
            crate::ents::remove(lua, handle);
        }
    }
    host(lua).effects.live.borrow_mut().retain(|&h| host(lua).entities.get(h).is_some_and(|e| !e.removed));
}

/// The effects of Lua draw themselves (`EFFECT:Render`), among what is seen through.
pub(crate) fn render(lua: &Lua) {
    let live: Vec<EntityHandle> = host(lua).effects.live.borrow().clone();
    for handle in live {
        if host(lua).entities.get(handle).is_none_or(|e| e.removed || e.no_draw) {
            continue;
        }
        if let Ok(Value::UserData(ud)) = entity::push(lua, handle) {
            crate::ents::call_method(lua, &ud, "Render", MultiValue::new());
        }
    }
}
