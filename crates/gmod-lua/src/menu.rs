//! What the engine gives the main menu: the addons and whether each is mounted, the games
//! that can be mounted, whether a game is running, and the commands that start and leave one
//! (they go to the program: [`Runtime::take_engine_commands`](crate::Runtime::take_engine_commands)).
//!
//! The Steam side - `steamworks`, `serverlist` - is what the menu needs of it without Steam:
//! the details of workshop items come from Steam's public web API; there are no servers to
//! list, no favourites, nothing to publish.

use std::cell::{Cell, RefCell};

use mlua::{Lua, MultiValue, Table, Value};

use crate::args::Args;
use crate::{Realm, host};

/// An addon, as `engine.GetAddons` lists it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Addon {
    pub title: String,
    /// Its file, as the game names it (`addons/foo.gma`, or the workshop download).
    pub file: String,
    /// Its workshop item; 0 for a `.gma` put into `addons/` by hand.
    pub workshop_id: u64,
    /// Whether it is (to be) mounted.
    pub mounted: bool,
    pub size: u64,
    /// Its tags, comma separated.
    pub tags: String,
    /// When it was last updated and first added (Unix time); 0 when not known.
    pub updated: i64,
    pub added: i64,
    pub models: u32,
}

impl Addon {
    /// The ID the menu knows it by: its workshop item, or for an addon from `addons/` a
    /// negative number of its own (the menu takes 0 and below as local addons).
    pub fn menu_id(&self, index: usize) -> String {
        if self.workshop_id != 0 { self.workshop_id.to_string() } else { format!("-{}", index + 1) }
    }
}

/// A game whose content can be mounted, as `engine.GetGames` lists it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MountableGame {
    pub depot: u32,
    pub title: String,
    pub folder: String,
    pub owned: bool,
    pub installed: bool,
    pub mounted: bool,
}

#[derive(Default)]
pub(crate) struct Menu {
    pub addons: RefCell<Vec<Addon>>,
    /// `steamworks.ApplyAddons` was called since the program last looked.
    pub applied: Cell<bool>,
    pub games: RefCell<Vec<MountableGame>>,
    pub in_game: Cell<bool>,
    /// Addons were not mounted at all (`-noaddons`), workshop ones were not (`-noworkshop`).
    pub no_addons: Cell<bool>,
}

fn def(lua: &Lua, table: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<Value>) -> mlua::Result<()> {
    table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
}

/// Calls a callback Lua gave; its errors are reported.
fn call_back(lua: &Lua, f: mlua::Function, args: impl mlua::IntoLuaMulti) {
    if let Ok(args) = args.into_lua_multi(lua) {
        let _ = crate::files::call_reporting(lua, f, args);
    }
}

fn nothing(_: &Args) -> mlua::Result<Value> {
    Ok(Value::Nil)
}

/// The library table `name`, made if there is none.
fn library(lua: &Lua, name: &str) -> mlua::Result<Table> {
    let globals = lua.globals();
    match globals.raw_get::<Option<Table>>(name)? {
        Some(t) => Ok(t),
        None => {
            let t = lua.create_table()?;
            globals.raw_set(name, &t)?;
            Ok(t)
        }
    }
}

/// The addon the menu means by `id`.
fn addon_index(lua: &Lua, id: &str) -> Option<usize> {
    let id = id.trim();
    let host = host(lua);
    let addons = host.menu.addons.borrow();
    addons.iter().enumerate().position(|(i, a)| a.menu_id(i) == id)
}

/// Lua that is the engine's in GMod but is easier said in Lua: workshop details over HTTP.
const MENU_LUA: &str = r#"
-- the news and the banned servers: none without Facepunch's API; the answer comes later, as
-- the engine's does
function GetAPIManifest( callback )
	timer.Simple( 0, function() callback( "{}" ) end )
end

local details = {}
function steamworks.FileInfo( id, callback )
	id = tostring( id )
	if ( details[ id ] ) then return callback( details[ id ] ) end
	HTTP( {
		url = "https://api.steampowered.com/ISteamRemoteStorage/GetPublishedFileDetails/v1/",
		method = "POST",
		parameters = { itemcount = "1", [ "publishedfileids[0]" ] = id },
		success = function( code, body )
			local data = util.JSONToTable( body or "", false, true )
			local item = data and data.response and data.response.publishedfiledetails and data.response.publishedfiledetails[ 1 ]
			if ( !item or tonumber( item.result ) != 1 ) then return callback( { error = item and item.result or code } ) end
			local tags = {}
			for _, tag in ipairs( item.tags or {} ) do tags[ #tags + 1 ] = tag.tag end
			local info = {
				id = id, title = item.title or "", description = item.description or "",
				owner = item.creator or "", ownername = "", previewid = item.preview_url, previewurl = item.preview_url,
				size = tonumber( item.file_size ) or 0, created = tonumber( item.time_created ) or 0,
				updated = tonumber( item.time_updated ) or 0, tags = table.concat( tags, "," ),
				banned = tonumber( item.banned ) == 1, installed = steamworks.IsSubscribed( id ), disabled = !steamworks.ShouldMountAddon( id ),
				children = {}
			}
			details[ id ] = info
			callback( info )
		end,
		failed = function( reason ) callback( { error = reason } ) end
	} )
end

-- the details give the address of the preview: it is its own download
function steamworks.Download( id, uncompress, callback )
	if ( isstring( id ) and id:find( "^https?://" ) ) then return callback( id ) end
	if ( callback ) then callback( nil ) end
end
"#;

pub(crate) fn install(lua: &Lua, realm: Realm) -> mlua::Result<()> {
    let engine = library(lua, "engine")?;
    def(lua, &engine, "GetAddons", |a| {
        let list = a.lua.create_table()?;
        for (i, addon) in host(a.lua).menu.addons.borrow().iter().enumerate() {
            let entry = a.lua.create_table()?;
            entry.raw_set("title", addon.title.as_str())?;
            entry.raw_set("file", addon.file.as_str())?;
            entry.raw_set("wsid", addon.menu_id(i))?;
            entry.raw_set("mounted", addon.mounted)?;
            entry.raw_set("downloaded", true)?;
            entry.raw_set("size", addon.size)?;
            entry.raw_set("tags", addon.tags.as_str())?;
            entry.raw_set("updated", addon.updated)?;
            entry.raw_set("timeadded", addon.added)?;
            entry.raw_set("models", addon.models)?;
            list.raw_push(entry)?;
        }
        Ok(Value::Table(list))
    })?;
    def(lua, &engine, "GetUserContent", |a| Ok(Value::Table(a.lua.create_table()?)))?;
    def(lua, &engine, "GetGames", |a| {
        let list = a.lua.create_table()?;
        for game in host(a.lua).menu.games.borrow().iter() {
            let entry = a.lua.create_table()?;
            entry.raw_set("depot", game.depot)?;
            entry.raw_set("title", game.title.as_str())?;
            entry.raw_set("folder", game.folder.as_str())?;
            entry.raw_set("owned", game.owned)?;
            entry.raw_set("installed", game.installed)?;
            entry.raw_set("mounted", game.mounted)?;
            list.raw_push(entry)?;
        }
        Ok(Value::Table(list))
    })?;
    if realm != Realm::Menu {
        return Ok(());
    }
    // mounting a game takes a restart in openGMOD: `cfg/mount.cfg` says what is mounted
    def(lua, &engine, "SetMounted", nothing)?;

    let globals = lua.globals();
    def(lua, &globals, "IsInGame", |a| Ok(Value::Boolean(host(a.lua).menu.in_game.get())))?;
    // two answers: no addons, no workshop addons
    globals.raw_set(
        "GetAddonStatus",
        lua.create_function(|lua, ()| {
            let none = host(lua).menu.no_addons.get();
            Ok((none, none))
        })?,
    )?;
    def(lua, &globals, "RunGameUICommand", |a| {
        let command = a.string(1)?;
        let line = match command.to_ascii_lowercase().as_str() {
            "quit" | "quitnoconfirm" => "quit",
            "disconnect" | "engine disconnect" => "disconnect",
            "resumegame" => "resume",
            _ => {
                host(a.lua).print(&format!("{command} is not in openGMOD yet\n"));
                return Ok(Value::Nil);
            }
        };
        host(a.lua).client.to_server.borrow_mut().push(line.to_string());
        Ok(Value::Nil)
    })?;
    def(lua, &globals, "CanAddServerToFavorites", |_| Ok(Value::Boolean(false)))?;
    def(lua, &globals, "OpenFolder", nothing)?;
    def(lua, &globals, "SetClipboardText", nothing)?;
    def(lua, &globals, "LoadAddonPresets", |a| {
        let text = host(a.lua).data_dir.borrow().as_ref().and_then(|d| d.read("settings/addonpresets.txt").ok());
        Ok(match text {
            Some(bytes) => Value::String(a.lua.create_string(bytes)?),
            None => Value::Nil,
        })
    })?;
    def(lua, &globals, "SaveAddonPresets", |a| {
        let text = a.string(1)?;
        if let Some(dir) = host(a.lua).data_dir.borrow().as_ref() {
            let _ = dir.write("settings/addonpresets.txt", text.as_bytes());
        }
        Ok(Value::Nil)
    })?;
    let gui = library(lua, "gui")?;
    def(lua, &gui, "ShowConsole", nothing)?;

    let steamworks = library(lua, "steamworks")?;
    def(lua, &steamworks, "ShouldMountAddon", |a| {
        let id = a.string(1)?;
        let mounted = addon_index(a.lua, &id).map(|i| host(a.lua).menu.addons.borrow()[i].mounted);
        Ok(Value::Boolean(mounted.unwrap_or(false)))
    })?;
    def(lua, &steamworks, "SetShouldMountAddon", |a| {
        let id = a.string(1)?;
        let mount = a.boolean(2);
        if let Some(i) = addon_index(a.lua, &id) {
            host(a.lua).menu.addons.borrow_mut()[i].mounted = mount;
        }
        Ok(Value::Nil)
    })?;
    def(lua, &steamworks, "IsSubscribed", |a| {
        let id = a.string(1)?;
        Ok(Value::Boolean(addon_index(a.lua, &id).is_some()))
    })?;
    def(lua, &steamworks, "ApplyAddons", |a| {
        host(a.lua).menu.applied.set(true);
        crate::call_hook(a.lua, "WorkshopSubscriptionsChanged", MultiValue::new());
        Ok(Value::Nil)
    })?;
    def(lua, &steamworks, "GetList", |a| {
        // the workshop's lists need Steam: an empty page
        if let Some(Value::Function(f)) = a.values.get(6) {
            let page = a.lua.create_table()?;
            page.raw_set("totalresults", 0)?;
            page.raw_set("numresults", 0)?;
            page.raw_set("results", a.lua.create_table()?)?;
            call_back(a.lua, f.clone(), Value::Table(page));
        }
        Ok(Value::Nil)
    })?;
    def(lua, &steamworks, "GetDownloadedItems", |a| Ok(Value::Table(a.lua.create_table()?)))?;
    def(lua, &steamworks, "FileUserInfo", |a| {
        if let Some(Value::Function(f)) = a.values.get(1) {
            let info = a.lua.create_table()?;
            info.raw_set("error", 1)?;
            call_back(a.lua, f.clone(), Value::Table(info));
        }
        Ok(Value::Nil)
    })?;
    def(lua, &steamworks, "RequestPlayerInfo", |a| {
        if let Some(Value::Function(f)) = a.values.get(1) {
            call_back(a.lua, f.clone(), "");
        }
        Ok(Value::Nil)
    })?;
    def(lua, &steamworks, "GetPlayerName", |a| Ok(Value::String(a.lua.create_string("")?)))?;
    def(lua, &steamworks, "ViewFile", |a| {
        let id = a.string(1)?;
        host(a.lua).print(&format!("Open in a browser: https://steamcommunity.com/sharedfiles/filedetails/?id={id}\n"));
        Ok(Value::Nil)
    })?;
    def(lua, &steamworks, "OpenWorkshop", |a| {
        host(a.lua).print("Open in a browser: https://steamcommunity.com/app/4000/workshop/\n");
        Ok(Value::Nil)
    })?;
    for name in [
        "Subscribe",
        "Unsubscribe",
        "MarkDownloadedItemAsUnused",
        "Vote",
        "VoteInfo",
        "SetFavorite",
        "SetFilePlayed",
        "SetFileCompleted",
        "Publish",
        "DownloadUGC",
    ] {
        def(lua, &steamworks, name, nothing)?;
    }

    let serverlist = library(lua, "serverlist")?;
    def(lua, &serverlist, "Query", |a| {
        // no servers without Steam's master server: the list is done at once
        if let Some(Value::Table(data)) = a.values.front()
            && let Some(finished) = data.get::<Option<mlua::Function>>("Finished")?
        {
            call_back(a.lua, finished, MultiValue::new());
        }
        Ok(Value::Nil)
    })?;
    for name in ["IsCurrentServerFavorite", "IsServerFavorite"] {
        def(lua, &serverlist, name, |_| Ok(Value::Boolean(false)))?;
    }
    for name in ["PingServer", "PlayerList", "AddServerToFavorites", "RemoveServerFromFavorites", "AddCurrentServerToFavorites"] {
        def(lua, &serverlist, name, nothing)?;
    }

    let chunk = lua.load(MENU_LUA).set_name("=[openGMOD menu]");
    chunk.exec()
}

/// The program's side of the menu.
impl crate::Runtime {
    /// The addons there are, mounted or not: `engine.GetAddons`, and what the menu switches.
    pub fn set_addons(&self, addons: Vec<Addon>) {
        *host(&self.lua).menu.addons.borrow_mut() = addons;
    }

    /// The addons as the menu left them, when it applied a change since the last call
    /// (`steamworks.ApplyAddons`): which to mount from now on.
    pub fn take_addon_changes(&self) -> Option<Vec<Addon>> {
        let menu = &host(&self.lua).menu;
        menu.applied.replace(false).then(|| menu.addons.borrow().clone())
    }

    /// No addons were mounted at all (`--no-addons`).
    pub fn set_no_addons(&self, none: bool) {
        host(&self.lua).menu.no_addons.set(none);
    }

    /// The games whose content can be mounted: `engine.GetGames`.
    pub fn set_games(&self, games: Vec<MountableGame>) {
        *host(&self.lua).menu.games.borrow_mut() = games;
    }

    /// Whether a game is running behind the menu: `IsInGame`.
    pub fn set_in_game(&self, in_game: bool) {
        host(&self.lua).menu.in_game.set(in_game);
    }

    /// The engine's commands the menu ran, oldest first: `map gm_construct`, `gamemode
    /// sandbox`, `disconnect`, `quit`, `resume` ... - the console lines it does not know
    /// itself.
    pub fn take_engine_commands(&self) -> Vec<String> {
        std::mem::take(&mut *host(&self.lua).client.to_server.borrow_mut())
    }

    /// The menu shows another gamemode (`gamemode` ran): its maps and backgrounds follow.
    pub fn set_gamemode(&self, name: &str) {
        *host(&self.lua).game.gamemode_name.borrow_mut() = name.to_string();
        self.call_hook("GameContentChanged", MultiValue::new());
    }
}
