//! Starting the game as the engine does, after `includes/init.lua`:
//!
//! 1. autorun: `lua/autorun/*.lua`, then `lua/autorun/server/*.lua` or `lua/autorun/client/*.lua`;
//!    a client loads Derma before (`lua/derma/init.lua`) and its controls after:
//!    `lua/postprocess/`, `lua/vgui/`, `lua/matproxy/`, `lua/skins/`;
//! 2. the gamemode: its base first (the `"base"` of `<name>.txt`, or what `DeriveGamemode`
//!    names), each loaded into a fresh `GM` table and handed to `gamemode.Register`;
//!    `GAMEMODE` is the result;
//! 3. scripted entities, weapons and (on clients) effects: those of every gamemode in the
//!    chain, base first, then those of `lua/`; each into a fresh `ENT`, `SWEP` or `EFFECT`
//!    and registered with `scripted_ents`, `weapons` or `effects`, whose `OnLoaded` then
//!    runs (it fills the base classes `DEFINE_BASECLASS` hands out);
//! 4. the hooks `PreGamemodeLoaded` (before 2), `OnGamemodeLoaded` and `PostGamemodeLoaded`
//!    (after 3), `CreateTeams` and `Initialize`.

use mlua::{Function, Lua, MultiValue, Table, Value};

use crate::args::Args;
use crate::engine;
use crate::files::{self, include_from_engine};
use crate::{Realm, ScriptError, host};

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    lua.globals().raw_set(
        "DeriveGamemode",
        lua.create_function(|lua, values: MultiValue| {
            let a = Args::new(lua, values, "DeriveGamemode");
            let base = a.string(1)?;
            host(lua).game.derived.borrow_mut().push(base.clone());
            load_gamemode(lua, &base);
            Ok(())
        })?,
    )?;
    Ok(())
}

/// The `"base"` of a gamemode's `<name>.txt`.
fn base_of(lua: &Lua, name: &str) -> Option<String> {
    engine::gamemodes(lua)
        .into_iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .and_then(|(_, info)| info.str("base").map(str::to_string))
}

fn gamemode_module(lua: &Lua) -> mlua::Result<Option<Table>> {
    lua.globals().get::<Option<Table>>("gamemode")
}

fn registered(lua: &Lua, name: &str) -> Option<Table> {
    let get = gamemode_module(lua).ok()??.get::<Function>("Get").ok()?;
    get.call::<Option<Table>>(name).ok()?
}

/// Loads a gamemode and its bases into `gamemode.Register`. The chain, base first, is kept
/// for loading the gamemodes' entities.
fn load_gamemode(lua: &Lua, name: &str) {
    if registered(lua, name).is_some() || host(lua).game.chain.borrow().iter().any(|n| n == name) {
        return;
    }
    let folder = format!("gamemodes/{name}");
    if !host(lua).vfs.is_dir(&folder, "GAME") {
        host(lua).report(ScriptError::new(format!("Couldn't load gamemode '{name}' - it is not installed")));
        return;
    }
    let base = base_of(lua, name).filter(|b| !b.is_empty() && !b.eq_ignore_ascii_case(name) && name != "base");
    if let Some(base) = &base {
        load_gamemode(lua, base);
    }
    host(lua).game.chain.borrow_mut().push(name.to_string());

    let result = (|| -> mlua::Result<()> {
        let globals = lua.globals();
        let outer_gm: Value = globals.get("GM")?;
        let outer_derived = std::mem::take(&mut *host(lua).game.derived.borrow_mut());
        let gm = lua.create_table()?;
        gm.raw_set("Folder", folder.as_str())?;
        gm.raw_set("FolderName", name)?;
        globals.raw_set("GM", &gm)?;

        let file = if host(lua).realm == Realm::Server { "init.lua" } else { "cl_init.lua" };
        include_from_engine(lua, &format!("{name}/gamemode/{file}"));

        let derived = host(lua).game.derived.borrow().last().cloned().or(base);
        *host(lua).game.derived.borrow_mut() = outer_derived;
        let gm: Value = globals.get("GM")?;
        if let Some(module) = gamemode_module(lua)? {
            let register: Function = module.get("Register")?;
            let _ = files::call_reporting(lua, register, mlua::IntoLuaMulti::into_lua_multi((gm, name, derived), lua)?);
        }
        globals.raw_set("GM", outer_gm)?;
        Ok(())
    })();
    if let Err(e) = result {
        host(lua).report(ScriptError::from_lua(&e));
    }
}

/// Loads every script of one kind (`entities`, `weapons`, `effects`) found in `folder`.
fn load_scripts(lua: &Lua, folder: &str, kind: Kind) {
    let vfs = host(lua).vfs.clone();
    let realm = host(lua).realm;
    let found = vfs.find(&format!("{folder}/*"), "GAME");
    let mut scripts: Vec<(String, Vec<&str>)> = Vec::new();
    for file in found.files.iter().filter(|f| f.ends_with(".lua")) {
        scripts.push((file.trim_end_matches(".lua").to_string(), vec![file.as_str()]));
    }
    for dir in &found.dirs {
        let first = |names: &[&'static str]| names.iter().copied().find(|n| vfs.exists(&format!("{folder}/{dir}/{n}"), "GAME"));
        let file = match realm {
            Realm::Server => first(&["init.lua", "shared.lua"]),
            _ => first(&["cl_init.lua", "shared.lua"]),
        };
        if let Some(file) = file {
            scripts.push((dir.clone(), vec![file]));
        }
    }
    let inside = folder.split_once('/').map_or(folder, |(root, rest)| if root == "lua" || root == "gamemodes" { rest } else { folder });
    for (name, files) in scripts {
        let result = (|| -> mlua::Result<()> {
            let globals = lua.globals();
            let table = lua.create_table()?;
            let script_folder = format!("{inside}/{name}");
            table.raw_set("Folder", script_folder.as_str())?;
            if kind == Kind::Weapon {
                table.raw_set("Primary", lua.create_table()?)?;
                table.raw_set("Secondary", lua.create_table()?)?;
            }
            globals.raw_set(kind.global(), &table)?;
            for file in &files {
                let path = if *file == format!("{name}.lua") { format!("{inside}/{file}") } else { format!("{script_folder}/{file}") };
                include_from_engine(lua, &path);
            }
            let table: Value = globals.get(kind.global())?;
            if let Some(library) = globals.get::<Option<Table>>(kind.library())? {
                let register: Function = library.get("Register")?;
                let _ = files::call_reporting(lua, register, mlua::IntoLuaMulti::into_lua_multi((table, name.as_str()), lua)?);
            }
            globals.raw_set(kind.global(), Value::Nil)?;
            Ok(())
        })();
        if let Err(e) = result {
            host(lua).report(ScriptError::from_lua(&e));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Entity,
    Weapon,
    Effect,
}

impl Kind {
    fn folder(self) -> &'static str {
        match self {
            Kind::Entity => "entities",
            Kind::Weapon => "weapons",
            Kind::Effect => "effects",
        }
    }

    fn global(self) -> &'static str {
        match self {
            Kind::Entity => "ENT",
            Kind::Weapon => "SWEP",
            Kind::Effect => "EFFECT",
        }
    }

    fn library(self) -> &'static str {
        match self {
            Kind::Entity => "scripted_ents",
            Kind::Weapon => "weapons",
            Kind::Effect => "effects",
        }
    }
}

/// Runs the Lua files of a folder in name order.
fn run_folder(lua: &Lua, folder: &str) {
    let found = host(lua).vfs.find(&format!("lua/{folder}/*.lua"), "GAME");
    for file in found.files {
        include_from_engine(lua, &format!("{folder}/{file}"));
    }
}

/// Everything after `includes/init.lua`, up to and with the `Initialize` hook.
pub(crate) fn start(runtime: &crate::Runtime, gamemode: &str) {
    let lua = runtime.lua();
    let realm = runtime.realm();
    if realm == Realm::Client {
        include_from_engine(lua, "derma/init.lua");
    }
    run_folder(lua, "autorun");
    run_folder(lua, if realm == Realm::Server { "autorun/server" } else { "autorun/client" });
    if realm == Realm::Client {
        // Derma's controls and skins come after autorun: an addon's autorun cannot make a
        // `DFrame` as it loads, as in GMod
        for folder in ["postprocess", "vgui", "matproxy", "skins"] {
            run_folder(lua, folder);
        }
    }

    *host(lua).game.gamemode_name.borrow_mut() = gamemode.to_string();
    runtime.call_hook("PreGamemodeLoaded", MultiValue::new());
    load_gamemode(lua, gamemode);
    let active = registered(lua, gamemode);
    if let Some(gm) = &active {
        let _ = lua.globals().raw_set("GAMEMODE", gm);
    }
    *host(lua).game.gamemode.borrow_mut() = active;

    let chain = host(lua).game.chain.borrow().clone();
    let mut kinds = vec![Kind::Entity, Kind::Weapon];
    if realm != Realm::Server {
        kinds.push(Kind::Effect);
    }
    for &kind in &kinds {
        for name in &chain {
            load_scripts(lua, &format!("gamemodes/{name}/entities/{}", kind.folder()), kind);
        }
        load_scripts(lua, &format!("lua/{}", kind.folder()), kind);
    }
    // all are there: each library sets up the base classes (`DEFINE_BASECLASS`) now
    for kind in kinds {
        let on_loaded = lua
            .globals()
            .get::<Option<Table>>(kind.library())
            .ok()
            .flatten()
            .and_then(|l| l.get::<Option<Function>>("OnLoaded").ok().flatten());
        if let Some(f) = on_loaded {
            let _ = files::call_reporting(lua, f, MultiValue::new());
        }
    }
    // after the entities and weapons: sandbox makes its spawn menu then, of what is loaded
    runtime.call_hook("OnGamemodeLoaded", MultiValue::new());
    runtime.call_hook("PostGamemodeLoaded", MultiValue::new());
    runtime.call_hook("CreateTeams", MultiValue::new());
    runtime.call_hook("Initialize", MultiValue::new());
    // what `game.AddAmmoType` added by now is what there is
    crate::weapon::freeze_ammo(runtime.lua());
}
