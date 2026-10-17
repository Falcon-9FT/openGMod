//! `sound.PlayFile` and its channels (`IGModAudioChannel`), which GMod plays with BASS: a
//! sound file the client's Lua starts, pauses, stops, makes louder or faster, moves about in
//! the world and seeks in, as it likes.
//!
//! A channel plays through the same [`SoundEvent`]s as the other sounds (a sound of its own
//! number, changed, moved and stopped by it). How far it has got is worked out from when it
//! started and how fast it plays; seeking or going on after a pause starts it again from
//! there. The callback of `PlayFile` comes on the next frame, as BASS's does.
//!
//! Not done: streams from the internet (`sound.PlayURL` says it cannot open them), the
//! levels and spectrum of what plays (`GetLevel`, `FFT` say silence), 3D cones.

use std::cell::RefCell;
use std::collections::HashMap;

use mlua::{AnyUserData, Function, Lua, MultiValue, Table, UserData, UserDataFields, UserDataMethods, Value};

use crate::args::Args;
use crate::host;
use crate::sound::{SoundEvent, SoundPlay};
use crate::vector::Vector;

const NAME: &str = "IGModAudioChannel";

// what `GetState` says
const STOPPED: i64 = 0;
const PLAYING: i64 = 1;
const PAUSED: i64 = 2;

// BASS's errors, as the callback is told them
const BASS_ERROR_FILEOPEN: i64 = 2;
const BASS_ERROR_FILEFORM: i64 = 41;

/// The distance up to which a 3D channel is heard whole, until `Set3DFadeDistance` says.
const DEFAULT_FADE: f32 = 200.0;

/// A channel, by its number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Channel(u64);

impl UserData for Channel {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("MetaName", NAME);
        fields.add_meta_field("MetaID", crate::meta::type_id::SOUNDHANDLE);
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function("__index", |lua, (ud, key): (AnyUserData, Value)| crate::meta::index(lua, NAME, ud, key));
        methods.add_meta_function("__newindex", |lua, (ud, key, value): (AnyUserData, Value, Value)| {
            crate::meta::new_index(lua, NAME, ud, key, value)
        });
        methods.add_meta_method("__tostring", |lua, this, ()| {
            Ok(format!("{NAME} [{}]", with(lua, *this, |c| c.file.clone()).unwrap_or_default()))
        });
        methods.add_meta_method("__eq", |_, this, other: AnyUserData| Ok(other.borrow::<Channel>().is_ok_and(|o| *o == *this)));
    }
}

/// What a channel is and does.
#[derive(Debug, Clone)]
struct State {
    file: String,
    volume: f32,
    rate: f32,
    pan: f32,
    position: Vector,
    three_d: bool,
    looping: bool,
    fade: (f32, f32),
    /// Playing now, and since when (seconds of the program) from where (seconds of the file).
    playing: bool,
    paused: bool,
    since: f64,
    from: f64,
    /// Seconds the file lasts, once worked out.
    length: Option<f64>,
    gone: bool,
}

/// A callback of `PlayFile` waiting for the next frame, and its channel or BASS's error.
type Waiting = (Function, Result<u64, (i64, &'static str)>);

/// The channels of a client.
#[derive(Default)]
pub(crate) struct Channels {
    channels: RefCell<HashMap<u64, State>>,
    /// Callbacks of `PlayFile` for the next frame: the callback, and the channel or the error.
    waiting: RefCell<Vec<Waiting>>,
    /// How long files last, by their path.
    lengths: RefCell<HashMap<String, f64>>,
}

fn with<R>(lua: &Lua, channel: Channel, f: impl FnOnce(&mut State) -> R) -> Option<R> {
    host(lua).channels.channels.borrow_mut().get_mut(&channel.0).filter(|c| !c.gone).map(f)
}

fn now(lua: &Lua) -> f64 {
    host(lua).started.elapsed().as_secs_f64()
}

fn push(lua: &Lua, event: SoundEvent) {
    host(lua).sounds.events.borrow_mut().push(event);
}

/// How long a file lasts, in seconds (decoded once).
fn length(lua: &Lua, file: &str) -> f64 {
    if let Some(&seconds) = host(lua).channels.lengths.borrow().get(file) {
        return seconds;
    }
    let extension = file.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    let seconds = host(lua)
        .vfs
        .read(file, "GAME")
        .ok()
        .and_then(|data| gmod_audio::clip::decode(&data, &extension).ok())
        .map_or(0.0, |clip| clip.frames() as f64 / f64::from(clip.rate.max(1)));
    host(lua).channels.lengths.borrow_mut().insert(file.to_string(), seconds);
    seconds
}

/// Where a channel has got to, in seconds of the file.
fn time(lua: &Lua, state: &mut State) -> f64 {
    let length = *state.length.get_or_insert_with(|| length(lua, &state.file));
    if !state.playing {
        return state.from;
    }
    let at = state.from + (now(lua) - state.since) * f64::from(state.rate);
    if state.looping && length > 0.0 {
        at % length
    } else {
        if at >= length {
            state.playing = false;
            state.from = length;
        }
        at.min(length)
    }
}

/// Starts a channel from where it is.
fn start(lua: &Lua, id: u64, state: &mut State) {
    push(
        lua,
        SoundEvent::Play(SoundPlay {
            name: format!("bass:{id}"),
            wave: state.file.clone(),
            entity: None,
            position: state.three_d.then_some(state.position),
            volume: state.volume.clamp(0.0, 1.0),
            pitch: state.rate.clamp(0.01, 10.0),
            level: if state.three_d { gmod_audio::mixer::level_for_distance(state.fade.0) } else { 0.0 },
            channel: 0,
            patch: Some(id),
            unheard_by: None,
            looping: Some(state.looping),
            start: state.from as f32,
        }),
    );
    state.playing = true;
    state.paused = false;
    state.since = now(lua);
}

/// Stops what plays of a channel, keeping where it got to.
fn halt(lua: &Lua, id: u64, state: &mut State) {
    if state.playing {
        state.from = time(lua, state);
        push(lua, SoundEvent::StopPatch(id));
        state.playing = false;
    }
}

fn this(a: &Args) -> mlua::Result<Channel> {
    a.userdata::<Channel>(1, NAME)
}

/// `sound.PlayFile(path, flags, callback)`: a channel of a file of the game (`sound/...`),
/// given to the callback on the next frame; it plays at once unless the flags say `noplay`,
/// in the world with `3d`.
fn play_file(a: &Args) -> mlua::Result<Value> {
    let file = a.string(1)?.replace('\\', "/").trim_start_matches('/').to_ascii_lowercase();
    let flags = a.opt_string(2)?.unwrap_or_default().to_ascii_lowercase();
    let Some(Value::Function(callback)) = a.values.get(2).cloned() else { return Ok(Value::Nil) };
    let lua = a.lua;
    let known = ["wav", "mp3", "ogg"].iter().any(|e| file.ends_with(&format!(".{e}")));
    let result = if !host(lua).vfs.exists(&file, "GAME") {
        Err((BASS_ERROR_FILEOPEN, "BASS_ERROR_FILEOPEN"))
    } else if !known {
        Err((BASS_ERROR_FILEFORM, "BASS_ERROR_FILEFORM"))
    } else {
        let id = host(lua).sounds.new_number();
        let mut state = State {
            file,
            volume: 1.0,
            rate: 1.0,
            pan: 0.0,
            position: Vector::default(),
            three_d: flags.split_whitespace().any(|f| f == "3d"),
            looping: false,
            fade: (DEFAULT_FADE, 1e9),
            playing: false,
            paused: false,
            since: 0.0,
            from: 0.0,
            length: None,
            gone: false,
        };
        if !flags.split_whitespace().any(|f| f == "noplay") {
            start(lua, id, &mut state);
        }
        host(lua).channels.channels.borrow_mut().insert(id, state);
        Ok(id)
    };
    host(lua).channels.waiting.borrow_mut().push((callback, result));
    Ok(Value::Nil)
}

/// The callbacks of `PlayFile` made since the last frame.
pub(crate) fn deliver(lua: &Lua) {
    let waiting = std::mem::take(&mut *host(lua).channels.waiting.borrow_mut());
    for (callback, result) in waiting {
        let args = match result {
            Ok(id) => lua.create_userdata(Channel(id)).map(|ud| MultiValue::from_iter([Value::UserData(ud)])),
            Err((code, name)) => {
                lua.create_string(name).map(|name| MultiValue::from_iter([Value::Nil, Value::Integer(code), Value::String(name)]))
            }
        };
        if let Ok(args) = args {
            let _ = crate::files::call_reporting(lua, callback, args);
        }
    }
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    let sound: Table = match globals.raw_get::<Option<Table>>("sound")? {
        Some(t) => t,
        None => {
            let t = lua.create_table()?;
            globals.raw_set("sound", &t)?;
            t
        }
    };
    let def_in = |table: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<Value>| -> mlua::Result<()> {
        table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
    };
    def_in(&sound, "PlayFile", play_file)?;
    // streams from the internet: BASS cannot open them here
    def_in(&sound, "PlayURL", |a| {
        if let Some(Value::Function(callback)) = a.values.get(2).cloned() {
            host(a.lua).channels.waiting.borrow_mut().push((callback, Err((BASS_ERROR_FILEOPEN, "BASS_ERROR_FILEOPEN"))));
        }
        Ok(Value::Nil)
    })?;

    let meta = crate::meta::meta_table(lua, NAME, crate::meta::type_id::SOUNDHANDLE)?;
    let def = |name: &'static str, f: fn(&Args) -> mlua::Result<Value>| def_in(&meta, name, f);
    def("IsValid", |a| {
        let Some(Value::UserData(ud)) = a.values.front() else { return Ok(Value::Boolean(false)) };
        let Ok(channel) = ud.borrow::<Channel>() else { return Ok(Value::Boolean(false)) };
        Ok(Value::Boolean(with(a.lua, *channel, |_| ()).is_some()))
    })?;
    def("Play", |a| {
        let channel = this(a)?;
        with(a.lua, channel, |state| {
            if !state.playing {
                // played to its end: again from the start
                if !state.paused && state.length.is_some_and(|l| state.from >= l) {
                    state.from = 0.0;
                }
                start(a.lua, channel.0, state);
            }
        });
        Ok(Value::Nil)
    })?;
    def("Pause", |a| {
        let channel = this(a)?;
        with(a.lua, channel, |state| {
            if state.playing {
                halt(a.lua, channel.0, state);
                state.paused = true;
            }
        });
        Ok(Value::Nil)
    })?;
    def("Stop", |a| {
        let channel = this(a)?;
        with(a.lua, channel, |state| {
            halt(a.lua, channel.0, state);
            state.paused = false;
            state.from = 0.0;
        });
        Ok(Value::Nil)
    })?;
    def("GetState", |a| {
        let channel = this(a)?;
        let state = with(a.lua, channel, |state| {
            let _ = time(a.lua, state);
            if state.playing {
                PLAYING
            } else if state.paused {
                PAUSED
            } else {
                STOPPED
            }
        });
        Ok(Value::Integer(state.unwrap_or(STOPPED)))
    })?;
    def("SetVolume", |a| {
        let (channel, volume) = (this(a)?, a.number(2)? as f32);
        with(a.lua, channel, |state| {
            state.volume = volume.max(0.0);
            if state.playing {
                push(a.lua, SoundEvent::ChangePatch { patch: channel.0, volume: state.volume.min(1.0), pitch: state.rate });
            }
        });
        Ok(Value::Nil)
    })?;
    def("GetVolume", |a| Ok(Value::Number(f64::from(with(a.lua, this(a)?, |s| s.volume).unwrap_or(0.0)))))?;
    def("SetPlaybackRate", |a| {
        let (channel, rate) = (this(a)?, a.number(2)? as f32);
        with(a.lua, channel, |state| {
            // where it got to at the old speed
            state.from = time(a.lua, state);
            state.since = now(a.lua);
            state.rate = rate.clamp(0.01, 10.0);
            if state.playing {
                push(a.lua, SoundEvent::ChangePatch { patch: channel.0, volume: state.volume.min(1.0), pitch: state.rate });
            }
        });
        Ok(Value::Nil)
    })?;
    def("GetPlaybackRate", |a| Ok(Value::Number(f64::from(with(a.lua, this(a)?, |s| s.rate).unwrap_or(1.0)))))?;
    def("SetPos", |a| {
        let (channel, position) = (this(a)?, a.userdata::<Vector>(2, "Vector")?);
        with(a.lua, channel, |state| {
            state.position = position;
            if state.playing && state.three_d {
                push(a.lua, SoundEvent::MovePatch { patch: channel.0, position });
            }
        });
        Ok(Value::Nil)
    })?;
    def("GetPos", |a| {
        let position = with(a.lua, this(a)?, |s| s.position).unwrap_or_default();
        Ok(Value::UserData(a.lua.create_userdata(position)?))
    })?;
    def("Set3DEnabled", |a| {
        let (channel, on) = (this(a)?, a.boolean(2));
        with(a.lua, channel, |state| state.three_d = on);
        Ok(Value::Nil)
    })?;
    def("Is3D", |a| Ok(Value::Boolean(with(a.lua, this(a)?, |s| s.three_d).unwrap_or(false))))?;
    def("Set3DFadeDistance", |a| {
        let channel = this(a)?;
        let (min, max) = (a.opt_number(2)?.unwrap_or(0.0) as f32, a.opt_number(3)?.unwrap_or(0.0) as f32);
        with(a.lua, channel, |state| {
            // 0 or less keeps what there was
            if min > 0.0 {
                state.fade.0 = min;
            }
            if max > 0.0 {
                state.fade.1 = max;
            }
        });
        Ok(Value::Nil)
    })?;
    meta.raw_set(
        "Get3DFadeDistance",
        lua.create_function(|lua, values: MultiValue| {
            let a = Args::new(lua, values, "Get3DFadeDistance");
            let (min, max) = with(lua, this(&a)?, |s| s.fade).unwrap_or((DEFAULT_FADE, 1e9));
            Ok((min, max))
        })?,
    )?;
    def("EnableLooping", |a| {
        let (channel, on) = (this(a)?, a.boolean(2));
        with(a.lua, channel, |state| {
            if state.looping != on {
                state.from = time(a.lua, state);
                state.since = now(a.lua);
                state.looping = on;
                // what plays now plays on as it is told
                if state.playing {
                    halt(a.lua, channel.0, state);
                    start(a.lua, channel.0, state);
                }
            }
        });
        Ok(Value::Nil)
    })?;
    def("IsLooping", |a| Ok(Value::Boolean(with(a.lua, this(a)?, |s| s.looping).unwrap_or(false))))?;
    def("SetTime", |a| {
        let (channel, seconds) = (this(a)?, a.number(2)?.max(0.0));
        with(a.lua, channel, |state| {
            let length = *state.length.get_or_insert_with(|| length(a.lua, &state.file));
            let playing = state.playing;
            halt(a.lua, channel.0, state);
            state.from = seconds.min(length);
            if playing {
                start(a.lua, channel.0, state);
            }
        });
        Ok(Value::Nil)
    })?;
    def("GetTime", |a| Ok(Value::Number(with(a.lua, this(a)?, |s| time(a.lua, s)).unwrap_or(0.0))))?;
    def("GetLength", |a| {
        let seconds = with(a.lua, this(a)?, |s| *s.length.get_or_insert_with(|| length(a.lua, &s.file)));
        Ok(Value::Number(seconds.unwrap_or(0.0)))
    })?;
    def("GetFileName", |a| Ok(Value::String(a.lua.create_string(with(a.lua, this(a)?, |s| s.file.clone()).unwrap_or_default())?)))?;
    def("SetPan", |a| {
        let (channel, pan) = (this(a)?, a.number(2)? as f32);
        with(a.lua, channel, |state| state.pan = pan.clamp(-1.0, 1.0));
        Ok(Value::Nil)
    })?;
    def("GetPan", |a| Ok(Value::Number(f64::from(with(a.lua, this(a)?, |s| s.pan).unwrap_or(0.0)))))?;
    def("IsOnline", |_| Ok(Value::Boolean(false)))?;
    def("IsBlockStreamed", |_| Ok(Value::Boolean(false)))?;
    // what plays is not listened to: silence
    meta.raw_set("GetLevel", lua.create_function(|_, _: MultiValue| Ok((0.0, 0.0)))?)?;
    def("FFT", |a| {
        if let Some(Value::Table(t)) = a.values.get(1) {
            for i in 1..=256 {
                t.raw_set(i, 0.0)?;
            }
        }
        Ok(Value::Integer(0))
    })?;
    Ok(())
}
