//! The Lua runtime: LuaJIT 2.1 with Garry's Mod's way of loading Lua and the functions GMod
//! implements in C++.
//!
//! A [`Runtime`] is one Lua state of one realm - the server, a client or the main menu - on
//! top of the game file system. The Lua of the game itself (`lua/includes/init.lua`, the
//! `base` and `sandbox` gamemodes, Derma) and of the addons is loaded from the installation,
//! never rewritten; this crate provides what that Lua expects from the engine:
//!
//! * the loader ([`files`]): `include`, `require`, `AddCSLuaFile`, `CompileString`,
//!   `CompileFile`, `RunString`. Every chunk goes through [`gmod_glua`] first, so GMod's
//!   additions to the syntax (`!=`, `&&`, `continue`, `//` ...) compile in a stock LuaJIT, with
//!   the line numbers of the original file.
//! * the sandbox ([`sandbox`]): what GMod takes out of the standard libraries (`io`,
//!   `loadfile`, `dofile`, C modules, most of `os`), and what openGMOD takes out on top:
//!   bytecode. LuaJIT runs bytecode without checking it, so a binary chunk from a server is
//!   a way out of the sandbox; `load`, `loadstring` and the loader accept source only.
//! * the console ([`console`]): `print`, `Msg`, `MsgN`, `MsgC`, `ErrorNoHalt` and the error
//!   reports GMod prints when a script fails, handed to the program through [`Output`].
//! * the engine's types and libraries: [`meta`] (`FindMetaTable`, `TYPE_*`), [`vector`]
//!   (`Vector`, `Angle`), [`matrix`] (`VMatrix`), [`entity`] (entities, `NULL`, the methods of
//!   `Entity`, the metatables of `Player`, `Weapon` ...), [`ents`] (`ents.*`, scripted
//!   entities, the entities of a map), [`player`] (players: joining, spawning, dying, their
//!   commands and movement, the `player` library), [`weapon`] (weapons in players' hands,
//!   ammo types), [`movedata`] (`CMoveData`, `CUserCmd`), [`damage`] (`CTakeDamageInfo`,
//!   taking damage), [`effect`] (`EffectData`, `util.Effect`), [`sound`] (`EmitSound`,
//!   `CreateSound`, the `sound` library, all quiet), [`trace`] (`util.TraceLine` and the like),
//!   [`physics`] (bodies of entities, `PhysObj`), [`physgun`] (`weapon_physgun`),
//!   [`anim`] (what entities play on their bones: sequences, cycles, pose parameters, layers,
//!   view models, the player's model), [`ragdoll`] (bodies on the bones of `prop_ragdoll` and
//!   corpses), [`chat`] (`say` through `PlayerSay`, the chat box, `chat.AddText`),
//!   [`constraint`] (welds, ropes, axes ... as joints), [`inputs`] (`Entity:Input`,
//!   `Entity:Fire`), [`timer`] (`CurTime`, `timer.*`), [`convar`] (ConVars, console commands), [`filesystem`]
//!   (`file`, `File`), [`util`], [`net`], [`sql`] (SQLite), [`http`] (`HTTP`), [`material`]
//!   (`Material`), [`engine`] (`game`, `engine`, `system`, `gmod`, the enumerations), and
//!   [`world`] (`ai` and `motionsensor`, as they answer while there are no NPCs). They grow towards the whole
//!   API - `gmod-check lua` counts how much of it is there.
//! * starting the game ([`Runtime::start`]): `includes/init.lua`, autorun, the gamemode and
//!   its bases, scripted entities and weapons, the hooks up to `Initialize`; the entities of
//!   the map ([`Runtime::spawn_map`]); players joining ([`Runtime::join`]) and what they press
//!   ([`Runtime::set_input`]); then [`Runtime::think`] moves time on, runs timers, and for
//!   every tick the players' commands and movement, the bodies of physics, the inputs fired,
//!   the entities that think, the animations and the `Tick` hook; then the `Think` hook. What is to be shown: a player's view ([`Runtime::view`]),
//!   the models of the entities they see, posed ([`Runtime::drawn`]), and what they hold before their eyes ([`Runtime::view_models`]).
//!
//! Errors in scripts are not errors of the runtime: as in GMod, a file that fails is
//! reported ([`Output::Error`], [`Runtime::errors`]) and the files around it keep loading.

#![deny(unsafe_code)]

use std::cell::{Cell, RefCell};
use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use gmod_fs::Vfs;
use mlua::Lua;

pub mod anim;
mod args;
pub mod chat;
pub mod client;
pub mod console;
pub mod constraint;
pub mod convar;
pub mod damage;
pub mod effect;
pub mod engine;
pub mod entity;
pub mod ents;
pub mod files;
pub mod filesystem;
pub mod html;
pub mod http;
pub mod hud;
pub mod inputs;
pub mod logic;
pub mod material;
pub mod matrix;
pub mod menu;
pub mod meta;
pub mod movedata;
pub mod movers;
pub mod net;
pub mod outputs;
pub mod physgun;
pub mod physics;
pub mod player;
pub mod ragdoll;
pub mod sandbox;
pub mod snapshot;
pub mod sound;
pub mod sql;
mod startup;
pub mod surface;
mod surfaceprops;
pub mod timer;
pub mod trace;
pub mod triggers;
pub mod util;
pub mod vector;
pub mod vgui;
pub mod weapon;
pub mod world;

#[cfg(test)]
mod tests;

pub use chat::ChatMessage;
pub use entity::EntityHandle;
pub use ents::{Drawn, Map};
pub use html::{WebButton, WebEvent, WebPage, WebPages};
pub use menu::{Addon, MountableGame};
pub use mlua;
pub use movedata::UserCmd;
pub use player::{PlayerInfo, PlayerView, WeaponView};
pub use snapshot::{NetValue, Snapshot};

/// Which of GMod's Lua states a [`Runtime`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Realm {
    /// The server: game logic, the authority over entities.
    Server,
    /// A client: the HUD, the menus of the game, the view.
    Client,
    /// The main menu, outside any game.
    Menu,
}

impl Realm {
    pub fn name(self) -> &'static str {
        match self {
            Realm::Server => "server",
            Realm::Client => "client",
            Realm::Menu => "menu",
        }
    }

    /// The file the engine runs first in this realm.
    pub fn init_file(self) -> &'static str {
        match self {
            Realm::Server | Realm::Client => "includes/init.lua",
            Realm::Menu => "includes/init_menu.lua",
        }
    }
}

impl fmt::Display for Realm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A script that failed, reported the way GMod prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptError {
    /// `lua/file.lua:12: attempt to call global 'Foo' (a nil value)`.
    pub message: String,
    /// The stack at the error, innermost first (`lua/file.lua:12: in main chunk`); empty when
    /// there is none, as for a file that does not compile.
    pub stack: Vec<String>,
}

impl ScriptError {
    pub(crate) fn new(message: impl Into<String>) -> ScriptError {
        ScriptError { message: message.into(), stack: Vec::new() }
    }

    /// An error of `mlua`, which carries Lua's message and traceback in its own wrapping.
    pub(crate) fn from_lua(error: &mlua::Error) -> ScriptError {
        let (message, traceback) = unwrap_error(error);
        let (message, own_traceback) = split_traceback(&message);
        let traceback = if traceback.is_empty() { own_traceback } else { traceback };
        let stack = traceback
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && *line != "stack traceback:" && !line.starts_with("[C]: in ?"))
            .map(str::to_string)
            .collect();
        ScriptError { message, stack }
    }
}

/// Lua's message and traceback out of mlua's wrappers: a Rust function (`include`) that was
/// called from Lua and failed in Lua it called itself.
fn unwrap_error(error: &mlua::Error) -> (String, String) {
    match error {
        mlua::Error::CallbackError { traceback, cause } => {
            let (message, inner) = unwrap_error(cause);
            (message, if inner.is_empty() { traceback.clone() } else { inner })
        }
        mlua::Error::WithContext { cause, .. } => unwrap_error(cause),
        mlua::Error::RuntimeError(message) | mlua::Error::MemoryError(message) => (message.clone(), String::new()),
        mlua::Error::SyntaxError { message, .. } => (message.clone(), String::new()),
        mlua::Error::BadArgument { to, pos, cause, .. } => {
            let (message, traceback) = unwrap_error(cause);
            let to = to.as_deref().unwrap_or("?");
            (format!("bad argument #{pos} to '{to}' ({message})"), traceback)
        }
        other => (other.to_string(), String::new()),
    }
}

fn split_traceback(message: &str) -> (String, String) {
    match message.find("\nstack traceback:") {
        Some(at) => (message[..at].to_string(), message[at + 1..].to_string()),
        None => (message.to_string(), String::new()),
    }
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[ERROR] {}", self.message)?;
        for (i, frame) in self.stack.iter().enumerate() {
            write!(f, "\n  {}. {frame}", i + 1)?;
        }
        Ok(())
    }
}

/// What scripts print, as the console would show it.
#[derive(Debug, Clone, Copy)]
pub enum Output<'a> {
    /// `print`, `Msg`, `MsgN`, `MsgC`: text as written, line breaks included.
    Text(&'a str),
    /// A script failed; it is in [`Runtime::errors`] too.
    Error(&'a ScriptError),
}

type OutputFn = Box<dyn FnMut(Output<'_>)>;

/// What the functions given to Lua share: the state of the engine behind them. Kept in the
/// Lua state's app data.
pub(crate) struct Host {
    pub vfs: Arc<Vfs>,
    pub realm: Realm,
    /// Files `AddCSLuaFile` sent to clients, as game paths, in the order they were added.
    pub sent: RefCell<Vec<String>>,
    pub errors: RefCell<Vec<ScriptError>>,
    pub output: RefCell<OutputFn>,
    /// The stack of the last error, kept by the message handler until the caller of
    /// `xpcall` takes it.
    pub pending_stack: RefCell<Vec<String>>,
    /// Lua files loaded so far.
    pub files_loaded: Cell<usize>,
    pub started: Instant,
    pub clock: timer::Clock,
    pub console: convar::Console,
    /// openGMOD's own folder, whose `data/` Lua reads and writes as `DATA`.
    pub data_dir: RefCell<Option<gmod_fs::UserDir>>,
    pub game: engine::Game,
    pub network_strings: util::NetworkStrings,
    pub ai_names: world::AiNames,
    pub net: net::Net,
    pub database: sql::Database,
    pub http: http::Http,
    pub entities: entity::Entities,
    pub models: ents::Models,
    /// The rigs of models, and the activities they name.
    pub anims: anim::Anims,
    pub world: trace::World,
    pub weapons: weapon::Weapons,
    pub players: player::Players,
    pub physics: physics::World,
    pub physgun: physgun::Guns,
    pub inputs: inputs::Queue,
    /// The 2D frame `surface` draws, and its fonts; the client and the menu have one.
    pub screen: surface::Screen,
    /// What the player sees and presses; the client's.
    pub client: client::Client,
    /// The client's panels.
    pub panels: vgui::Panels,
    /// The pages of its HTML panels.
    pub pages: html::Pages,
    /// The addons and games, and what the main menu tells the program.
    pub menu: menu::Menu,
    /// The engine's HUD: where its elements go, its colours.
    pub hud: RefCell<hud::Hud>,
    /// The sounds Lua played, for the program.
    pub sounds: sound::Sounds,
    /// What surfaces are made of.
    pub surfaces: surfaceprops::Surfaces,
    /// What is said: the server's messages for its players, a client's chat box.
    pub chat: chat::Chat,
    /// What touched what last tick.
    pub(crate) touching: triggers::Touching,
    /// What is solid of the brushes that may move.
    pub(crate) movers: movers::Moving,
}

impl Host {
    pub(crate) fn print(&self, text: &str) {
        (self.output.borrow_mut())(Output::Text(text));
    }

    /// Queues a console line for the next frame, as the engine's command buffer does.
    pub(crate) fn console_buffer(&self, line: String) {
        self.console.buffer.borrow_mut().push(line);
    }

    pub(crate) fn report(&self, error: ScriptError) {
        (self.output.borrow_mut())(Output::Error(&error));
        self.errors.borrow_mut().push(error);
    }
}

pub(crate) fn host(lua: &Lua) -> mlua::AppDataRef<'_, Host> {
    lua.app_data_ref::<Host>().expect("every Runtime keeps a Host")
}

fn default_output(output: Output<'_>) {
    use std::io::Write;
    match output {
        Output::Text(text) => {
            let _ = std::io::stdout().write_all(text.as_bytes());
        }
        Output::Error(error) => {
            let _ = writeln!(std::io::stderr(), "{error}");
        }
    }
}

/// One Lua state of one realm.
pub struct Runtime {
    lua: Lua,
}

impl Runtime {
    /// A Lua state for `realm` that reads Lua from `vfs` (search path `GAME`), with the
    /// standard libraries as GMod has them and the engine's functions, before any Lua of the
    /// game has run. What scripts print goes to standard output until
    /// [`set_output`](Self::set_output) says otherwise.
    pub fn new(realm: Realm, vfs: Arc<Vfs>) -> mlua::Result<Runtime> {
        let lua = sandbox::new_state()?;
        lua.set_app_data(Host {
            vfs,
            realm,
            sent: RefCell::new(Vec::new()),
            errors: RefCell::new(Vec::new()),
            output: RefCell::new(Box::new(default_output)),
            pending_stack: RefCell::new(Vec::new()),
            files_loaded: Cell::new(0),
            started: Instant::now(),
            clock: timer::Clock::default(),
            console: convar::Console::default(),
            data_dir: RefCell::new(None),
            game: engine::Game::default(),
            network_strings: util::NetworkStrings::default(),
            ai_names: world::AiNames::default(),
            net: net::Net::default(),
            database: sql::Database::default(),
            http: http::Http::default(),
            entities: entity::Entities::default(),
            models: ents::Models::default(),
            anims: anim::Anims::default(),
            world: trace::World::default(),
            weapons: weapon::Weapons::default(),
            players: player::Players::default(),
            physics: physics::World::default(),
            physgun: physgun::Guns::default(),
            inputs: inputs::Queue::default(),
            screen: surface::Screen::default(),
            client: client::Client::default(),
            panels: vgui::Panels::default(),
            pages: html::Pages::default(),
            menu: menu::Menu::default(),
            hud: RefCell::new(hud::Hud::default()),
            sounds: sound::Sounds::default(),
            surfaces: surfaceprops::Surfaces::default(),
            chat: chat::Chat::default(),
            touching: triggers::Touching::default(),
            movers: movers::Moving::default(),
        });
        // `getmetatable` first: the sandbox's `debug.getmetatable` asks it about userdata
        meta::install(&lua)?;
        sandbox::install(&lua)?;
        console::install(&lua)?;
        files::install(&lua)?;
        timer::install(&lua)?;
        convar::install(&lua)?;
        filesystem::install(&lua)?;
        vector::install(&lua)?;
        matrix::install(&lua)?;
        entity::install(&lua, realm)?;
        util::install(&lua)?;
        anim::install_util(&lua)?;
        trace::install(&lua)?;
        effect::install(&lua)?;
        constraint::install(&lua)?;
        sound::install(&lua)?;
        material::install(&lua)?;
        world::install(&lua)?;
        ents::install(&lua, realm)?;
        damage::install(&lua)?;
        movedata::install(&lua)?;
        player::install(&lua, realm)?;
        sql::install(&lua)?;
        http::install(&lua)?;
        net::install(&lua, realm)?;
        startup::install(&lua)?;
        engine::install(&lua, realm)?;
        weapon::install(&lua)?;
        if realm != Realm::Server {
            surface::install(&lua)?;
            client::install(&lua)?;
            vgui::install(&lua)?;
            surface::load_fonts(&lua);
            client::load(&lua);
            hud::load(&lua);
        }
        menu::install(&lua, realm)?;
        Ok(Runtime { lua })
    }

    pub fn realm(&self) -> Realm {
        host(&self.lua).realm
    }

    /// The Lua state, for what this crate does not wrap yet.
    pub fn lua(&self) -> &Lua {
        &self.lua
    }

    /// Where printed text and error reports go.
    pub fn set_output(&self, output: impl FnMut(Output<'_>) + 'static) {
        *host(&self.lua).output.borrow_mut() = Box::new(output);
    }

    /// openGMOD's own folder: its `data/` is the `DATA` search path of the `file` library,
    /// the only place Lua writes to, and the `sql` database is in it ([`sql::file_name`]).
    pub fn set_data_dir(&self, dir: gmod_fs::UserDir) {
        let host = host(&self.lua);
        *host.data_dir.borrow_mut() = Some(dir);
        host.database.close();
    }

    /// Lets `HTTP` reach private networks (this machine, the local network), as GMod's
    /// `-allowlocalhttp` does for dedicated servers. Only the server realm takes it.
    pub fn allow_local_http(&self, allow: bool) {
        let host = host(&self.lua);
        host.http.allow_local.set(allow && host.realm == Realm::Server);
    }

    /// How many `HTTP` requests are still waiting for an answer.
    pub fn http_in_flight(&self) -> usize {
        host(&self.lua).http.in_flight()
    }

    /// Runs the file the engine starts the realm with ([`Realm::init_file`]). False if it
    /// could not be run at all; errors inside it are reported as usual.
    pub fn init(&self) -> bool {
        self.include(self.realm().init_file())
    }

    /// Starts the game as the engine does: `includes/init.lua`, autorun, the gamemode and
    /// its bases, scripted entities and weapons, and the hooks up to `Initialize`; the main
    /// menu starts with its Derma and `menu/menu.lua` (`gamemode` is the one it shows).
    /// Errors are reported on the way; [`errors`](Self::errors) has them all.
    pub fn start(&self, gamemode: &str) {
        self.init();
        if self.realm() == Realm::Menu {
            *host(&self.lua).game.gamemode_name.borrow_mut() = gamemode.to_string();
            startup::start_menu(self);
        } else {
            startup::start(self, gamemode);
        }
    }

    /// Runs a Lua file as the engine does (`lua/` or `gamemodes/` relative, as `include`
    /// sees it). False if it was not found or failed; the error is reported.
    pub fn include(&self, path: &str) -> bool {
        files::include_from_engine(&self.lua, path)
    }

    /// Runs a piece of Lua source as `RunString` would, named `name` in errors. A failure is
    /// reported and returned.
    pub fn run_string(&self, code: &str, name: &str) -> Result<(), ScriptError> {
        let function = files::compile(&self.lua, code.as_bytes(), name).map_err(|e| {
            let error = ScriptError::from_lua(&e);
            host(&self.lua).report(error.clone());
            error
        })?;
        files::call_reporting(&self.lua, function, mlua::MultiValue::new()).map(drop)
    }

    /// Moves game time on by `seconds` - one frame - and runs what happens in it: the
    /// answers to `HTTP` requests that came in, the timers that are due, for every tick that
    /// passed the players' commands and movement, the bodies of physics, the inputs fired, the
    /// scripted entities that think and the `Tick` hook, each at its own `CurTime`; the `Think` hook; then the
    /// entities removed in the frame go.
    pub fn think(&self, seconds: f64) {
        let (before, end, ticks) = {
            let clock = &host(&self.lua).clock;
            let before = (clock.cur_time.get() / timer::TICK_INTERVAL).floor();
            let end = clock.cur_time.get() + seconds;
            clock.cur_time.set(end);
            clock.frame_time.set(seconds);
            let ticks = ((end / timer::TICK_INTERVAL).floor() - before).max(0.0) as u64;
            clock.ticks.set(clock.ticks.get() + ticks);
            (before, end, ticks)
        };
        convar::run_buffer(&self.lua);
        http::deliver(&self.lua);
        timer::run_timers(&self.lua);
        // a client does not simulate: what moves comes from the server
        let server = self.realm() == Realm::Server;
        for tick in 1..=ticks {
            host(&self.lua).clock.cur_time.set(((before + tick as f64) * timer::TICK_INTERVAL).min(end));
            if server {
                player::tick_all(&self.lua);
                physgun::think(&self.lua);
                physics::simulate(&self.lua);
                triggers::tick(&self.lua);
                inputs::service(&self.lua);
                logic::tick(&self.lua);
                movers::tick(&self.lua);
            }
            ents::think(&self.lua);
            if server {
                anim::advance(&self.lua);
            }
            self.call_hook("Tick", mlua::MultiValue::new());
        }
        host(&self.lua).clock.cur_time.set(end);
        self.call_hook("Think", mlua::MultiValue::new());
        ents::collect(&self.lua);
    }

    /// Loads the entities of a map, as the server does when the map starts (after
    /// [`start`](Self::start)): the world takes the `worldspawn` key-values, every other
    /// entity is made and spawned, and the `InitPostEntity` hook runs.
    pub fn spawn_map(&self, map: &ents::Map) {
        ents::spawn_map(&self.lua, map);
    }

    /// Runs console commands now, as typed into the server's console (`sv_gravity 300;
    /// my_command "a b"`).
    pub fn run_command(&self, line: &str) {
        for words in convar::split_commands(line) {
            convar::execute(&self.lua, &words);
        }
    }

    /// A player joins the game, as a client connecting does, and spawns. None when every
    /// player slot is taken ([`player::MAX_PLAYERS`]).
    pub fn join(&self, info: PlayerInfo) -> Option<EntityHandle> {
        player::join(&self.lua, info)
    }

    /// Runs console commands typed by a player, as a client's console sends them: the
    /// engine's commands for players (`noclip`, `kill`, `give`, choosing a weapon), their user
    /// info (`gmod_toolmode weld`), ConVars, and the commands of `concommand.Add`, which get
    /// the player. The host of the game - the
    /// first player - also has `lua_run`, the rest of the line being the code.
    pub fn run_player_command(&self, player: EntityHandle, line: &str) {
        let trimmed = line.trim_start();
        if let Some(code) = trimmed.get(..8).filter(|start| start.eq_ignore_ascii_case("lua_run ")).map(|_| &trimmed[8..])
            && player.index == Some(1)
        {
            // errors are reported as they happen
            let _ = self.run_string(code, "lua_run");
            return;
        }
        player::console(&self.lua, player, line);
    }

    /// A player's client says a value of its user info (`gmod_toolmode`), which `GetInfo`
    /// reads.
    pub fn set_user_info(&self, player: EntityHandle, name: &str, value: &str) {
        player::store_user_info(&self.lua, player, name, value);
    }

    /// What to show of a player: where they see from, their health, the weapon in hand.
    pub fn view(&self, player: EntityHandle) -> Option<PlayerView> {
        player::view(&self.lua, player)
    }

    /// The models of entities to draw as a player sees them: every entity with a model that
    /// is not hidden, but not the player and what they carry.
    pub fn drawn(&self, viewer: EntityHandle) -> Vec<Drawn> {
        ents::drawn(&self.lua, viewer)
    }

    /// What a player holds before their eyes: the view models of the weapon in hand (and the
    /// hands on them) at their eyes, with their bones, and the field of view they are drawn
    /// with. None while there is no weapon to show.
    pub fn view_models(&self, viewer: EntityHandle) -> (Vec<Drawn>, f32) {
        anim::view_models(&self.lua, viewer)
    }

    /// Where a client's Lua puts the view models, from the eyes of its player
    /// (`CalcViewModelView`).
    pub fn view_model_view(&self, origin: vector::Vector, angles: vector::Angle) -> (vector::Vector, vector::Angle) {
        anim::view_model_view(&self.lua, origin, angles)
    }

    /// How far game time is into the next tick, 0 to 1: where to draw between where things
    /// were at the last two ticks.
    pub fn tick_fraction(&self) -> f32 {
        let ticks = host(&self.lua).clock.cur_time.get() / timer::TICK_INTERVAL;
        (ticks - ticks.floor()) as f32
    }

    /// A player leaves the game.
    pub fn leave(&self, player: EntityHandle) {
        player::leave(&self.lua, player);
    }

    /// What a player asks for from now on, every tick until told otherwise: the keys held,
    /// where they look, how they move (as a client sends its commands). The impulse (100: the
    /// flashlight) and the mouse wheel go with the next tick only.
    pub fn set_input(&self, player: EntityHandle, cmd: UserCmd) {
        player::set_input(&self.lua, player, cmd);
    }

    /// The players in the game.
    pub fn players(&self) -> Vec<EntityHandle> {
        player::all(&self.lua)
    }

    /// Where an entity is in the world, if it is there.
    pub fn position(&self, entity: EntityHandle) -> Option<vector::Vector> {
        host(&self.lua).entities.transform(entity).map(|(pos, _)| pos)
    }

    /// Game time: what `CurTime` returns.
    pub fn cur_time(&self) -> f64 {
        host(&self.lua).clock.cur_time.get()
    }

    /// Calls a hook as the engine does: `hook.Call(name, GAMEMODE, ...)`. What the hook
    /// returned, or nothing when it failed (the error is reported) or the `hook` library is
    /// not loaded.
    pub fn call_hook(&self, name: &str, args: mlua::MultiValue) -> mlua::MultiValue {
        call_hook(&self.lua, name, args)
    }

    /// What Lua did with sounds since the last call, oldest first: what to play, change and
    /// stop.
    pub fn take_sounds(&self) -> Vec<sound::SoundEvent> {
        std::mem::take(&mut *host(&self.lua).sounds.events.borrow_mut())
    }

    /// The client's `DynamicLight`s that shine now.
    pub fn dynamic_lights(&self) -> Vec<client::DynamicLight> {
        client::dynamic_lights(&self.lua)
    }

    /// Where an entity is in the world now, if it is still there: for its sounds to follow.
    pub fn entity_position(&self, entity: entity::EntityHandle) -> Option<vector::Vector> {
        host(&self.lua).entities.transform(entity).map(|(pos, _)| pos)
    }

    /// The `net` messages sent since the last call, oldest first.
    pub fn take_net_messages(&self) -> Vec<net::NetMessage> {
        std::mem::take(&mut *host(&self.lua).net.sent.borrow_mut())
    }

    /// A `net` message arrives (from the player `from`, or from the server: `NULL`): the
    /// receiver Lua registered with `net.Receive` runs.
    pub fn receive_net_message(&self, name: &str, data: &net::Bits, from: entity::EntityHandle) {
        if let Err(e) = net::receive(&self.lua, name, data, from) {
            host(&self.lua).report(ScriptError::from_lua(&e));
        }
    }

    /// How many entities there are, the world included.
    pub fn entity_count(&self) -> usize {
        host(&self.lua).entities.handles().len()
    }

    /// Every error reported so far.
    pub fn errors(&self) -> Vec<ScriptError> {
        host(&self.lua).errors.borrow().clone()
    }

    /// The files `AddCSLuaFile` has sent to clients so far, as game paths.
    pub fn sent_files(&self) -> Vec<String> {
        host(&self.lua).sent.borrow().clone()
    }

    /// How many Lua files have been loaded.
    pub fn files_loaded(&self) -> usize {
        host(&self.lua).files_loaded.get()
    }
}

/// `hook.Call(name, GAMEMODE, ...)`, as the engine calls hooks: what the hook returned, or
/// nothing when it failed (the error is reported) or the `hook` library is not loaded.
pub(crate) fn call_hook(lua: &Lua, name: &str, args: mlua::MultiValue) -> mlua::MultiValue {
    let call = || -> mlua::Result<Option<(mlua::Function, mlua::MultiValue)>> {
        let Some(hook) = lua.globals().get::<Option<mlua::Table>>("hook")? else { return Ok(None) };
        let Some(call) = hook.get::<Option<mlua::Function>>("Call")? else { return Ok(None) };
        let mut full = mlua::MultiValue::new();
        full.push_back(mlua::Value::String(lua.create_string(name)?));
        full.push_back(engine::gamemode(lua)?);
        full.extend(args);
        Ok(Some((call, full)))
    };
    match call() {
        Ok(Some((call, args))) => files::call_reporting(lua, call, args).unwrap_or_default(),
        Ok(None) => mlua::MultiValue::new(),
        Err(e) => {
            host(lua).report(ScriptError::from_lua(&e));
            mlua::MultiValue::new()
        }
    }
}
