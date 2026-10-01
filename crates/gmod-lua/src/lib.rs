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
//! * the engine's globals ([`engine`]): realm flags, time, versions, and the functions the
//!   game's own Lua calls while it starts. They grow towards the whole API one library at a
//!   time.
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
pub mod engine;
pub mod files;
pub mod meta;
pub mod sandbox;
pub mod vector;

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
}

impl Host {
    pub(crate) fn print(&self, text: &str) {
        (self.output.borrow_mut())(Output::Text(text));
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
        });
        // `getmetatable` first: the sandbox's `debug.getmetatable` asks it about userdata
        meta::install(&lua)?;
        sandbox::install(&lua)?;
        console::install(&lua)?;
        files::install(&lua)?;
        vector::install(&lua)?;
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

    /// Runs the file the engine starts the realm with ([`Realm::init_file`]). False if it
    /// could not be run at all; errors inside it are reported as usual.
    pub fn init(&self) -> bool {
        self.include(self.realm().init_file())
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
