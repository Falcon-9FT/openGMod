//! The console: ConVars, console commands, and running them.
//!
//! ConVars hold strings and read them as numbers the way the engine does (`atof`: the number
//! at the start, else 0). A changed ConVar is announced to Lua through
//! `cvars.OnConVarChanged(name, old, new)`, as the engine does. Commands run from Lua
//! (`RunConsoleCommand`, `game.ConsoleCommand`) wait in a buffer for the next frame, as in the
//! engine; commands of Lua end in `concommand.Run`.

use std::cell::RefCell;
use std::collections::BTreeMap;

use mlua::{AnyUserData, Lua, MultiValue, Table, UserData, UserDataFields, UserDataMethods, Value};

use crate::args::Args;
use crate::entity::EntityHandle;
use crate::meta::{self, type_id};
use crate::{ScriptError, files, host};

/// ConVars of the engine itself that the game's Lua reads.
const ENGINE_CONVARS: &[(&str, &str, &str)] = &[
    ("sv_cheats", "0", "Allow cheats on server"),
    ("sv_gravity", "600", "World gravity."),
    ("sv_friction", "8", "World friction."),
    ("sv_stopspeed", "10", "Minimum stopping speed when on ground."),
    ("sv_accelerate", "10", "Linear acceleration amount (old value is 5.6)"),
    ("sv_airaccelerate", "10", ""),
    ("sv_maxvelocity", "3500", "Maximum speed any ballistically moving object is allowed to attain per axis."),
    ("sv_noclipspeed", "5", ""),
    ("sv_noclipaccelerate", "5", ""),
    ("sv_lan", "0", "Server is a lan server (no heartbeat, no authentication, no non-class C addresses)"),
    ("hostname", "openGMOD", "Hostname for server."),
    ("host_map", "", "The map being played (its file)"),
    ("sv_password", "", "Server password for entry into multiplayer games"),
    ("developer", "0", "Set developer message level"),
    ("sv_allowcslua", "0", "Allow clients to run Lua of their own"),
    ("sv_loadingurl", "", "The server's loading screen: an address (%s the player's SteamID64, %m the map)"),
    ("cl_enable_loadingurl", "1", "Show the loading screens of servers"),
    ("snd_musicvolume", "1", "Music volume"),
    ("gamemode", "sandbox", "The gamemode the next map starts with"),
    ("skill", "1", "Game skill level (1-3)."),
    ("deathmatch", "1", "Running a deathmatch server."),
    ("coop", "0", "Cooperative play."),
    ("crosshair", "1", ""),
    // the map's sky, from its `worldspawn` ([`set_value`] when the map spawns)
    ("sv_skyname", "sky_day01_01", "Current name of the skybox texture"),
    // the NPC menu of the spawn menu's bar
    ("ai_disabled", "0", "Disables all NPC thinking"),
    ("ai_ignoreplayers", "0", "NPCs ignore players"),
    ("ai_serverragdolls", "0", "Ragdolls of dead NPCs stay on the server, as physics"),
    ("npc_citizen_auto_player_squad", "1", "Citizens join the player's squad"),
];

/// ConVars of a client's engine that the game's Lua reads.
const CLIENT_CONVARS: &[(&str, &str, &str)] = &[
    ("cl_drawhud", "1", "Enable the rendering of the hud"),
    ("fov_desired", "75", "Sets the base field-of-view."),
    ("sensitivity", "3", "Mouse sensitivity."),
    ("m_pitch", "0.022", "Mouse pitch factor."),
    ("m_yaw", "0.022", "Mouse yaw factor."),
    ("cl_showfps", "0", "Draw fps meter at top of screen"),
    ("r_drawviewmodel", "1", ""),
    ("hud_fastswitch", "0", ""),
    ("mat_fullbright", "0", ""),
    ("r_shadows", "1", ""),
    ("r_drawdetailprops", "1", ""),
    ("name", "Player", "Current user name"),
    ("hud_saytext_time", "12", "How long lines of chat show"),
    ("cl_chatfilters", "63", "What the chat shows"),
    // addons read it for their own translations (`GetConVar("gmod_language"):GetString()`)
    ("gmod_language", "en", "Changes language of the game"),
];

/// A ConVar of a client the server is told of (`GetInfo`).
const FCVAR_USERINFO: i64 = 512;

struct ConVar {
    name: String,
    default: String,
    value: String,
    help: String,
    flags: i64,
    min: Option<f64>,
    max: Option<f64>,
}

#[derive(Default)]
pub(crate) struct Console {
    convars: RefCell<Vec<ConVar>>,
    by_name: RefCell<BTreeMap<String, usize>>,
    /// Commands of Lua (`AddConsoleCommand`), lower case.
    commands: RefCell<BTreeMap<String, (String, i64)>>,
    /// Lines waiting for the next frame.
    pub(crate) buffer: RefCell<Vec<String>>,
}

/// A ConVar as Lua holds it: its index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConVarHandle(usize);

/// The number at the start of a string, as C's `atof` reads it; 0 when there is none.
pub fn atof(text: &str) -> f64 {
    let text = text.trim_start();
    let bytes = text.as_bytes();
    let mut end = 0;
    let mut longest = 0;
    if end < bytes.len() && matches!(bytes[end], b'+' | b'-') {
        end += 1;
    }
    let mut digits = false;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
        digits = true;
    }
    if end < bytes.len() && bytes[end] == b'.' {
        end += 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
            digits = true;
        }
    }
    if digits {
        longest = end;
        if end < bytes.len() && matches!(bytes[end], b'e' | b'E') {
            let mut e = end + 1;
            if e < bytes.len() && matches!(bytes[e], b'+' | b'-') {
                e += 1;
            }
            let start = e;
            while e < bytes.len() && bytes[e].is_ascii_digit() {
                e += 1;
            }
            if e > start {
                longest = e;
            }
        }
    }
    text[..longest].parse().unwrap_or(0.0)
}

/// A number as the engine stores it in a ConVar.
fn number_string(n: f64) -> String {
    if n == n.trunc() && n.abs() < 1e15 { format!("{}", n as i64) } else { format!("{n}") }
}

impl UserData for ConVarHandle {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("MetaName", "ConVar");
        fields.add_meta_field("MetaID", type_id::CONVAR);
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function("__index", |lua, (_, key): (AnyUserData, Value)| meta::lookup(lua, "ConVar", key));
        methods.add_meta_function("__tostring", |lua, ud: AnyUserData| {
            let handle = *ud.borrow::<ConVarHandle>()?;
            Ok(format!("ConVar [{}]", host(lua).console.convars.borrow()[handle.0].name))
        });
        methods.add_meta_function("__eq", |_, (a, b): (AnyUserData, AnyUserData)| {
            Ok(matches!((a.borrow::<ConVarHandle>(), b.borrow::<ConVarHandle>()), (Ok(a), Ok(b)) if *a == *b))
        });
    }
}

fn find(lua: &Lua, name: &str) -> Option<ConVarHandle> {
    host(lua).console.by_name.borrow().get(&name.to_ascii_lowercase()).copied().map(ConVarHandle)
}

/// Makes a ConVar, or finds the one of that name.
fn create(lua: &Lua, name: &str, default: &str, help: &str, flags: i64, min: Option<f64>, max: Option<f64>) -> ConVarHandle {
    if let Some(handle) = find(lua, name) {
        return handle;
    }
    let console = &host(lua).console;
    let mut convars = console.convars.borrow_mut();
    convars.push(ConVar {
        name: name.to_string(),
        default: default.to_string(),
        value: default.to_string(),
        help: help.to_string(),
        flags,
        min,
        max,
    });
    let index = convars.len() - 1;
    drop(convars);
    console.by_name.borrow_mut().insert(name.to_ascii_lowercase(), index);
    if flags & FCVAR_USERINFO != 0 {
        send_user_info(lua, name, default);
    }
    ConVarHandle(index)
}

/// A client tells the server a value of its user info, as it does when it joins and when the
/// value changes.
fn send_user_info(lua: &Lua, name: &str, value: &str) {
    let host = host(lua);
    if host.realm == crate::Realm::Client {
        let mut user_info = host.client.user_info.borrow_mut();
        user_info.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
        user_info.push((name.to_string(), value.to_string()));
    }
}

/// A word of a console line, in quotes when it has to be.
pub(crate) fn quoted(word: &str) -> String {
    if word.is_empty() || word.contains(|c: char| c.is_whitespace() || c == ';' || c == '"') {
        format!("\"{}\"", word.replace('"', "'"))
    } else {
        word.to_string()
    }
}

/// Sets a ConVar, clamped to its limits, and tells Lua when the value changed.
fn set(lua: &Lua, handle: ConVarHandle, value: &str) {
    let changed = {
        let console = &host(lua).console;
        let mut convars = console.convars.borrow_mut();
        let convar = &mut convars[handle.0];
        let mut value = value.to_string();
        if convar.min.is_some() || convar.max.is_some() {
            let n = atof(&value);
            let clamped = n.max(convar.min.unwrap_or(f64::NEG_INFINITY)).min(convar.max.unwrap_or(f64::INFINITY));
            if clamped != n {
                value = number_string(clamped);
            }
        }
        if convar.value == value {
            None
        } else {
            let old = std::mem::replace(&mut convar.value, value.clone());
            Some((convar.name.clone(), old, value, convar.flags & FCVAR_USERINFO != 0))
        }
    };
    if let Some((name, old, new, user_info)) = changed {
        if user_info {
            send_user_info(lua, &name, &new);
        }
        // another language: its phrases from now on
        if name.eq_ignore_ascii_case("gmod_language") && host(lua).realm != crate::Realm::Server {
            crate::client::load_phrases(lua);
        }
        let callback = || -> mlua::Result<Option<mlua::Function>> {
            let Some(cvars) = lua.globals().get::<Option<Table>>("cvars")? else { return Ok(None) };
            cvars.get::<Option<mlua::Function>>("OnConVarChanged")
        };
        if let Ok(Some(callback)) = callback() {
            let args = (name, old, new);
            if let Ok(args) = mlua::IntoLuaMulti::into_lua_multi(args, lua) {
                let _ = files::call_reporting(lua, callback, args);
            }
        }
    }
}

/// Sets a ConVar of that name, if there is one.
pub(crate) fn set_value(lua: &Lua, name: &str, value: &str) {
    if let Some(handle) = find(lua, name) {
        set(lua, handle, value);
    }
}

/// The value of a ConVar, if there is one of that name.
pub(crate) fn value(lua: &Lua, name: &str) -> Option<String> {
    find(lua, name).map(|handle| with(lua, handle, |c| c.value.clone()))
}

fn with<R>(lua: &Lua, handle: ConVarHandle, f: impl FnOnce(&ConVar) -> R) -> R {
    f(&host(lua).console.convars.borrow()[handle.0])
}

/// FCVAR flags: a number, or a table of numbers to combine; a boolean, which some addons give,
/// is none, as GMod takes it.
fn flags_arg(args: &Args, position: usize) -> mlua::Result<i64> {
    match args.get(position) {
        Some(Value::Boolean(_)) => Ok(0),
        Some(Value::Table(t)) => {
            let mut flags = 0i64;
            for flag in t.sequence_values::<f64>() {
                flags |= flag? as i64;
            }
            Ok(flags)
        }
        _ => Ok(args.opt_number(position)?.unwrap_or(0.0) as i64),
    }
}

/// Splits a console line into commands (`;` and line breaks) and those into words, with
/// quoted words kept whole.
pub fn split_commands(text: &str) -> Vec<Vec<String>> {
    let mut commands = Vec::new();
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut in_word = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                in_word = true;
            }
            ';' | '\n' | '\r' if !quoted => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
                if !words.is_empty() {
                    commands.push(std::mem::take(&mut words));
                }
            }
            '/' if !quoted && chars.peek() == Some(&'/') => {
                // a comment, to the end of the line
                while chars.peek().is_some_and(|c| *c != '\n') {
                    chars.next();
                }
            }
            c if c.is_whitespace() && !quoted => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            c => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(word);
    }
    if !words.is_empty() {
        commands.push(words);
    }
    commands
}

/// Whether there is a ConVar, a command of Lua or one of the chat of that name.
pub(crate) fn is_known(lua: &Lua, name: &str) -> bool {
    find(lua, name).is_some()
        || host(lua).console.commands.borrow().contains_key(&name.to_ascii_lowercase())
        || crate::chat::is_command(lua, name)
}

/// Runs one command now: sets a ConVar, or calls a command of Lua.
pub(crate) fn execute(lua: &Lua, words: &[String]) {
    execute_as(lua, words, EntityHandle::NULL);
}

/// Runs a command typed by `player` (`NULL`: the server's console), whom the commands of
/// `concommand.Add` get.
pub(crate) fn execute_as(lua: &Lua, words: &[String], player: EntityHandle) {
    let Some((name, arguments)) = words.split_first() else { return };
    if let Some(handle) = find(lua, name) {
        match arguments.first() {
            Some(value) => set(lua, handle, value),
            None => {
                let (name, value, default, help) =
                    with(lua, handle, |c| (c.name.clone(), c.value.clone(), c.default.clone(), c.help.clone()));
                host(lua).print(&format!("\"{name}\" = \"{value}\" ( def. \"{default}\" )\n - {help}\n"));
            }
        }
        return;
    }
    let known = host(lua).console.commands.borrow().contains_key(&name.to_ascii_lowercase());
    if !known && crate::chat::command(lua, player, words) {
        return;
    }
    if !known {
        let host = host(lua);
        if host.realm != crate::Realm::Server {
            // the server's, as a client's console sends what it does not know; the menu's
            // go to the engine (`map`, `disconnect` ...)
            host.client.to_server.borrow_mut().push(words.iter().map(|w| quoted(w)).collect::<Vec<_>>().join(" "));
        } else {
            host.print(&format!("Unknown command \"{name}\"\n"));
        }
        return;
    }
    let run = || -> mlua::Result<Option<(mlua::Function, MultiValue)>> {
        let Some(concommand) = lua.globals().get::<Option<Table>>("concommand")? else { return Ok(None) };
        let Some(run) = concommand.get::<Option<mlua::Function>>("Run")? else { return Ok(None) };
        let args = (player, name.as_str(), lua.create_sequence_from(arguments.iter().map(String::as_str))?, arguments.join(" "));
        Ok(Some((run, mlua::IntoLuaMulti::into_lua_multi(args, lua)?)))
    };
    match run() {
        Ok(Some((run, args))) => {
            let _ = files::call_reporting(lua, run, args);
        }
        Ok(None) => {}
        Err(e) => host(lua).report(ScriptError::from_lua(&e)),
    }
}

/// Runs the commands that waited for this frame.
pub(crate) fn run_buffer(lua: &Lua) {
    let lines = std::mem::take(&mut *host(lua).console.buffer.borrow_mut());
    for line in lines {
        for words in split_commands(&line) {
            execute(lua, &words);
        }
    }
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    for (name, value, help) in ENGINE_CONVARS.iter().chain(crate::physgun::CONVARS) {
        create(lua, name, value, help, 0, None, None);
    }
    if host(lua).realm != crate::Realm::Server {
        for (name, value, help) in CLIENT_CONVARS {
            create(lua, name, value, help, 0, None, None);
        }
    }
    let globals = lua.globals();
    let def = |table: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<Value>| -> mlua::Result<()> {
        table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
    };
    def(&globals, "CreateConVar", |a| {
        let name = a.string(1)?;
        let default = a.string(2)?;
        let flags = flags_arg(a, 3)?;
        let help = a.opt_string(4)?.unwrap_or_default();
        let (min, max) = (a.opt_number(5)?, a.opt_number(6)?);
        Ok(Value::UserData(a.lua.create_userdata(create(a.lua, &name, &default, &help, flags, min, max))?))
    })?;
    def(&globals, "GetConVar_Internal", |a| match find(a.lua, &a.string(1)?) {
        Some(handle) => Ok(Value::UserData(a.lua.create_userdata(handle)?)),
        None => Ok(Value::Nil),
    })?;
    def(&globals, "ConVarExists", |a| {
        let name = a.string(1)?;
        let command = host(a.lua).console.commands.borrow().contains_key(&name.to_ascii_lowercase());
        Ok(Value::Boolean(command || find(a.lua, &name).is_some()))
    })?;
    def(&globals, "AddConsoleCommand", |a| {
        let name = a.string(1)?;
        let help = a.opt_string(2)?.unwrap_or_default();
        let flags = flags_arg(a, 3)?;
        host(a.lua).console.commands.borrow_mut().insert(name.to_ascii_lowercase(), (help, flags));
        Ok(Value::Nil)
    })?;
    def(&globals, "RunConsoleCommand", |a| {
        let mut words = vec![a.string(1)?];
        for position in 2..=a.values.len() {
            words.push(crate::console::join(a.lua, &MultiValue::from_iter(a.values.get(position - 1).cloned()), "")?);
        }
        let line = words.iter().map(|w| format!("\"{}\"", w.replace('"', ""))).collect::<Vec<_>>().join(" ");
        host(a.lua).console.buffer.borrow_mut().push(line);
        Ok(Value::Nil)
    })?;

    let meta = meta::meta_table(lua, "ConVar", type_id::CONVAR)?;
    fn this(a: &Args) -> mlua::Result<ConVarHandle> {
        a.userdata::<ConVarHandle>(1, "ConVar")
    }
    def(&meta, "GetString", |a| Ok(Value::String(a.lua.create_string(with(a.lua, this(a)?, |c| c.value.clone()))?)))?;
    def(&meta, "GetFloat", |a| Ok(Value::Number(with(a.lua, this(a)?, |c| atof(&c.value)))))?;
    def(&meta, "GetInt", |a| Ok(Value::Integer(with(a.lua, this(a)?, |c| atof(&c.value) as i64))))?;
    def(&meta, "GetBool", |a| Ok(Value::Boolean(with(a.lua, this(a)?, |c| atof(&c.value) as i64 != 0))))?;
    def(&meta, "GetDefault", |a| Ok(Value::String(a.lua.create_string(with(a.lua, this(a)?, |c| c.default.clone()))?)))?;
    def(&meta, "GetName", |a| Ok(Value::String(a.lua.create_string(with(a.lua, this(a)?, |c| c.name.clone()))?)))?;
    def(&meta, "GetHelpText", |a| Ok(Value::String(a.lua.create_string(with(a.lua, this(a)?, |c| c.help.clone()))?)))?;
    def(&meta, "GetFlags", |a| Ok(Value::Integer(with(a.lua, this(a)?, |c| c.flags))))?;
    def(&meta, "IsFlagSet", |a| {
        let flag = a.number(2)? as i64;
        Ok(Value::Boolean(with(a.lua, this(a)?, |c| c.flags & flag != 0)))
    })?;
    def(&meta, "GetMin", |a| Ok(with(a.lua, this(a)?, |c| c.min).map_or(Value::Nil, Value::Number)))?;
    def(&meta, "GetMax", |a| Ok(with(a.lua, this(a)?, |c| c.max).map_or(Value::Nil, Value::Number)))?;
    def(&meta, "SetString", |a| {
        let value = a.string(2)?;
        set(a.lua, this(a)?, &value);
        Ok(Value::Nil)
    })?;
    def(&meta, "SetFloat", |a| {
        let value = number_string(a.number(2)?);
        set(a.lua, this(a)?, &value);
        Ok(Value::Nil)
    })?;
    def(&meta, "SetInt", |a| {
        let value = (a.number(2)? as i64).to_string();
        set(a.lua, this(a)?, &value);
        Ok(Value::Nil)
    })?;
    def(&meta, "SetBool", |a| {
        let value = if a.boolean(2) { "1" } else { "0" };
        set(a.lua, this(a)?, value);
        Ok(Value::Nil)
    })?;
    def(&meta, "Revert", |a| {
        let handle = this(a)?;
        let default = with(a.lua, handle, |c| c.default.clone());
        set(a.lua, handle, &default);
        Ok(Value::Nil)
    })?;
    Ok(())
}
