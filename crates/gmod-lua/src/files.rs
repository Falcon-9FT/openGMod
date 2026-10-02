//! Loading Lua as GMod does: `include`, `require`, `AddCSLuaFile`, `CompileString`,
//! `CompileFile`, `RunString`.
//!
//! Lua files are looked up in the `LUA` search path: the `lua/` and `gamemodes/` folders of
//! every mount (the game's packs, the game folder, addons). A name is tried relative to the
//! folder of the file that asks first, then from the root of the search path, so
//! `include("shared.lua")` in `gamemodes/sandbox/gamemode/init.lua` is the sandbox's own
//! `shared.lua`, and `include("util.lua")` in `lua/includes/init.lua` is `lua/includes/util.lua`.
//! Chunks are named after their game path (`@lua/includes/init.lua`), which is what errors and
//! `debug.getinfo` show, as in GMod.
//!
//! A file that fails is reported and does not stop the file that included it, as in GMod.
//! `require` is Lua's own: it loads `includes/modules/<name>.lua` once, and its errors are
//! errors of the caller. Binary modules (`lua/bin/gmsv_*.dll`) are never loaded.

use mlua::{Function, Lua, MultiValue, Value};

use crate::sandbox::compile_source;
use crate::{Realm, ScriptError, host};

/// The folders of the `LUA` search path, in lookup order.
const ROOTS: [&str; 2] = ["lua", "gamemodes"];

const XPCALL: &str = "gmod-lua.xpcall";
const HANDLER: &str = "gmod-lua.handler";

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    lua.set_named_registry_value(XPCALL, globals.get::<Function>("xpcall")?)?;
    lua.set_named_registry_value(HANDLER, lua.create_function(error_handler)?)?;

    globals.raw_set("include", lua.create_function(include)?)?;
    globals.raw_set("require", lua.create_function(require)?)?;
    globals.raw_set("AddCSLuaFile", lua.create_function(add_cs_lua_file)?)?;
    globals.raw_set("CompileString", lua.create_function(compile_string)?)?;
    globals.raw_set("CompileFile", lua.create_function(compile_file)?)?;
    globals.raw_set("RunString", lua.create_function(run_string)?)?;
    globals.raw_set("RunStringEx", lua.create_function(run_string)?)?;
    globals.raw_set("ProtectedCall", lua.create_function(protected_call)?)?;
    Ok(())
}

/// `ProtectedCall(f, ...)`: calls `f`, reporting an error it raises rather than passing it on.
/// Whether it ran through.
fn protected_call(lua: &Lua, mut values: MultiValue) -> mlua::Result<bool> {
    match values.pop_front() {
        Some(Value::Function(f)) => Ok(call_reporting(lua, f, values).is_ok()),
        other => Err(crate::args::bad(lua, 1, "ProtectedCall", "function", other.as_ref())),
    }
}

/// Compiles a chunk the way `CompileString` names it.
pub(crate) fn compile(lua: &Lua, source: &[u8], name: &str) -> mlua::Result<Function> {
    compile_source(lua, source, &format!("@{name}"))
}

/// The game path of the file whose code is running at `level` (1: the caller of the Rust
/// function asking), if it is a file of the `LUA` search path.
fn file_at(lua: &Lua, level: usize) -> Option<String> {
    let source = lua.inspect_stack(level, |debug| debug.source().source.map(|s| s.into_owned())).flatten()?;
    let path = source.strip_prefix('@')?;
    ROOTS.iter().any(|root| path.strip_prefix(root).is_some_and(|rest| rest.starts_with('/'))).then(|| path.to_string())
}

/// `lua/includes/init.lua` -> `includes`: the folder a name is relative to.
fn folder_in_search_path(path: &str) -> &str {
    let inside = ROOTS.iter().find_map(|root| path.strip_prefix(root).and_then(|rest| rest.strip_prefix('/'))).unwrap_or(path);
    inside.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// The game path of the Lua file `name` refers to from the file `from`, if it exists.
fn find(lua: &Lua, name: &str, from: Option<&str>) -> Option<String> {
    let host = host(lua);
    let relative = from.map(|file| format!("{}/{name}", folder_in_search_path(file)));
    relative.iter().map(String::as_str).chain([name]).find_map(|candidate| {
        let candidate = gmod_fs::path::normalize(candidate)?;
        ROOTS.iter().map(|root| format!("{root}/{candidate}")).find(|path| host.vfs.locate(path, "GAME").is_some())
    })
}

/// Reads and compiles a file of the game.
fn load_file(lua: &Lua, path: &str) -> mlua::Result<Function> {
    let bytes = host(lua).vfs.read(path, "GAME").map_err(|e| mlua::Error::RuntimeError(format!("{path}: {e}")))?;
    let function = compile_source(lua, &bytes, &format!("@{path}"))?;
    let host = host(lua);
    host.files_loaded.set(host.files_loaded.get() + 1);
    Ok(function)
}

/// The message handler of [`call_reporting`]: the message, and the stack where the error
/// was raised, which is gone once `xpcall` returns.
fn error_handler(lua: &Lua, error: Value) -> mlua::Result<mlua::LuaString> {
    let message = match &error {
        Value::String(s) => s.to_string_lossy(),
        Value::Error(e) => ScriptError::from_lua(e).message,
        Value::Nil => "nil".to_string(),
        other => crate::console::join(lua, &MultiValue::from_iter([other.clone()]), "").unwrap_or_else(|_| other.type_name().into()),
    };
    *host(lua).pending_stack.borrow_mut() = crate::console::stack(lua);
    lua.create_string(message)
}

/// Calls `function`; an error is reported - with the stack where it happened - and returned.
pub(crate) fn call_reporting(lua: &Lua, function: Function, args: MultiValue) -> Result<MultiValue, ScriptError> {
    let xpcall: Function = lua.named_registry_value(XPCALL).map_err(|e| ScriptError::from_lua(&e))?;
    let handler: Function = lua.named_registry_value(HANDLER).map_err(|e| ScriptError::from_lua(&e))?;
    let mut call_args = MultiValue::from_iter([Value::Function(function), Value::Function(handler)]);
    call_args.extend(args);
    let error = match xpcall.call::<MultiValue>(call_args) {
        Ok(mut results) => match results.pop_front() {
            Some(Value::Boolean(true)) => return Ok(results),
            _ => {
                let message = match results.pop_front() {
                    Some(Value::String(s)) => s.to_string_lossy(),
                    _ => "unknown error".into(),
                };
                let stack = std::mem::take(&mut *host(lua).pending_stack.borrow_mut());
                ScriptError { message, stack }
            }
        },
        Err(e) => ScriptError::from_lua(&e),
    };
    host(lua).report(error.clone());
    Err(error)
}

fn string_arg(value: &Value, position: usize, function: &str) -> mlua::Result<String> {
    match value {
        Value::String(s) => Ok(s.to_string_lossy()),
        Value::Integer(n) => Ok(n.to_string()),
        Value::Number(n) => Ok(n.to_string()),
        other => Err(mlua::Error::RuntimeError(format!(
            "bad argument #{position} to '{function}' (string expected, got {})",
            if other.is_nil() { "no value" } else { other.type_name() }
        ))),
    }
}

/// Where the caller is, for messages: `lua/file.lua:12`.
fn caller_place(lua: &Lua) -> String {
    lua.inspect_stack(1, |debug| {
        let src = debug.source().short_src.map(|s| s.into_owned()).unwrap_or_else(|| "?".into());
        match debug.current_line() {
            Some(line) => format!("{src}:{line}"),
            None => src,
        }
    })
    .unwrap_or_else(|| "engine".into())
}

/// `include(file)`: runs a Lua file and returns what it returns; a failure is reported.
fn include(lua: &Lua, name: Value) -> mlua::Result<MultiValue> {
    let name = string_arg(&name, 1, "include")?;
    let from = file_at(lua, 1);
    let Some(path) = find(lua, &name, from.as_deref()) else {
        let place = caller_place(lua);
        host(lua).report(ScriptError::new(format!("Couldn't include file '{name}' - File not found ({place})")));
        return Ok(MultiValue::new());
    };
    run_reporting(lua, &path).or_else(|_| Ok(MultiValue::new()))
}

fn run_reporting(lua: &Lua, path: &str) -> Result<MultiValue, ScriptError> {
    let function = load_file(lua, path).map_err(|e| {
        let error = ScriptError::from_lua(&e);
        host(lua).report(error.clone());
        error
    })?;
    call_reporting(lua, function, MultiValue::new())
}

/// The engine runs a file (`includes/init.lua`, a gamemode, autorun): as `include` from
/// nowhere. False when it is not there or fails.
pub(crate) fn include_from_engine(lua: &Lua, name: &str) -> bool {
    match find(lua, name, None) {
        Some(path) => run_reporting(lua, &path).is_ok(),
        None => {
            host(lua).report(ScriptError::new(format!("Couldn't include file '{name}' - File not found")));
            false
        }
    }
}

/// `require(name)`: `includes/modules/<name>.lua`, once.
fn require(lua: &Lua, name: Value) -> mlua::Result<Value> {
    let name = string_arg(&name, 1, "require")?;
    let package: mlua::Table = lua.globals().get("package")?;
    let loaded: mlua::Table = package.get("loaded")?;
    let already: Value = loaded.raw_get(name.as_str())?;
    if !already.is_nil() {
        return Ok(already);
    }
    let Some(path) = find(lua, &format!("includes/modules/{name}.lua"), None) else {
        return Err(mlua::Error::RuntimeError("Module not found!".into()));
    };
    let function = load_file(lua, &path)?;
    let result: Value = function.call(name.as_str())?;
    if !result.is_nil() {
        loaded.raw_set(name.as_str(), result)?;
    }
    if loaded.raw_get::<Value>(name.as_str())?.is_nil() {
        loaded.raw_set(name.as_str(), true)?;
    }
    loaded.raw_get(name.as_str())
}

/// `AddCSLuaFile([file])`: on the server, sends a file - the calling one without an
/// argument - to clients. Elsewhere it does nothing.
fn add_cs_lua_file(lua: &Lua, name: Option<Value>) -> mlua::Result<()> {
    if host(lua).realm != Realm::Server {
        return Ok(());
    }
    let from = file_at(lua, 1);
    let path = match &name {
        None | Some(Value::Nil) => from,
        Some(name) => {
            let name = string_arg(name, 1, "AddCSLuaFile")?;
            let found = find(lua, &name, from.as_deref());
            if found.is_none() {
                let place = caller_place(lua);
                host(lua).report(ScriptError::new(format!("Couldn't AddCSLuaFile file '{name}' - File not found ({place})")));
            }
            found
        }
    };
    if let Some(path) = path {
        let host = host(lua);
        let mut sent = host.sent.borrow_mut();
        if !sent.contains(&path) {
            sent.push(path);
        }
    }
    Ok(())
}

/// `CompileString(code, identifier [, handleError])`: the function, or - when not handling
/// errors itself - the message.
fn compile_string(lua: &Lua, (code, identifier, handle): (mlua::LuaString, Option<String>, Option<bool>)) -> mlua::Result<Value> {
    let identifier = identifier.unwrap_or_else(|| "CompileString".into());
    match compile(lua, &code.as_bytes(), &identifier) {
        Ok(function) => Ok(Value::Function(function)),
        Err(e) => {
            let error = ScriptError::from_lua(&e);
            if handle.unwrap_or(true) {
                host(lua).report(error);
                Ok(Value::Nil)
            } else {
                Ok(Value::String(lua.create_string(error.message)?))
            }
        }
    }
}

/// `RunString(code [, identifier [, handleError]])`: nothing, or the error message.
fn run_string(lua: &Lua, (code, identifier, handle): (mlua::LuaString, Option<String>, Option<bool>)) -> mlua::Result<Value> {
    let identifier = identifier.unwrap_or_else(|| "RunString".into());
    let handle = handle.unwrap_or(true);
    let function = match compile(lua, &code.as_bytes(), &identifier) {
        Ok(function) => function,
        Err(e) => {
            let error = ScriptError::from_lua(&e);
            if handle {
                host(lua).report(error);
                return Ok(Value::Nil);
            }
            return Ok(Value::String(lua.create_string(error.message)?));
        }
    };
    if handle {
        let _ = call_reporting(lua, function, MultiValue::new());
        return Ok(Value::Nil);
    }
    match function.call::<()>(()) {
        Ok(()) => Ok(Value::Nil),
        Err(e) => Ok(Value::String(lua.create_string(ScriptError::from_lua(&e).message)?)),
    }
}

/// `CompileFile(path)`: a file of the `LUA` search path as a function, not run.
fn compile_file(lua: &Lua, name: Value) -> mlua::Result<Value> {
    let name = string_arg(&name, 1, "CompileFile")?;
    let Some(path) = find(lua, &name, None) else { return Ok(Value::Nil) };
    match load_file(lua, &path) {
        Ok(function) => Ok(Value::Function(function)),
        Err(e) => {
            host(lua).report(ScriptError::from_lua(&e));
            Ok(Value::Nil)
        }
    }
}
