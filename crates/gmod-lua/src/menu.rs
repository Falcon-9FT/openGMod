//! What the engine gives the main menu: the addons and whether each is mounted, the games
//! that can be mounted, whether a game is running, and the commands that start and leave one
//! (they go to the program: [`Runtime::take_engine_commands`](crate::Runtime::take_engine_commands)).
//!
//! The Steam side - `steamworks` - is what the menu needs of it without Steam: the details of
//! workshop items come from Steam's public web API; there is nothing to publish. The server
//! browser (`serverlist`) is [`crate::serverlist`]'s.

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

/// The screen's sizes, and its own.
pub(crate) type VideoModes = (Vec<(u32, u32)>, (u32, u32));

/// What the options window changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingChange {
    /// A setting (`sensitivity`) and its value.
    Value(String, String),
    /// A key (its engine name, lower case) and the command it does from now on; none: nothing.
    Bind(String, Option<String>),
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
    /// The lines typed into the console window, for the program to run.
    pub console_typed: RefCell<Vec<String>>,
    /// The player's settings as the program has them, for the options window; and what the
    /// window changed, for the program.
    pub settings: RefCell<std::collections::BTreeMap<String, String>>,
    pub binds: RefCell<Vec<(String, String)>>,
    pub setting_changes: RefCell<Vec<SettingChange>>,
    /// The screen's sizes the video options offer, and the screen's own.
    pub video_modes: RefCell<VideoModes>,
    /// The graphics API it draws with (`dx12`, `vulkan`), the GPU's name, and the APIs the
    /// player may choose.
    pub renderer: RefCell<(String, String, Vec<String>)>,
    pub games: RefCell<Vec<MountableGame>>,
    pub in_game: Cell<bool>,
    /// Addons were not mounted at all (`-noaddons`), workshop ones were not (`-noworkshop`).
    pub no_addons: Cell<bool>,
    /// The server browser's questions and favourites.
    pub servers: crate::serverlist::Servers,
    /// Steam, when the program signed in to it, and what was asked of it.
    pub steam: crate::steam::SteamState,
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

/// The words of a list of the engine's (`scripts/kb_act.lst`): quoted or not, `//` ending a
/// line; two to a pair.
fn list_pairs(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    for line in text.lines() {
        let mut rest = line;
        loop {
            rest = rest.trim_start();
            if rest.is_empty() || rest.starts_with("//") {
                break;
            }
            if let Some(quoted) = rest.strip_prefix('"') {
                let end = quoted.find('"').unwrap_or(quoted.len());
                words.push(quoted[..end].to_string());
                rest = quoted.get(end + 1..).unwrap_or("");
            } else {
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                words.push(rest[..end].to_string());
                rest = &rest[end..];
            }
        }
    }
    words
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

local previews, steam = ...

-- the details of a workshop item: from Steam when the program signed in to it, else from its
-- public web API
local details = {}
function steamworks.FileInfo( id, callback )
	id = tostring( id )
	if ( details[ id ] ) then return callback( details[ id ] ) end
	local function got( info )
		if ( info.error ) then return callback( info ) end
		info.installed = steamworks.IsSubscribed( id )
		info.disabled = !steamworks.ShouldMountAddon( id )
		info.children = {}
		details[ id ] = info
		callback( info )
	end
	if ( steam.FileInfo( id, got ) ) then return end
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
			got( {
				id = id, title = item.title or "", description = item.description or "",
				owner = item.creator or "", ownername = "", previewid = item.preview_url, previewurl = item.preview_url,
				size = tonumber( item.file_size ) or 0, created = tonumber( item.time_created ) or 0,
				updated = tonumber( item.time_updated ) or 0, tags = table.concat( tags, "," ),
				banned = tonumber( item.banned ) == 1
			} )
		end,
		failed = function( reason ) callback( { error = reason } ) end
	} )
end

-- the details give the address of the preview: it is its own download, kept in openGMOD's
-- cache; the menu shows it by the path it gets back (`../<path>` from its page), as GMod's
function steamworks.Download( id, uncompress, callback )
	if ( !callback ) then return end
	if ( !isstring( id ) or !id:find( "^https://" ) ) then return callback( nil ) end
	local kept = previews.find( id )
	if ( kept ) then return callback( kept ) end
	HTTP( {
		url = id,
		method = "GET",
		success = function( code, body ) callback( code == 200 and previews.save( id, body ) or nil ) end,
		failed = function() callback( nil ) end
	} )
end

-- the names of workshop authors: from Steam, else their public Steam profiles, asked one at
-- a time - Steam
-- turns away many at once (429), and those are asked again a little later
local names, waiting, queue, busy = {}, {}, {}, false
local function AskNextName( tries )
	local steamid = queue[ 1 ]
	if ( !steamid ) then busy = false return end
	busy = true
	local function answer( name )
		table.remove( queue, 1 )
		if ( name != "" ) then names[ steamid ] = name end
		for _, callback in ipairs( waiting[ steamid ] or {} ) do callback( name ) end
		waiting[ steamid ] = nil
		timer.Simple( 0.1, function() AskNextName( 0 ) end )
	end
	HTTP( {
		url = "https://steamcommunity.com/profiles/" .. steamid .. "/?xml=1",
		method = "GET",
		success = function( code, body )
			if ( code == 429 and tries < 5 ) then
				return timer.Simple( 2 ^ tries, function() AskNextName( tries + 1 ) end )
			end
			answer( code == 200 and body and body:match( "<steamID><!%[CDATA%[(.-)%]%]></steamID>" ) or "" )
		end,
		failed = function() answer( "" ) end
	} )
end
function steamworks.RequestPlayerInfo( steamid, callback )
	steamid = tostring( steamid )
	if ( steam.PlayerName( steamid, callback ) ) then return end
	if ( names[ steamid ] ) then return callback( names[ steamid ] ) end
	if ( !steamid:find( "^%d+$" ) ) then return callback( "" ) end
	if ( waiting[ steamid ] ) then return table.insert( waiting[ steamid ], callback ) end
	waiting[ steamid ] = { callback }
	queue[ #queue + 1 ] = steamid
	if ( !busy ) then AskNextName( 0 ) end
end

-- what servers were let do (`connect` elsewhere): for this run of the program, those given
-- for good too; by the address of the server played on
local granted = { temporary = {}, permanent = {} }
local function ServerAddress()
	local address = engine.CurrentServerAddress and engine.CurrentServerAddress() or ""
	return address != "" and address or "loopback"
end
local function Changed()
	if ( hook ) then hook.Run( "OnPermissionsChanged" ) end
end
permissions = {}
function permissions.GetAll()
	local all = { temporary = {}, permanent = {} }
	for kind, servers in pairs( granted ) do
		for address, perms in pairs( servers ) do
			local names = {}
			for name in pairs( perms ) do names[ #names + 1 ] = name end
			table.sort( names )
			if ( #names > 0 ) then all[ kind ][ address ] = table.concat( names, "," ) end
		end
	end
	return all
end
function permissions.IsGranted( perm )
	local address = ServerAddress()
	for _, servers in pairs( granted ) do
		if ( servers[ address ] and servers[ address ][ perm ] ) then return true end
	end
	return false
end
function permissions.Grant( perm, temporary )
	local servers = granted[ temporary and "temporary" or "permanent" ]
	local address = ServerAddress()
	servers[ address ] = servers[ address ] or {}
	servers[ address ][ perm ] = true
	Changed()
end
function permissions.Revoke( perm, address )
	for _, servers in pairs( granted ) do
		if ( servers[ address ] ) then servers[ address ][ perm ] = nil end
	end
	Changed()
end
function permissions.Connect( address )
	RunConsoleCommand( "connect", address )
end
function permissions.AskToConnect( address )
	if ( RequestConnectToServer ) then RequestConnectToServer( address ) end
end

-- the console (`): what the game, its Lua and the program said, and a line to type commands
-- into, as the engine's own window; made the first time it opens, what came before kept
OpenGMODConsole = OpenGMODConsole or { pending = {} }
local Console = OpenGMODConsole

local function MakeConsole()
	surface.CreateFont( "OpenGMODConsole", { font = "Lucida Console", size = 13, weight = 400, antialias = true } )
	local frame = vgui.Create( "DFrame" )
	frame:SetTitle( "Console" )
	frame:SetSize( math.min( ScrW() - 40, 900 ), math.min( ScrH() - 40, 560 ) )
	frame:Center()
	frame:SetSizable( true )
	frame:SetScreenLock( true )
	frame:SetDeleteOnClose( false )
	frame:SetMinWidth( 300 )
	frame:SetMinHeight( 160 )
	frame.btnMaxim:SetVisible( false )
	frame.btnMinim:SetVisible( false )
	-- the main menu comes up with it, over the game: the console stays in front of it
	local base = vgui.GetControlTable( "DFrame" )
	frame.Think = function( self )
		if ( base and base.Think ) then base.Think( self ) end
		if ( ( self.Fronts or 0 ) > 0 ) then
			self.Fronts = self.Fronts - 1
			self:MoveToFront()
		end
	end

	local bottom = frame:Add( "Panel" )
	bottom:Dock( BOTTOM )
	bottom:SetTall( 22 )
	bottom:DockMargin( 0, 4, 0, 0 )
	local submit = bottom:Add( "DButton" )
	submit:Dock( RIGHT )
	submit:SetWide( 80 )
	submit:SetText( "Submit" )
	local entry = bottom:Add( "DTextEntry" )
	entry:Dock( FILL )
	entry:DockMargin( 0, 0, 4, 0 )
	entry:SetHistoryEnabled( true )

	local back = frame:Add( "DPanel" )
	back:Dock( FILL )
	back.Paint = function( self, w, h )
		surface.SetDrawColor( 24, 24, 24, 240 )
		surface.DrawRect( 0, 0, w, h )
	end
	local log = back:Add( "RichText" )
	log:Dock( FILL )
	log:DockMargin( 4, 2, 2, 2 )
	log.PerformLayout = function( self ) self:SetFontInternal( "OpenGMODConsole" ) end

	local function Run()
		local line = entry:GetText()
		entry:SetText( "" )
		entry:RequestFocus()
		if ( string.Trim( line ) == "" ) then return end
		entry:AddHistory( line )
		Console.Print( "] " .. line .. "\n", 220, 220, 220 )
		OpenGMOD.ConsoleCommand( line )
	end
	entry.OnEnter = Run
	submit.DoClick = Run

	frame.Log, frame.Entry = log, entry
	Console.Frame = frame
	for _, chunk in ipairs( Console.pending ) do
		log:InsertColorChange( chunk[ 2 ], chunk[ 3 ], chunk[ 4 ], 255 )
		log:AppendText( chunk[ 1 ] )
	end
	Console.pending = {}
	return frame
end

function Console.Print( text, r, g, b )
	text = string.Replace( text, "\t", "    " )
	local frame = Console.Frame
	if ( IsValid( frame ) ) then
		frame.Log:InsertColorChange( r, g, b, 255 )
		frame.Log:AppendText( text )
		return
	end
	local pending = Console.pending
	pending[ #pending + 1 ] = { text, r, g, b }
	if ( #pending > 2000 ) then table.remove( pending, 1 ) end
end

function Console.Toggle()
	local frame = Console.Frame
	if ( !IsValid( frame ) ) then
		frame = MakeConsole()
	elseif ( frame:IsVisible() ) then
		frame:Close()
		return false
	end
	frame:SetVisible( true )
	frame:MakePopup()
	frame.Fronts = 5
	frame.Entry:RequestFocus()
	return true
end

function Console.IsOpen()
	return IsValid( Console.Frame ) and Console.Frame:IsVisible()
end
"#;

/// Where the previews of workshop items are kept, in openGMOD's folder: the menu's page
/// asks for them by this path (`asset://garrysmod/cache/previews/...`).
pub const PREVIEWS: &str = "cache/previews";

/// The file a preview at `url` is kept in, without its extension: a hash of the address.
fn preview_stem(url: &str) -> String {
    // FNV-1a
    let hash = url.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    format!("{PREVIEWS}/{hash:016x}")
}

/// The picture types a preview may be, by their first bytes.
const PICTURES: [(&[u8], &str); 3] = [(b"\xff\xd8\xff", "jpg"), (b"\x89PNG", "png"), (b"GIF8", "gif")];

/// The table the menu's Lua keeps previews with: `find(url)` the path of one kept already,
/// `save(url, body)` keeps one downloaded (a picture only) and gives its path.
pub(crate) fn previews(lua: &Lua) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.raw_set(
        "find",
        lua.create_function(|lua, url: String| {
            let stem = preview_stem(&url);
            let host = host(lua);
            let dir = host.data_dir.borrow();
            let Some(dir) = dir.as_ref() else { return Ok(None) };
            Ok(PICTURES.iter().map(|(_, ext)| format!("{stem}.{ext}")).find(|path| dir.path(path).is_ok_and(|p| p.is_file())))
        })?,
    )?;
    table.raw_set(
        "save",
        lua.create_function(|lua, (url, body): (String, mlua::LuaString)| {
            let body = body.as_bytes();
            let Some((_, ext)) = PICTURES.iter().find(|(magic, _)| body.starts_with(magic)) else { return Ok(None) };
            let path = format!("{}.{ext}", preview_stem(&url));
            let host = host(lua);
            let dir = host.data_dir.borrow();
            Ok(dir.as_ref().and_then(|dir| dir.write(&path, &body).ok()).map(|_| path))
        })?,
    )?;
    Ok(table)
}

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
    crate::loading::install(lua)?;
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
            // the options window, the menu's own
            "openoptionsdialog" => {
                if let Ok(options) = a.lua.globals().raw_get::<Table>("OpenGMODOptions")
                    && let Ok(open) = options.raw_get::<mlua::Function>("Open")
                {
                    let _ = crate::files::call_reporting(a.lua, open, MultiValue::new());
                }
                return Ok(Value::Nil);
            }
            _ => {
                host(a.lua).print(&format!("{command} is not in openGMOD yet\n"));
                return Ok(Value::Nil);
            }
        };
        host(a.lua).client.to_server.borrow_mut().push(line.to_string());
        Ok(Value::Nil)
    })?;
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
    // the console window's side of the program: what is typed into it goes to the program
    let opengmod = library(lua, "OpenGMOD")?;
    // the options window is laid out as the engine's: its pages' `.res` files, with what does
    // not hold on this computer left out; and the lists of keys (`scripts/kb_act.lst`)
    def(lua, &opengmod, "ReadResource", |a| {
        let path = a.string(1)?;
        let Some(text) = host(a.lua).vfs.read(&path, "GAME").ok() else { return Ok(Value::Nil) };
        let (mut document, _) = gmod_kv::parse_lenient(&gmod_kv::decode_text(&text), gmod_kv::Options::default());
        document.root.apply_conditions(&gmod_kv::pc_symbol);
        let controls = a.lua.create_table()?;
        let Some(top) = document.root.pairs.first().and_then(|p| p.value.as_block()) else { return Ok(Value::Table(controls)) };
        for pair in top.iter() {
            let Some(block) = pair.value.as_block() else { continue };
            let control = a.lua.create_table()?;
            control.raw_set("name", pair.key.as_str())?;
            for field in block.iter() {
                let key = field.key.to_ascii_lowercase();
                // the first of a key, as the engine reads it
                if let Some(value) = field.value.as_str()
                    && control.raw_get::<Value>(key.as_str())?.is_nil()
                {
                    control.raw_set(key, value)?;
                }
            }
            controls.raw_push(control)?;
        }
        Ok(Value::Table(controls))
    })?;
    def(lua, &opengmod, "ReadPairs", |a| {
        let path = a.string(1)?;
        let Some(text) = host(a.lua).vfs.read(&path, "GAME").ok() else { return Ok(Value::Nil) };
        let pairs = a.lua.create_table()?;
        for [key, value] in list_pairs(&gmod_kv::decode_text(&text)).as_chunks::<2>().0 {
            pairs.raw_push(a.lua.create_sequence_from([key.as_str(), value.as_str()])?)?;
        }
        Ok(Value::Table(pairs))
    })?;
    def(lua, &opengmod, "VideoModes", |a| {
        let (modes, native) = host(a.lua).menu.video_modes.borrow().clone();
        let list = a.lua.create_table()?;
        for (w, h) in modes {
            list.raw_push(a.lua.create_sequence_from([w, h])?)?;
        }
        list.raw_set("native", a.lua.create_sequence_from([native.0, native.1])?)?;
        Ok(Value::Table(list))
    })?;
    // what draws: `{ api = "dx12", gpu = "...", available = { "dx12", "vulkan" } }`
    def(lua, &opengmod, "Renderer", |a| {
        let (api, gpu, available) = host(a.lua).menu.renderer.borrow().clone();
        let out = a.lua.create_table()?;
        out.raw_set("api", api)?;
        out.raw_set("gpu", gpu)?;
        out.raw_set("available", a.lua.create_sequence_from(available)?)?;
        Ok(Value::Table(out))
    })?;
    // the options window's: the player's settings and key bindings
    def(lua, &opengmod, "GetSetting", |a| {
        let name = a.string(1)?.to_ascii_lowercase();
        let value = host(a.lua).menu.settings.borrow().get(&name).cloned();
        Ok(match value {
            Some(value) => Value::String(a.lua.create_string(value)?),
            None => Value::Nil,
        })
    })?;
    def(lua, &opengmod, "SetSetting", |a| {
        let name = a.string(1)?.to_ascii_lowercase();
        let value = match a.get(2) {
            Some(Value::Boolean(b)) => if *b { "1" } else { "0" }.to_string(),
            Some(Value::Number(n)) if n.fract() == 0.0 && n.abs() < 1e15 => (*n as i64).to_string(),
            Some(Value::Number(n)) => n.to_string(),
            Some(Value::Integer(n)) => n.to_string(),
            _ => a.string(2)?,
        };
        let menu = &host(a.lua).menu;
        menu.settings.borrow_mut().insert(name.clone(), value.clone());
        menu.setting_changes.borrow_mut().push(SettingChange::Value(name, value));
        Ok(Value::Nil)
    })?;
    // every key bound, and what it does
    def(lua, &opengmod, "GetBinds", |a| {
        let list = a.lua.create_table()?;
        for (key, command) in host(a.lua).menu.binds.borrow().iter() {
            list.raw_push(a.lua.create_sequence_from([key.as_str(), command.as_str()])?)?;
        }
        Ok(Value::Table(list))
    })?;
    // the keys that do a command, in the order they were bound
    def(lua, &opengmod, "GetBindKeys", |a| {
        let command = a.string(1)?;
        let keys: Vec<String> =
            host(a.lua).menu.binds.borrow().iter().filter(|(_, c)| c.eq_ignore_ascii_case(&command)).map(|(k, _)| k.clone()).collect();
        Ok(Value::Table(a.lua.create_sequence_from(keys)?))
    })?;
    // a key does a command from now on (nil: nothing)
    def(lua, &opengmod, "BindKey", |a| {
        let key = a.string(1)?.to_lowercase();
        let command = a.opt_string(2)?.filter(|c| !c.is_empty());
        let menu = &host(a.lua).menu;
        {
            let mut binds = menu.binds.borrow_mut();
            binds.retain(|(k, _)| *k != key);
            if let Some(command) = &command {
                binds.push((key.clone(), command.clone()));
            }
        }
        menu.setting_changes.borrow_mut().push(SettingChange::Bind(key, command));
        Ok(Value::Nil)
    })?;
    def(lua, &opengmod, "ConsoleCommand", |a| {
        let line = a.string(1)?;
        host(a.lua).menu.console_typed.borrow_mut().push(line);
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
    crate::steam::install(lua, &steamworks)?;

    let serverlist = library(lua, "serverlist")?;
    crate::serverlist::install(lua, &serverlist)?;

    let chunk = lua.load(MENU_LUA).set_name("=[openGMOD menu]");
    chunk.call::<()>((previews(lua)?, crate::steam::natives(lua)?))?;
    // the options dialog, as the engine's GameUI makes it
    lua.load(include_str!("menu_options.lua")).set_name("=[openGMOD options]").exec()
}

/// The program's side of the menu.
impl crate::Runtime {
    /// The addons there are, mounted or not: `engine.GetAddons`, and what the menu switches.
    pub fn set_addons(&self, addons: Vec<Addon>) {
        *host(&self.lua).menu.addons.borrow_mut() = addons;
    }

    /// The addons there are changed (subscribed to or left on Steam): the menu lists them
    /// anew (`WorkshopSubscriptionsChanged`).
    pub fn addons_changed(&self, addons: Vec<Addon>) {
        self.set_addons(addons);
        crate::call_hook(&self.lua, "WorkshopSubscriptionsChanged", MultiValue::new());
    }

    /// The addons as the menu left them, when it applied a change since the last call
    /// (`steamworks.ApplyAddons`): which to mount from now on.
    pub fn take_addon_changes(&self) -> Option<Vec<Addon>> {
        let menu = &host(&self.lua).menu;
        menu.applied.replace(false).then(|| menu.addons.borrow().clone())
    }

    /// What the game said, for the console window (`): text, line breaks included, in a
    /// colour; kept until the window is first opened.
    pub fn console_print(&self, text: &str, color: [u8; 3]) {
        let _ = self.console_call("Print", (text, color[0], color[1], color[2]));
    }

    /// Opens the console window, or closes it: whether it shows now.
    pub fn toggle_console(&self) -> bool {
        self.console_call("Toggle", ()).and_then(|v| v.into_iter().next()).is_some_and(|v| matches!(v, Value::Boolean(true)))
    }

    /// Whether the console window shows.
    pub fn console_shown(&self) -> bool {
        self.console_call("IsOpen", ()).and_then(|v| v.into_iter().next()).is_some_and(|v| matches!(v, Value::Boolean(true)))
    }

    /// The player's settings and key bindings, for the options window.
    pub fn set_settings(&self, values: std::collections::BTreeMap<String, String>, binds: Vec<(String, String)>) {
        let menu = &host(&self.lua).menu;
        *menu.settings.borrow_mut() = values;
        *menu.binds.borrow_mut() = binds;
    }

    /// The screen's sizes the video options offer, and the screen's own.
    pub fn set_video_modes(&self, modes: Vec<(u32, u32)>, native: (u32, u32)) {
        *host(&self.lua).menu.video_modes.borrow_mut() = (modes, native);
    }

    /// What draws: the graphics API (`dx12`, `vulkan`), the GPU's name, and the APIs it could
    /// draw with, for the advanced video options.
    pub fn set_renderer(&self, api: &str, gpu: &str, available: Vec<String>) {
        *host(&self.lua).menu.renderer.borrow_mut() = (api.to_string(), gpu.to_string(), available);
    }

    /// What the options window changed since the last call, in order.
    pub fn take_setting_changes(&self) -> Vec<SettingChange> {
        std::mem::take(&mut *host(&self.lua).menu.setting_changes.borrow_mut())
    }

    /// The lines typed into the console window since the last call, oldest first.
    pub fn take_console_commands(&self) -> Vec<String> {
        std::mem::take(&mut *host(&self.lua).menu.console_typed.borrow_mut())
    }

    fn console_call(&self, name: &str, args: impl mlua::IntoLuaMulti) -> Option<MultiValue> {
        let console: Table = self.lua.globals().raw_get("OpenGMODConsole").ok()?;
        let f: mlua::Function = console.raw_get(name).ok()?;
        crate::files::call_reporting(&self.lua, f, args.into_lua_multi(&self.lua).ok()?).ok()
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
