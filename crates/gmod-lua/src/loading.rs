//! The loading screen, as the engine drives the menu's (`lua/menu/loading.lua`). While a map
//! loads or a server is joined, the menu's load panel (`GetLoadPanel()`) is up over the menu:
//! activated (`OnActivate`, which opens `GetDefaultLoadingHTML()`), told what is being joined
//! (`GameDetails`: the server, its loading screen `sv_loadingurl`, the map, the gamemode), and
//! reading each frame what goes on (`GetLoadStatus`, `NumDownloadables`, `GetDownloadables`);
//! once the game is there it is deactivated (`OnDeactivate`) and goes.

use std::cell::{Cell, RefCell};

use mlua::{Function, Lua, MultiValue, Value};

use crate::host;

/// The page the menu shows while loading.
const DEFAULT_HTML: &str = "asset://garrysmod/html/loading.html";

/// What the program says of the loading going on.
#[derive(Default)]
pub(crate) struct Loading {
    active: Cell<bool>,
    status: RefCell<Option<String>>,
    downloads: RefCell<Option<Vec<String>>>,
    /// The game being loaded is this computer's own (`IsHostingGame`).
    hosting: Cell<bool>,
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    globals.raw_set("GetDefaultLoadingHTML", lua.create_function(|_, ()| Ok(DEFAULT_HTML))?)?;
    globals.raw_set("GetLoadStatus", lua.create_function(|lua, ()| Ok(host(lua).loading.status.borrow().clone()))?)?;
    globals.raw_set(
        "NumDownloadables",
        lua.create_function(|lua, ()| Ok(host(lua).loading.downloads.borrow().as_ref().map(|d| d.len() as i64)))?,
    )?;
    globals.raw_set(
        "GetDownloadables",
        lua.create_function(|lua, ()| {
            let downloads = host(lua).loading.downloads.borrow().clone();
            downloads.map(|d| lua.create_sequence_from(d)).transpose()
        })?,
    )?;
    globals.raw_set("IsHostingGame", lua.create_function(|lua, ()| Ok(host(lua).loading.hosting.get()))?)?;
    Ok(())
}

/// The load panel goes up (`OnActivate`) or down (`OnDeactivate`).
fn panel(lua: &Lua, up: bool) {
    let script = "local up = ...
        if not GetLoadPanel then return end
        local p = GetLoadPanel()
        if p == nil or (p.IsValid and not p:IsValid()) then return end
        if up then
            -- over the main menu, which is a popup itself
            p:SetVisible(true) p:MakePopup() p:MoveToFront()
            if p.OnActivate then p:OnActivate() end
        else
            if p.OnDeactivate then p:OnDeactivate() end
            p:SetVisible(false)
        end";
    match lua.load(script).set_name("=[openGMOD loading]").into_function() {
        Ok(f) => {
            let _ = crate::files::call_reporting(lua, f, MultiValue::from_iter([Value::Boolean(up)]));
        }
        Err(e) => host(lua).report(crate::ScriptError::from_lua(&e)),
    }
}

/// The program's side of the loading screen (the menu's).
impl crate::Runtime {
    /// A map starts loading - the player's own game (`hosting`) or a server's: the load panel
    /// comes up.
    pub fn begin_loading(&self, hosting: bool) {
        let loading = &host(&self.lua).loading;
        loading.active.set(true);
        loading.hosting.set(hosting);
        *loading.status.borrow_mut() = Some("Loading".to_string());
        *loading.downloads.borrow_mut() = None;
        panel(&self.lua, true);
    }

    /// What is being joined: `GameDetails` (the server's name, its loading screen, the map,
    /// how many play, the gamemode).
    pub fn game_details(&self, server: &str, loading_url: &str, map: &str, max_players: u32, gamemode: &str) {
        let Ok(Some(details)) = self.lua.globals().get::<Option<Function>>("GameDetails") else { return };
        let s = |text: &str| self.lua.create_string(text).map(Value::String);
        let args = (|| -> mlua::Result<MultiValue> {
            Ok(MultiValue::from_iter([
                s(server)?,
                s(loading_url)?,
                s(map)?,
                Value::Integer(i64::from(max_players)),
                Value::Integer(i64::from(max_players)),
                s("")?,
                s(gamemode)?,
            ]))
        })();
        if let Ok(args) = args {
            let _ = crate::files::call_reporting(&self.lua, details, args);
        }
    }

    /// What the loading does now (`GetLoadStatus`): `Downloading <file>`, `Loading map`...
    pub fn set_load_status(&self, status: &str) {
        *host(&self.lua).loading.status.borrow_mut() = Some(status.to_string());
    }

    /// The files the server has the client fetch (`GetDownloadables`).
    pub fn set_downloadables(&self, files: Vec<String>) {
        *host(&self.lua).loading.downloads.borrow_mut() = Some(files);
    }

    /// The game is there, or the loading failed: the load panel goes.
    pub fn end_loading(&self) {
        let loading = &host(&self.lua).loading;
        if !loading.active.replace(false) {
            return;
        }
        *loading.status.borrow_mut() = None;
        *loading.downloads.borrow_mut() = None;
        loading.hosting.set(false);
        panel(&self.lua, false);
    }

    /// Whether the loading screen is up.
    pub fn loading(&self) -> bool {
        host(&self.lua).loading.active.get()
    }
}
