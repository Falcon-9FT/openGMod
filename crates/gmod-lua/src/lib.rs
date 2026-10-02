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
//!   (`Vector`, `Angle`), [`matrix`] (`VMatrix`), [`entity`] (`NULL`, the metatables of `Entity`, `Player` ...),
//!   [`timer`] (`CurTime`, `timer.*`), [`convar`] (ConVars, console commands), [`filesystem`]
//!   (`file`, `File`), [`util`], [`net`], [`sql`] (SQLite), [`http`] (`HTTP`), [`material`] (`Material`), [`engine`] (`game`,
//!   `engine`, `system`, `gmod`, the enumerations), and [`world`] (`ents`, `player` ... as
//!   they answer while there are no entities). They grow towards the whole API - `gmod-check
//!   lua` counts how much of it is there.
//! * starting the game ([`Runtime::start`]): `includes/init.lua`, autorun, the gamemode and
//!   its bases, scripted entities and weapons, the hooks up to `Initialize`; then
//!   [`Runtime::think`] moves time on, runs timers and the `Think` hook.
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

mod args;
pub mod console;
pub mod convar;
pub mod engine;
pub mod entity;
pub mod files;
pub mod filesystem;
pub mod http;
pub mod material;
pub mod matrix;
pub mod meta;
pub mod net;
pub mod sandbox;
pub mod sql;
mod startup;
pub mod timer;
pub mod util;
pub mod vector;
pub mod world;

#[cfg(test)]
mod tests;

pub use mlua;

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
        material::install(&lua)?;
        world::install(&lua)?;
        sql::install(&lua)?;
        http::install(&lua)?;
        net::install(&lua, realm)?;
        startup::install(&lua)?;
        engine::install(&lua, realm)?;
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
    /// its bases, scripted entities and weapons, and the hooks up to `Initialize`. Errors
    /// are reported on the way; [`errors`](Self::errors) has them all.
    pub fn start(&self, gamemode: &str) {
        self.init();
        if self.realm() != Realm::Menu {
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
    /// answers to `HTTP` requests that came in, the timers that are due, the `Tick` hook for
    /// every tick that passed, the `Think` hook.
    pub fn think(&self, seconds: f64) {
        let ticks = {
            let clock = &host(&self.lua).clock;
            let before = (clock.cur_time.get() / timer::TICK_INTERVAL).floor();
            clock.cur_time.set(clock.cur_time.get() + seconds);
            clock.frame_time.set(seconds);
            let ticks = ((clock.cur_time.get() / timer::TICK_INTERVAL).floor() - before).max(0.0) as u64;
            clock.ticks.set(clock.ticks.get() + ticks);
            ticks
        };
        convar::run_buffer(&self.lua);
        http::deliver(&self.lua);
        timer::run_timers(&self.lua);
        for _ in 0..ticks {
            self.call_hook("Tick", mlua::MultiValue::new());
        }
        self.call_hook("Think", mlua::MultiValue::new());
    }

    /// Runs console commands now, as typed into the server's console (`sv_gravity 300;
    /// my_command "a b"`).
    pub fn run_command(&self, line: &str) {
        for words in convar::split_commands(line) {
            convar::execute(&self.lua, &words);
        }
    }

    /// Game time: what `CurTime` returns.
    pub fn cur_time(&self) -> f64 {
        host(&self.lua).clock.cur_time.get()
    }

    /// Calls a hook as the engine does: `hook.Call(name, GAMEMODE, ...)`. What the hook
    /// returned, or nothing when it failed (the error is reported) or the `hook` library is
    /// not loaded.
    pub fn call_hook(&self, name: &str, args: mlua::MultiValue) -> mlua::MultiValue {
        let call = || -> mlua::Result<Option<(mlua::Function, mlua::MultiValue)>> {
            let Some(hook) = self.lua.globals().get::<Option<mlua::Table>>("hook")? else { return Ok(None) };
            let Some(call) = hook.get::<Option<mlua::Function>>("Call")? else { return Ok(None) };
            let mut full = mlua::MultiValue::new();
            full.push_back(mlua::Value::String(self.lua.create_string(name)?));
            full.push_back(engine::gamemode(&self.lua)?);
            full.extend(args);
            Ok(Some((call, full)))
        };
        match call() {
            Ok(Some((call, args))) => files::call_reporting(&self.lua, call, args).unwrap_or_default(),
            Ok(None) => mlua::MultiValue::new(),
            Err(e) => {
                host(&self.lua).report(ScriptError::from_lua(&e));
                mlua::MultiValue::new()
            }
        }
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
