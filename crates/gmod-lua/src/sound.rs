//! Sounds: `Entity:EmitSound`, `EmitSound`, `sound.Play`, `CreateSound` (`CSoundPatch`),
//! `Entity:StartLoopingSound` and the `sound` library.
//!
//! What Lua asks to play becomes a [`SoundEvent`] for the program to play
//! ([`crate::Runtime::take_sounds`]). A name is a sound script - one of `sound.Add`, else one
//! of the engine's (`scripts/game_sounds*.txt`, [`gmod_audio::Scripts`]) - or a file under
//! `sound/`. A script's wave, volume and pitch are picked from what it allows each time it
//! plays, its sound level and channel are its own.
//!
//! A client that predicts its player's weapon ([`crate::player::predict`]) plays what it makes
//! heard as it predicts it, once; the server's run of the same command then sends that player
//! none of it (`SuppressHostEvents`, which the engine does for every player whose client
//! predicts), and the others all of it.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;

use mlua::{AnyUserData, Lua, MultiValue, Table, UserData, UserDataFields, UserDataMethods, Value};

use crate::args::Args;
use crate::entity::{self, EntityHandle};
use crate::host;
use crate::meta::{self, type_id};
use crate::vector::Vector;

const NAME: &str = "CSoundPatch";
/// The registry's table of sound scripts, by name.
const SCRIPTS: &str = "opengmod.sound_scripts";
/// `SND_STOP`: `EmitSound` stops the sound instead.
const SND_STOP: i64 = 4;

/// A sound to play, as the game asked for it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SoundPlay {
    /// What it was asked for by (a script or a file), to stop it by.
    pub name: String,
    /// The file: a game path, `sound/...`.
    pub wave: String,
    /// Who makes it: its sound goes where the entity goes.
    pub entity: Option<EntityHandle>,
    /// Where it is, when no entity carries it; neither: a sound of the interface, in both ears.
    pub position: Option<Vector>,
    /// 1 is the sound as recorded.
    pub volume: f32,
    /// 1 is the recorded pitch.
    pub pitch: f32,
    /// In decibels: how far it carries.
    pub level: f32,
    pub channel: i32,
    /// The `CSoundPatch` or looping sound it is, to change or stop it by.
    pub patch: Option<u64>,
    /// The player whose client made it itself, predicting: it is not sent to them.
    #[serde(default)]
    pub unheard_by: Option<EntityHandle>,
    /// Whether it plays again from its start when it ends; none: as the file says (its loop).
    #[serde(default)]
    pub looping: Option<bool>,
    /// Seconds into the file it starts at.
    #[serde(default)]
    pub start: f32,
}

/// What Lua did with sounds, for the program to hear.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SoundEvent {
    Play(SoundPlay),
    /// `StopSound`: an entity's sounds (or those of nobody) asked for by this name.
    Stop {
        entity: Option<EntityHandle>,
        name: String,
    },
    /// A `CSoundPatch` or looping sound stops.
    StopPatch(u64),
    /// A `CSoundPatch` changes its volume or pitch (1 is as recorded).
    ChangePatch {
        patch: u64,
        volume: f32,
        pitch: f32,
    },
    /// A sound of no entity is somewhere else now (a channel of `sound.PlayFile`).
    MovePatch {
        patch: u64,
        position: Vector,
    },
}

impl SoundEvent {
    /// The player who is not sent it: their client made it itself.
    pub fn unheard_by(&self) -> Option<EntityHandle> {
        match self {
            SoundEvent::Play(play) => play.unheard_by,
            _ => None,
        }
    }
}

/// The sounds of a runtime.
#[derive(Default)]
pub(crate) struct Sounds {
    pub events: RefCell<Vec<SoundEvent>>,
    /// The player the sounds made now are not sent to (`SuppressHostEvents`).
    pub unheard_by: Cell<Option<EntityHandle>>,
    /// The engine's sound scripts, read when first asked for.
    engine: OnceCell<gmod_audio::Scripts>,
    /// The entity and name of each `CSoundPatch` and looping sound, by number.
    patches: RefCell<HashMap<u64, (Option<EntityHandle>, String)>>,
    next_patch: Cell<u64>,
    seed: Cell<u64>,
}

impl Sounds {
    /// The sounds of a realm: a client numbers its patches apart from its server's, whose
    /// sounds it plays beside its own.
    pub(crate) fn new(realm: crate::Realm) -> Sounds {
        let sounds = Sounds::default();
        if realm != crate::Realm::Server {
            sounds.next_patch.set(1 << 63);
        }
        sounds
    }

    /// A number for a sound to change or stop it by, of no `CSoundPatch` (a channel of
    /// [`crate::bass`]).
    pub(crate) fn new_number(&self) -> u64 {
        let id = self.next_patch.get() + 1;
        self.next_patch.set(id);
        id
    }

    fn new_patch(&self, entity: Option<EntityHandle>, name: String) -> u64 {
        let id = self.next_patch.get() + 1;
        self.next_patch.set(id);
        self.patches.borrow_mut().insert(id, (entity, name));
        id
    }

    /// A number from 0 to 1, for the picks of scripts.
    fn random(&self) -> f32 {
        // xorshift: the picks need no more than to differ
        let mut x = self.seed.get();
        if x == 0 {
            x = 0x9E37_79B9_7F4A_7C15;
        }
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.seed.set(x);
        (x >> 40) as f32 / (1u64 << 24) as f32
    }

    fn pick(&self, (from, to): (f32, f32)) -> f32 {
        from + (to - from) * self.random()
    }
}

/// A sound made to be played and changed (`CreateSound`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SoundPatch {
    pub id: u64,
    pub playing: bool,
    pub pitch: f32,
    pub volume: f32,
    pub level: f32,
    pub dsp: i64,
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

/// The sound scripts of `sound.Add`, made when first asked for.
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

/// What a sound plays and how, before where.
struct Resolved {
    wave: String,
    volume: f32,
    pitch: f32,
    level: f32,
    channel: i32,
}

/// A number or a range of two (`{ 95, 105 }`) of a `sound.Add` table.
fn lua_range(value: Value) -> Option<(f32, f32)> {
    match value {
        Value::Integer(n) => Some((n as f32, n as f32)),
        Value::Number(n) => Some((n as f32, n as f32)),
        Value::Table(t) => {
            let from = t.raw_get::<Option<f64>>(1).ok().flatten()? as f32;
            let to = t.raw_get::<Option<f64>>(2).ok().flatten().map_or(from, |n| n as f32);
            Some((from.min(to), from.max(to)))
        }
        _ => None,
    }
}

/// A name made into what plays: a script of `sound.Add`, of the engine, or a file - which
/// plays as the caller says (`volume` 1 is as recorded, `pitch` in percent).
fn resolve(lua: &Lua, name: &str, volume: f32, pitch: f32, level: f32, channel: i32) -> Option<Resolved> {
    let sounds = &host(lua).sounds;
    if let Ok(Some(script)) = scripts(lua).and_then(|t| t.raw_get::<Option<Table>>(name.to_ascii_lowercase())) {
        let waves: Vec<String> = match script.get::<Value>("sound").ok()? {
            Value::String(s) => vec![s.to_string_lossy()],
            Value::Table(t) => t.sequence_values::<String>().filter_map(Result::ok).collect(),
            _ => Vec::new(),
        };
        let wave = waves.get((sounds.random() * waves.len() as f32) as usize % waves.len().max(1))?.clone();
        let number = |key: &str| lua_range(script.get::<Value>(key).unwrap_or(Value::Nil));
        return Some(Resolved {
            wave: gmod_audio::script::wave_path(&wave),
            volume: sounds.pick(number("volume").unwrap_or((1.0, 1.0))),
            pitch: sounds.pick(number("pitch").unwrap_or((100.0, 100.0))),
            level: number("level").map_or(75.0, |r| r.0),
            channel: number("channel").map_or(0, |r| r.0 as i32),
        });
    }
    let vfs = host(lua).vfs.clone();
    let engine = sounds.engine.get_or_init(|| gmod_audio::Scripts::load(&vfs));
    if let Some(script) = engine.get(name) {
        let count = script.waves.len();
        let wave = script.waves.get((sounds.random() * count as f32) as usize % count.max(1))?;
        return Some(Resolved {
            wave: gmod_audio::script::wave_path(wave),
            volume: sounds.pick(script.volume),
            pitch: sounds.pick(script.pitch),
            level: script.level,
            channel: script.channel,
        });
    }
    // a file: what Source knows by its extension
    let lower = name.to_ascii_lowercase();
    if [".wav", ".mp3", ".ogg"].iter().any(|e| lower.ends_with(e)) {
        return Some(Resolved { wave: gmod_audio::script::wave_path(name), volume, pitch, level, channel });
    }
    None
}

/// Plays a sound by name; nothing when the name is no sound.
#[allow(clippy::too_many_arguments)]
fn play(
    lua: &Lua,
    name: &str,
    entity: Option<EntityHandle>,
    position: Option<Vector>,
    volume: f32,
    pitch: f32,
    level: f32,
    channel: i32,
    patch: Option<u64>,
) {
    // a tick predicted again was heard the first time
    if !crate::timer::first_time_predicted(lua) {
        return;
    }
    let Some(r) = resolve(lua, name, volume, pitch, level, channel) else { return };
    let unheard_by = host(lua).sounds.unheard_by.get();
    host(lua).sounds.events.borrow_mut().push(SoundEvent::Play(SoundPlay {
        name: name.to_string(),
        wave: r.wave,
        entity,
        position,
        volume: r.volume.clamp(0.0, 1.0),
        pitch: (r.pitch / 100.0).clamp(0.01, 2.55),
        level: r.level,
        channel: r.channel,
        patch,
        unheard_by,
        looping: None,
        start: 0.0,
    }));
}

/// A sound at a place, of no entity: what is left of something that broke or blew up.
pub(crate) fn emit_at(lua: &Lua, name: &str, position: Vector) {
    play(lua, name, None, Some(position), 1.0, 100.0, 75.0, 0, None);
}

/// A sound an entity makes, as loud as `scale` makes what its script says (an impact's).
pub(crate) fn emit_scaled(lua: &Lua, name: &str, entity: EntityHandle, scale: f32) {
    let before = host(lua).sounds.events.borrow().len();
    play(lua, name, Some(entity), None, 1.0, 100.0, 75.0, 0, None);
    if let Some(SoundEvent::Play(play)) = host(lua).sounds.events.borrow_mut().get_mut(before) {
        play.volume *= scale.clamp(0.0, 1.0);
    }
}

/// `surface.PlaySound`: a sound of the interface, in both ears.
pub(crate) fn play_ui(lua: &Lua, name: &str) {
    play(lua, name, None, None, 1.0, 100.0, 0.0, 0, None);
}

/// An optional number argument.
fn number_or(a: &Args, position: usize, default: f64) -> mlua::Result<f64> {
    match a.get(position) {
        None | Some(Value::Nil) => Ok(default),
        _ => a.number(position),
    }
}

fn vector_arg(a: &Args, position: usize) -> mlua::Result<Option<Vector>> {
    match a.get(position) {
        None | Some(Value::Nil) => Ok(None),
        _ => a.userdata::<Vector>(position, "Vector").map(Some),
    }
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    let def = |t: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<Value>| -> mlua::Result<()> {
        t.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
    };
    if host(lua).realm == crate::Realm::Server {
        // SuppressHostEvents(player): what is heard from now on is not sent to them (NULL: to all)
        def(&globals, "SuppressHostEvents", |a| {
            let player = entity::handle_of(a.get(1)).filter(|&h| h != EntityHandle::NULL);
            host(a.lua).sounds.unheard_by.set(player);
            Ok(Value::Nil)
        })?;
    }
    // EmitSound(name, position, entity index, channel, volume, level, flags, pitch, dsp, filter)
    def(&globals, "EmitSound", |a| {
        let name = a.string(1)?;
        let position = vector_arg(a, 2)?;
        let index = number_or(a, 3, 0.0)? as u32;
        let entity = (index > 0).then(|| host(a.lua).entities.at(index)).flatten();
        let channel = number_or(a, 4, 0.0)? as i32;
        let (volume, level, flags, pitch) = (number_or(a, 5, 1.0)?, number_or(a, 6, 75.0)?, number_or(a, 7, 0.0)?, number_or(a, 8, 100.0)?);
        if flags as i64 & SND_STOP != 0 {
            host(a.lua).sounds.events.borrow_mut().push(SoundEvent::Stop { entity, name });
        } else {
            play(a.lua, &name, entity, position, volume as f32, pitch as f32, level as f32, channel, None);
        }
        Ok(Value::Nil)
    })?;
    def(&globals, "CreateSound", |a| {
        let entity = a.userdata::<EntityHandle>(1, "Entity")?;
        let name = a.string(2)?;
        let id = host(a.lua).sounds.new_patch(Some(entity), name);
        Ok(Value::UserData(a.lua.create_userdata(SoundPatch { id, playing: false, pitch: 100.0, volume: 1.0, level: 75.0, dsp: 0 })?))
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
    // sound.Play(name, position, level, pitch, volume, dsp)
    def(&sound, "Play", |a| {
        let name = a.string(1)?;
        let position = vector_arg(a, 2)?;
        let (level, pitch, volume) = (number_or(a, 3, 75.0)?, number_or(a, 4, 100.0)?, number_or(a, 5, 1.0)?);
        play(a.lua, &name, None, position, volume as f32, pitch as f32, level as f32, 0, None);
        Ok(Value::Nil)
    })?;
    def(&sound, "EmitHint", nothing)?;
    def(&sound, "AddSoundOverrides", nothing)?;
    def(&sound, "SetActorGender", nothing)?;
    def(&sound, "GetLoudestSoundHint", nothing)?;
    globals.raw_set("sound", sound)?;

    let t = meta::meta_table(lua, NAME, type_id::SOUND)?;
    def(&t, "Play", |a| {
        let p = patch(a)?;
        start_patch(a.lua, &p);
        a.change::<SoundPatch>(1, NAME, |p| p.playing = true)
    })?;
    def(&t, "PlayEx", |a| {
        let (volume, pitch) = (a.number(2)? as f32, a.number(3)? as f32);
        a.change::<SoundPatch>(1, NAME, |p| {
            p.volume = volume.clamp(0.0, 1.0);
            p.pitch = pitch.clamp(0.0, 255.0);
        })?;
        let p = patch(a)?;
        start_patch(a.lua, &p);
        a.change::<SoundPatch>(1, NAME, |p| p.playing = true)
    })?;
    def(&t, "Stop", stop_patch)?;
    def(&t, "FadeOut", stop_patch)?;
    def(&t, "IsPlaying", |a| Ok(Value::Boolean(patch(a)?.playing)))?;
    def(&t, "ChangePitch", |a| {
        let pitch = a.number(2)? as f32;
        a.change::<SoundPatch>(1, NAME, |p| p.pitch = pitch.clamp(0.0, 255.0))?;
        change_patch(a)
    })?;
    def(&t, "ChangeVolume", |a| {
        let volume = a.number(2)? as f32;
        a.change::<SoundPatch>(1, NAME, |p| p.volume = volume.clamp(0.0, 1.0))?;
        change_patch(a)
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

/// A patch starts (again): what plays before it stops.
fn start_patch(lua: &Lua, p: &SoundPatch) {
    let Some((entity, name)) = host(lua).sounds.patches.borrow().get(&p.id).cloned() else { return };
    let events = &host(lua).sounds.events;
    if p.playing {
        events.borrow_mut().push(SoundEvent::StopPatch(p.id));
    }
    play(lua, &name, entity, None, p.volume, p.pitch, p.level, 0, Some(p.id));
    // a script gives the pitch and volume; the patch's own, when changed, go over them
    if p.volume != 1.0 || p.pitch != 100.0 {
        events.borrow_mut().push(SoundEvent::ChangePatch { patch: p.id, volume: p.volume, pitch: p.pitch / 100.0 });
    }
}

fn stop_patch(a: &Args) -> mlua::Result<Value> {
    let p = patch(a)?;
    if p.playing {
        host(a.lua).sounds.events.borrow_mut().push(SoundEvent::StopPatch(p.id));
    }
    a.change::<SoundPatch>(1, NAME, |p| p.playing = false)
}

fn change_patch(a: &Args) -> mlua::Result<Value> {
    let p = patch(a)?;
    if p.playing {
        host(a.lua).sounds.events.borrow_mut().push(SoundEvent::ChangePatch { patch: p.id, volume: p.volume, pitch: p.pitch / 100.0 });
    }
    Ok(Value::Nil)
}

/// The sound methods of entities.
pub(crate) fn install_methods(def: &entity::Def) -> mlua::Result<()> {
    // Entity:EmitSound(name, level, pitch, volume, channel, flags, dsp, filter)
    def("EmitSound", |a| {
        let entity = entity::this(a, 1)?;
        let name = a.string(2)?;
        let (level, pitch, volume) = (number_or(a, 3, 75.0)?, number_or(a, 4, 100.0)?, number_or(a, 5, 1.0)?);
        let (channel, flags) = (number_or(a, 6, 0.0)? as i32, number_or(a, 7, 0.0)? as i64);
        if flags & SND_STOP != 0 {
            host(a.lua).sounds.events.borrow_mut().push(SoundEvent::Stop { entity: Some(entity), name });
        } else {
            play(a.lua, &name, Some(entity), None, volume as f32, pitch as f32, level as f32, channel, None);
        }
        Ok(Value::Nil)
    })?;
    def("StopSound", |a| {
        let entity = entity::this(a, 1)?;
        let name = a.string(2)?;
        host(a.lua).sounds.events.borrow_mut().push(SoundEvent::Stop { entity: Some(entity), name });
        Ok(Value::Nil)
    })?;
    // a looping sound's number counts from 1
    def("StartLoopingSound", |a| {
        let entity = entity::this(a, 1)?;
        let name = a.string(2)?;
        let id = host(a.lua).sounds.new_patch(Some(entity), name.clone());
        play(a.lua, &name, Some(entity), None, 1.0, 100.0, 75.0, 0, Some(id));
        Ok(Value::Integer(id as i64))
    })?;
    def("StopLoopingSound", |a| {
        entity::this(a, 1)?;
        let id = a.number(2)? as u64;
        host(a.lua).sounds.events.borrow_mut().push(SoundEvent::StopPatch(id));
        Ok(Value::Nil)
    })?;
    Ok(())
}
