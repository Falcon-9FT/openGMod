//! The engine's globals: which realm this is, the version, the enumerations; and the
//! libraries about the game being played: `gmod`, `game`, `engine`, `system`.

use std::cell::RefCell;

use mlua::{Lua, MultiValue, Table, Value};

use crate::args::Args;
use crate::entity::EntityHandle;
use crate::{Realm, host, timer};

/// The enumerations of GMod's Lua (`ACT_IDLE`, `TEAM_SPECTATOR`, `SENSORBONE.HIP` ...): the
/// engine defines them, the game's Lua uses them. From the GMod wiki; `scripts/wiki-api.py`
/// rebuilds the list.
const ENUMS: &str = include_str!("../data/enums.txt");

/// The version openGMOD answers to: the GMod release whose Lua it is written against.
pub const VERSION: u32 = 241029;
pub const VERSION_STR: &str = "2024.10.29";

pub(crate) fn install(lua: &Lua, realm: Realm) -> mlua::Result<()> {
    let globals = lua.globals();
    globals.raw_set("SERVER", realm == Realm::Server)?;
    globals.raw_set("CLIENT", realm != Realm::Server)?;
    globals.raw_set("MENU_DLL", realm == Realm::Menu)?;
    if realm == Realm::Server {
        globals.raw_set("GAME_DLL", true)?;
    }
    if realm == Realm::Client {
        globals.raw_set("CLIENT_DLL", true)?;
    }
    globals.raw_set("VERSION", VERSION)?;
    globals.raw_set("VERSIONSTR", VERSION_STR)?;
    globals.raw_set("BRANCH", "x86-64")?;
    // the bits of an entity's index: what `net.WriteEntity` writes
    globals.raw_set("MAX_EDICT_BITS", crate::entity::MAX_EDICTS.trailing_zeros())?;
    if realm == Realm::Menu {
        globals.raw_set("NETVERSIONSTR", VERSION_STR)?;
    }
    install_enums(lua, &globals)?;

    globals.raw_set("SysTime", lua.create_function(|lua, ()| Ok(host(lua).started.elapsed().as_secs_f64()))?)?;
    globals.raw_set("GetHostName", lua.create_function(|lua, ()| Ok(crate::convar::value(lua, "hostname").unwrap_or_default()))?)?;
    install_libraries(lua, &globals)?;
    Ok(())
}

/// The game being played: map, gamemode, players.
#[derive(Default)]
pub(crate) struct Game {
    pub map: RefCell<String>,
    pub gamemode_name: RefCell<String>,
    /// The table of the active gamemode (`GAMEMODE`).
    pub gamemode: RefCell<Option<Table>>,
    pub time_scale: std::cell::Cell<f64>,
    /// Gamemodes loaded, base first.
    pub chain: RefCell<Vec<String>>,
    /// What `DeriveGamemode` named while the gamemode being loaded ran.
    pub derived: RefCell<Vec<String>>,
    /// Player slots: entity indices 1 to this (`game.MaxPlayers`); 0 is 1.
    pub max_players: std::cell::Cell<u32>,
    /// A server without a player of its own at the screen (`game.IsDedicated`).
    pub dedicated: std::cell::Cell<bool>,
}

/// The active gamemode's table, or nil.
pub(crate) fn gamemode(lua: &Lua) -> mlua::Result<Value> {
    Ok(host(lua).game.gamemode.borrow().clone().map_or(Value::Nil, Value::Table))
}

fn def(lua: &Lua, table: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<Value>) -> mlua::Result<()> {
    table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
}

fn string(a: &Args, text: &str) -> mlua::Result<Value> {
    Ok(Value::String(a.lua.create_string(text)?))
}

/// The gamemodes of the installation: folders of `gamemodes/` with a `<name>.txt`.
pub(crate) fn gamemodes(lua: &Lua) -> Vec<(String, gmod_kv::Block)> {
    let vfs = host(lua).vfs.clone();
    let mut out = Vec::new();
    for name in vfs.find("gamemodes/*", "GAME").dirs {
        let Ok(bytes) = vfs.read(&format!("gamemodes/{name}/{name}.txt"), "GAME") else { continue };
        let (document, _) = gmod_kv::parse_lenient(&gmod_kv::decode_text(&bytes), gmod_kv::Options::default());
        if let Some(info) = document.root.iter().find_map(|pair| pair.value.as_block()) {
            out.push((name, info.clone()));
        }
    }
    out
}

fn install_libraries(lua: &Lua, globals: &Table) -> mlua::Result<()> {
    host(lua).game.time_scale.set(1.0);

    let gmod = lua.create_table()?;
    def(lua, &gmod, "GetGamemode", |a| gamemode(a.lua))?;
    globals.raw_set("gmod", gmod)?;

    let game = lua.create_table()?;
    def(lua, &game, "SinglePlayer", |a| Ok(Value::Boolean(crate::player::max_players(a.lua) == 1)))?;
    def(lua, &game, "MaxPlayers", |a| Ok(Value::Integer(crate::player::max_players(a.lua).into())))?;
    def(lua, &game, "IsDedicated", |a| Ok(Value::Boolean(host(a.lua).game.dedicated.get())))?;
    def(lua, &game, "GetMap", |a| string(a, &host(a.lua).game.map.borrow()))?;
    def(lua, &game, "GetIPAddress", |a| string(a, "loopback"))?;
    def(lua, &game, "GetWorld", |a| crate::entity::push(a.lua, EntityHandle::WORLD))?;
    def(lua, &game, "ConsoleCommand", |a| {
        let line = a.string(1)?;
        host(a.lua).console_buffer(line);
        Ok(Value::Nil)
    })?;
    def(lua, &game, "GetTimeScale", |a| Ok(Value::Number(host(a.lua).game.time_scale.get())))?;
    def(lua, &game, "SetTimeScale", |a| {
        host(a.lua).game.time_scale.set(a.number(1)?);
        Ok(Value::Nil)
    })?;
    def(lua, &game, "GetSkillLevel", |a| {
        let level = crate::convar::value(a.lua, "skill").map_or(1, |v| crate::convar::atof(&v) as i64);
        Ok(Value::Integer(level))
    })?;
    // particle and decal definitions are for the renderer, which does not read them yet
    def(lua, &game, "AddParticles", |_| Ok(Value::Nil))?;
    def(lua, &game, "AddDecal", |_| Ok(Value::Nil))?;
    globals.raw_set("game", game)?;

    let engine = lua.create_table()?;
    def(lua, &engine, "ActiveGamemode", |a| string(a, &host(a.lua).game.gamemode_name.borrow()))?;
    def(lua, &engine, "GetGamemodes", |a| {
        let list = a.lua.create_table()?;
        for (name, info) in gamemodes(a.lua) {
            let entry = a.lua.create_table()?;
            entry.raw_set("name", name.as_str())?;
            entry.raw_set("title", info.str("title").unwrap_or(&name))?;
            entry.raw_set("maps", info.str("maps").unwrap_or_default())?;
            entry.raw_set("menusystem", info.str("menusystem").is_some_and(|v| v != "0"))?;
            entry.raw_set("workshopid", info.str("workshopid").unwrap_or_default())?;
            list.raw_push(entry)?;
        }
        Ok(Value::Table(list))
    })?;
    def(lua, &engine, "GetAddons", |a| Ok(Value::Table(a.lua.create_table()?)))?;
    def(lua, &engine, "GetGames", |a| Ok(Value::Table(a.lua.create_table()?)))?;
    def(lua, &engine, "TickInterval", |_| Ok(Value::Number(timer::TICK_INTERVAL)))?;
    def(lua, &engine, "TickCount", |a| Ok(Value::Integer(host(a.lua).clock.ticks.get() as i64)))?;
    def(lua, &engine, "AbsoluteFrameTime", |a| Ok(Value::Number(host(a.lua).clock.frame_time.get())))?;
    def(lua, &engine, "ServerFrameTime", |a| Ok(Value::Number(host(a.lua).clock.frame_time.get())))?;
    def(lua, &engine, "IsPlayingDemo", |_| Ok(Value::Boolean(false)))?;
    def(lua, &engine, "IsRecordingDemo", |_| Ok(Value::Boolean(false)))?;
    def(lua, &engine, "LightStyle", |_| Ok(Value::Nil))?;
    globals.raw_set("engine", engine)?;

    let system = lua.create_table()?;
    def(lua, &system, "IsWindows", |_| Ok(Value::Boolean(cfg!(windows))))?;
    def(lua, &system, "IsLinux", |_| Ok(Value::Boolean(cfg!(target_os = "linux"))))?;
    def(lua, &system, "IsOSX", |_| Ok(Value::Boolean(cfg!(target_os = "macos"))))?;
    def(lua, &system, "AppTime", |a| Ok(Value::Number(host(a.lua).started.elapsed().as_secs_f64())))?;
    def(lua, &system, "UpTime", |a| Ok(Value::Number(host(a.lua).started.elapsed().as_secs_f64())))?;
    def(lua, &system, "SteamTime", |_| {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        Ok(Value::Integer(now as i64))
    })?;
    def(lua, &system, "IsWindowed", |_| Ok(Value::Boolean(true)))?;
    // the window asks for the player's attention: there is no one to tell but the window
    def(lua, &system, "FlashWindow", |_| Ok(Value::Nil))?;
    def(lua, &system, "HasFocus", |_| Ok(Value::Boolean(true)))?;
    // 255: no battery
    def(lua, &system, "BatteryPower", |_| Ok(Value::Integer(255)))?;
    // the country Steam would say the player is in: the one of the game's language
    def(lua, &system, "GetCountry", |a| {
        let language = crate::convar::value(a.lua, "gmod_language").unwrap_or_default().to_ascii_lowercase();
        let country = match language.as_str() {
            "ru" => "RU",
            "uk" => "UA",
            "de" => "DE",
            "fr" => "FR",
            "es-es" => "ES",
            "pt-br" => "BR",
            "pt-pt" => "PT",
            "pl" => "PL",
            "it" => "IT",
            "ja" => "JP",
            "ko" => "KR",
            "zh-cn" => "CN",
            "zh-tw" => "TW",
            "tr" => "TR",
            "cs" => "CZ",
            "nl" => "NL",
            "sv-se" => "SE",
            "fi" => "FI",
            "da" => "DK",
            "no" => "NO",
            "hu" => "HU",
            "bg" => "BG",
            "el" => "GR",
            "th" => "TH",
            "vi" => "VN",
            "en-pt" | "en" | "" => "US",
            other => return Ok(Value::String(a.lua.create_string(other.split('-').next_back().unwrap_or("US").to_ascii_uppercase())?)),
        };
        Ok(Value::String(a.lua.create_string(country)?))
    })?;
    globals.raw_set("system", system)?;
    Ok(())
}

/// The activities of the game's enumerations (`ACT_IDLE` ...), by name and number.
fn activities() -> &'static [(&'static str, i64)] {
    static LIST: std::sync::OnceLock<Vec<(&'static str, i64)>> = std::sync::OnceLock::new();
    LIST.get_or_init(|| {
        ENUMS
            .lines()
            .filter(|l| l.starts_with("ACT_"))
            .filter_map(|l| l.split_once(' ').and_then(|(name, value)| Some((name, value.parse().ok()?))))
            .collect()
    })
}

/// The number of an activity of the game's (`ACT_VM_IDLE`), ignoring case.
pub(crate) fn activity(name: &str) -> Option<i64> {
    activities().iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|&(_, id)| id)
}

/// The name of an activity of the game's by its number.
pub(crate) fn activity_name(id: i64) -> Option<&'static str> {
    activities().iter().find(|&&(_, n)| n == id).map(|&(name, _)| name)
}

fn install_enums(lua: &Lua, globals: &Table) -> mlua::Result<()> {
    for line in ENUMS.lines().filter(|l| !l.starts_with('#') && !l.is_empty()) {
        let Some((name, value)) = line.split_once(' ') else { continue };
        let Ok(value) = value.parse::<i64>() else { continue };
        match name.split_once('.') {
            Some((table, key)) => {
                let table = match globals.raw_get::<Value>(table)? {
                    Value::Table(t) => t,
                    _ => {
                        let t = lua.create_table()?;
                        globals.raw_set(table, &t)?;
                        t
                    }
                };
                table.raw_set(key, value)?;
            }
            None => globals.raw_set(name, value)?,
        }
    }
    Ok(())
}
