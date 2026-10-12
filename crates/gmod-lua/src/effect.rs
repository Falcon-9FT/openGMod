//! Effects: `EffectData` (`CEffectData`) and `util.Effect`.
//!
//! An effect - sparks, the glow of a frozen prop, a tracer - is drawn by the clients that see
//! it, so on the server `util.Effect` only takes what it is given. There are no clients yet,
//! and so nothing to send it to.

use mlua::{AnyUserData, Lua, MultiValue, Table, UserData, UserDataFields, UserDataMethods, Value};

use crate::args::Args;
use crate::entity::{self, EntityHandle};
use crate::meta::{self, type_id};
use crate::vector::{Angle, Vector};

/// What an effect is made with.
#[derive(Debug, Clone, Copy, PartialEq)]
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
        methods.add_meta_function("__index", |lua, (_, key): (AnyUserData, Value)| meta::lookup(lua, "CEffectData", key));
    }
}

const NAME: &str = "CEffectData";

fn data(a: &Args) -> mlua::Result<EffectData> {
    a.userdata::<EffectData>(1, NAME)
}

/// An effect the engine makes itself (an explosion's), the way Lua makes one: through
/// `util.Effect`, so that what the game puts in its place sees it too.
pub(crate) fn dispatch(lua: &Lua, name: &str, data: EffectData) {
    let effect = lua.globals().get::<Table>("util").and_then(|util| util.get::<mlua::Function>("Effect"));
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
        let handle = u32::try_from(index).ok().and_then(|i| crate::host(a.lua).entities.at(i)).unwrap_or(EntityHandle::NULL);
        a.change::<EffectData>(1, NAME, |d| d.entity = handle)
    })?;

    let util: Table = lua.globals().get("util")?;
    util.raw_set(
        "Effect",
        lua.create_function(|lua, values: MultiValue| {
            let a = Args::new(lua, values, "Effect");
            a.string(1)?;
            a.userdata::<EffectData>(2, NAME)?;
            Ok(())
        })?,
    )?;
    Ok(())
}
