//! The engine's globals: which realm this is, the version, the clocks, the enumerations.

use mlua::{Lua, Table, Value};

use crate::{Realm, host};

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
    if realm == Realm::Menu {
        globals.raw_set("NETVERSIONSTR", VERSION_STR)?;
    }
    install_enums(lua, &globals)?;

    globals.raw_set("SysTime", lua.create_function(|lua, ()| Ok(host(lua).started.elapsed().as_secs_f64()))?)?;
    Ok(())
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
