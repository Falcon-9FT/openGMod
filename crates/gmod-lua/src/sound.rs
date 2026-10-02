//! Sounds: `Entity:EmitSound`, `EmitSound`, `CreateSound` (`CSoundPatch`) and the `sound`
//! library.
//!
//! The server tells clients what to play, and there is no client yet - and no sound - so
//! sounds are taken and kept quiet: a `CSoundPatch` remembers whether it plays and how, the
//! sound scripts of `sound.Add` are kept for `sound.GetProperties` and `sound.GetTable`.

use mlua::{AnyUserData, Lua, MultiValue, Table, UserData, UserDataFields, UserDataMethods, Value};

use crate::args::Args;
use crate::entity;
use crate::meta::{self, type_id};

const NAME: &str = "CSoundPatch";
/// The registry's table of sound scripts, by name.
const SCRIPTS: &str = "opengmod.sound_scripts";

/// A sound made to be played and changed (`CreateSound`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SoundPatch {
    pub playing: bool,
    pub pitch: f32,
    pub volume: f32,
    pub level: f32,
    pub dsp: i64,
}

impl Default for SoundPatch {
    fn default() -> SoundPatch {
        SoundPatch { playing: false, pitch: 100.0, volume: 1.0, level: 75.0, dsp: 0 }
    }
}

impl UserData for SoundPatch {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("MetaName", NAME);
        fields.add_meta_field("MetaID", type_id::SOUND);
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function("__index", |lua, (_, key): (AnyUserData, Value)| meta::lookup(lua, NAME, key));
        methods.add_meta_method("__tostring", |_, patch, ()| {
            Ok(format!("CSoundPatch [{}]", if patch.playing { "playing" } else { "stopped" }))
        });
    }
}

fn patch(a: &Args) -> mlua::Result<SoundPatch> {
    a.userdata::<SoundPatch>(1, NAME)
}

fn nothing(_: &Args) -> mlua::Result<Value> {
    Ok(Value::Nil)
}

/// The sound scripts, made when first asked for.
fn scripts(lua: &Lua) -> mlua::Result<Table> {
    if let Some(table) = lua.named_registry_value::<Option<Table>>(SCRIPTS)? {
        return Ok(table);
    }
    let table = lua.create_table()?;
    lua.set_named_registry_value(SCRIPTS, &table)?;
    Ok(table)
}

/// A table's copy, a level deep.
fn copy(lua: &Lua, table: &Table) -> mlua::Result<Table> {
    let copied = lua.create_table()?;
    for pair in table.pairs::<Value, Value>() {
        let (k, v) = pair?;
        copied.raw_set(k, v)?;
    }
    Ok(copied)
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    let def = |t: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<Value>| -> mlua::Result<()> {
        t.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
    };
    def(&globals, "EmitSound", |a| a.string(1).map(|_| Value::Nil))?;
    def(&globals, "CreateSound", |a| {
        a.userdata::<entity::EntityHandle>(1, "Entity")?;
        a.string(2)?;
        Ok(Value::UserData(a.lua.create_userdata(SoundPatch::default())?))
    })?;

    let sound = lua.create_table()?;
    def(&sound, "Add", |a| {
        let data = a.table(1)?;
        if let Some(name) = data.get::<Option<String>>("name")? {
            scripts(a.lua)?.raw_set(name.to_ascii_lowercase(), copy(a.lua, &data)?)?;
        }
        Ok(Value::Nil)
    })?;
    def(&sound, "GetProperties", |a| {
        let name = a.string(1)?.to_ascii_lowercase();
        match scripts(a.lua)?.raw_get::<Option<Table>>(name)? {
            Some(data) => Ok(Value::Table(copy(a.lua, &data)?)),
            None => Ok(Value::Nil),
        }
    })?;
    def(&sound, "GetTable", |a| {
        let names = a.lua.create_table()?;
        for pair in scripts(a.lua)?.pairs::<Value, Table>() {
            let (_, data) = pair?;
            names.raw_push(data.get::<Value>("name")?)?;
        }
        Ok(Value::Table(names))
    })?;
    def(&sound, "Play", |a| a.string(1).map(|_| Value::Nil))?;
    def(&sound, "EmitHint", nothing)?;
    def(&sound, "AddSoundOverrides", nothing)?;
    def(&sound, "SetActorGender", nothing)?;
    def(&sound, "GetLoudestSoundHint", nothing)?;
    globals.raw_set("sound", sound)?;

    let t = meta::meta_table(lua, NAME, type_id::SOUND)?;
    def(&t, "Play", |a| a.change::<SoundPatch>(1, NAME, |p| p.playing = true))?;
    def(&t, "PlayEx", |a| {
        let (volume, pitch) = (a.number(2)? as f32, a.number(3)? as f32);
        a.change::<SoundPatch>(1, NAME, |p| {
            p.playing = true;
            p.volume = volume.clamp(0.0, 1.0);
            p.pitch = pitch.clamp(0.0, 255.0);
        })
    })?;
    def(&t, "Stop", |a| a.change::<SoundPatch>(1, NAME, |p| p.playing = false))?;
    def(&t, "FadeOut", |a| a.change::<SoundPatch>(1, NAME, |p| p.playing = false))?;
    def(&t, "IsPlaying", |a| Ok(Value::Boolean(patch(a)?.playing)))?;
    def(&t, "ChangePitch", |a| {
        let pitch = a.number(2)? as f32;
        a.change::<SoundPatch>(1, NAME, |p| p.pitch = pitch.clamp(0.0, 255.0))
    })?;
    def(&t, "ChangeVolume", |a| {
        let volume = a.number(2)? as f32;
        a.change::<SoundPatch>(1, NAME, |p| p.volume = volume.clamp(0.0, 1.0))
    })?;
    def(&t, "SetSoundLevel", |a| {
        let level = a.number(2)? as f32;
        a.change::<SoundPatch>(1, NAME, |p| p.level = level)
    })?;
    def(&t, "SetDSP", |a| {
        let dsp = a.number(2)? as i64;
        a.change::<SoundPatch>(1, NAME, |p| p.dsp = dsp)
    })?;
    def(&t, "GetPitch", |a| Ok(Value::Number(f64::from(patch(a)?.pitch))))?;
    def(&t, "GetVolume", |a| Ok(Value::Number(f64::from(patch(a)?.volume))))?;
    def(&t, "GetSoundLevel", |a| Ok(Value::Number(f64::from(patch(a)?.level))))?;
    def(&t, "GetDSP", |a| Ok(Value::Integer(patch(a)?.dsp)))?;
    Ok(())
}

/// The sound methods of entities.
pub(crate) fn install_methods(def: &entity::Def) -> mlua::Result<()> {
    def("EmitSound", |a| {
        entity::this(a, 1)?;
        a.string(2)?;
        Ok(Value::Nil)
    })?;
    def("StopSound", |a| {
        entity::this(a, 1)?;
        a.string(2)?;
        Ok(Value::Nil)
    })?;
    // the IDs of looping sounds count from 1
    def("StartLoopingSound", |a| {
        entity::this(a, 1)?;
        a.string(2)?;
        let next: i64 = a.lua.named_registry_value::<Option<i64>>("opengmod.looping_sounds")?.unwrap_or(0) + 1;
        a.lua.set_named_registry_value("opengmod.looping_sounds", next)?;
        Ok(Value::Integer(next))
    })?;
    def("StopLoopingSound", |a| entity::this(a, 1).map(|_| Value::Nil))?;
    Ok(())
}
