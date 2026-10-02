//! The standard libraries as GMod has them, minus bytecode.
//!
//! GMod opens LuaJIT's libraries and takes out what reaches the computer around the game:
//! `io`, `dofile`, `loadfile`, C modules, and `os` but for `clock`, `date`, `difftime` and
//! `time`. openGMOD does the same and goes one step further: LuaJIT runs bytecode without
//! verifying it, and crafted bytecode reads and writes memory anywhere - so `load`,
//! `loadstring` and every loader of this crate take source only. Source in GMod's dialect is
//! translated by [`gmod_glua`], as GMod's own parser would accept it.
//!
//! `table.insert` gives back where it put the value, as GMod's does.
//!
//! The `debug` library is GMod's, cut to what cannot reach into the runtime's own values:
//! upvalues and environments only of Lua functions, metatables only of what is not userdata,
//! no locals and no registry (GMod's `util.lua` replaces `debug.getregistry` anyway).

use mlua::chunk::ChunkMode;
use mlua::{Function, Lua, LuaOptions, MultiValue, StdLib, Table, Value};

/// The first byte of a LuaJIT bytecode dump (and of Lua's).
const BYTECODE_SIGNATURE: u8 = 0x1b;

/// `strftime` conversions every C library knows. Others make the Microsoft C library end the
/// program instead of failing, so `os.date` refuses them before they get there.
const DATE_CONVERSIONS: &[u8] = b"aAbBcdHIjmMpSUwWxXyYZ%";

/// A LuaJIT state with the libraries GMod opens.
pub(crate) fn new_state() -> mlua::Result<Lua> {
    // in LuaJIT `coroutine` comes with the base library
    let libs = StdLib::TABLE | StdLib::OS | StdLib::STRING | StdLib::BIT | StdLib::MATH | StdLib::PACKAGE | StdLib::JIT | StdLib::DEBUG;
    // SAFETY: mlua calls the `debug` library unsafe because, untouched, it reaches values
    // mlua relies on (the registry, the upvalues of Rust functions, the metatables of
    // userdata). `install` replaces every such function before any script runs; FFI is
    // not opened, and C modules are disabled by `install` too.
    #[allow(unsafe_code)]
    let lua = unsafe { Lua::unsafe_new_with(libs, LuaOptions::new()) };
    Ok(lua)
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    globals.raw_remove("dofile")?;
    globals.raw_remove("loadfile")?;
    globals.raw_set("load", lua.create_function(load)?)?;
    globals.raw_set("loadstring", lua.create_function(loadstring)?)?;

    // os: time and dates only
    let os: Table = globals.get("os")?;
    let trimmed = lua.create_table()?;
    for name in ["clock", "difftime", "time"] {
        trimmed.raw_set(name, os.get::<Value>(name)?)?;
    }
    let date: Function = os.get("date")?;
    trimmed.raw_set(
        "date",
        lua.create_function(move |_, args: MultiValue| {
            if let Some(Value::String(format)) = args.front() {
                check_date_format(&format.as_bytes())?;
            }
            date.call::<MultiValue>(args)
        })?,
    )?;
    globals.raw_set("os", trimmed)?;

    // package: `module` and `package.loaded` stay; finding files is `require`'s, in the game
    // file system, and there are no C modules
    let package: Table = globals.get("package")?;
    package.raw_remove("loadlib")?;
    package.raw_set("path", "")?;
    package.raw_set("cpath", "")?;
    let loaders: Table = package.get("loaders")?;
    let preload: Value = loaders.raw_get(1)?;
    let only_preload = lua.create_table()?;
    only_preload.raw_set(1, preload)?;
    package.raw_set("loaders", only_preload)?;

    let table: Table = globals.get("table")?;
    table.raw_set("insert", lua.create_function(table_insert)?)?;

    install_debug(lua)?;
    Ok(())
}

/// `table.insert(t, [pos,] value)`, as LuaJIT's, giving back the position.
fn table_insert(lua: &Lua, args: MultiValue) -> mlua::Result<i64> {
    let a = crate::args::Args::new(lua, args.clone(), "insert");
    let t = a.table(1)?;
    let end = t.raw_len() as i64 + 1;
    match args.len() {
        2 => {
            t.raw_set(end, args[1].clone())?;
            Ok(end)
        }
        3 => {
            let pos = a.number(2)? as i64;
            // the elements from `pos` move up one
            for i in (pos + 1..=end.max(pos)).rev() {
                t.raw_set(i, t.raw_get::<Value>(i - 1)?)?;
            }
            t.raw_set(pos, args[2].clone())?;
            Ok(pos)
        }
        _ => Err(mlua::Error::runtime("wrong number of arguments to 'insert'")),
    }
}

fn check_date_format(format: &[u8]) -> mlua::Result<()> {
    let format = format.strip_prefix(b"!").unwrap_or(format);
    let mut bytes = format.iter();
    while let Some(&b) = bytes.next() {
        if b == b'%' && !bytes.next().is_some_and(|c| DATE_CONVERSIONS.contains(c)) {
            return Err(mlua::Error::RuntimeError("bad argument #1 to 'date' (invalid conversion specifier)".into()));
        }
    }
    Ok(())
}

/// Compiles source - never bytecode - in GMod's dialect, named `name` (Lua's chunk name:
/// `@file`, `=label`, or the source itself).
pub(crate) fn compile_source(lua: &Lua, source: &[u8], name: &str) -> mlua::Result<Function> {
    if source.first() == Some(&BYTECODE_SIGNATURE) {
        return Err(mlua::Error::SyntaxError {
            message: format!("{}: bytecode is not allowed, only source", chunk_label(name)),
            incomplete_input: false,
        });
    }
    let translated = gmod_glua::translate(source).map_err(|e| mlua::Error::SyntaxError {
        message: format!("{}:{}: {}", chunk_label(name), e.line, e.message),
        incomplete_input: false,
    })?;
    // a NUL in the name makes it unusable as a C string; Lua would cut it there
    let name = name.split('\0').next().unwrap_or_default();
    lua.load(translated.as_ref()).set_name(name).set_mode(ChunkMode::Text).into_function()
}

/// How Lua shows a chunk name in messages.
fn chunk_label(name: &str) -> String {
    if let Some(file) = name.strip_prefix('@').or_else(|| name.strip_prefix('=')) {
        file.to_string()
    } else {
        let line = name.lines().next().unwrap_or_default();
        let short: String = line.chars().take(60).collect();
        let more = if short.len() < name.len() { "..." } else { "" };
        format!("[string \"{short}{more}\"]")
    }
}

/// The `(nil, message)` Lua's loaders return for a chunk that does not compile.
fn failed(lua: &Lua, error: mlua::Error) -> mlua::Result<MultiValue> {
    let message = match error {
        mlua::Error::SyntaxError { message, .. } => message,
        other => other.to_string(),
    };
    Ok(MultiValue::from_iter([Value::Nil, Value::String(lua.create_string(message)?)]))
}

fn with_env(f: &Function, env: Value) -> mlua::Result<()> {
    if let Value::Table(env) = env {
        f.set_environment(env)?;
    }
    Ok(())
}

/// `loadstring(source [, chunkname])`.
fn loadstring(lua: &Lua, (source, name): (mlua::LuaString, Option<mlua::LuaString>)) -> mlua::Result<MultiValue> {
    let bytes = source.as_bytes().to_vec();
    let name = match name {
        Some(name) => name.to_string_lossy(),
        None => String::from_utf8_lossy(&bytes).into_owned(),
    };
    match compile_source(lua, &bytes, &name) {
        Ok(f) => Ok(MultiValue::from_iter([Value::Function(f)])),
        Err(e) => failed(lua, e),
    }
}

/// `load(source or reader [, chunkname [, mode [, env]]])`, LuaJIT 2.1's.
fn load(lua: &Lua, (chunk, name, mode, env): (Value, Option<mlua::LuaString>, Option<mlua::LuaString>, Value)) -> mlua::Result<MultiValue> {
    if let Some(mode) = &mode
        && !mode.as_bytes().contains(&b't')
    {
        return failed(
            lua,
            mlua::Error::SyntaxError { message: "attempt to load a binary chunk: not allowed".into(), incomplete_input: false },
        );
    }
    let (bytes, default_name) = match chunk {
        Value::String(s) => {
            let bytes = s.as_bytes().to_vec();
            let name = String::from_utf8_lossy(&bytes).into_owned();
            (bytes, name)
        }
        Value::Function(reader) => {
            let mut bytes = Vec::new();
            loop {
                match reader.call::<Value>(())? {
                    Value::String(piece) if !piece.as_bytes().is_empty() => bytes.extend_from_slice(&piece.as_bytes()),
                    Value::Nil | Value::String(_) => break,
                    _ => {
                        return failed(
                            lua,
                            mlua::Error::SyntaxError { message: "reader function must return a string".into(), incomplete_input: false },
                        );
                    }
                }
            }
            (bytes, "=(load)".to_string())
        }
        other => {
            return Err(mlua::Error::RuntimeError(format!("bad argument #1 to 'load' (function expected, got {})", other.type_name())));
        }
    };
    let name = name.map(|n| n.to_string_lossy()).unwrap_or(default_name);
    match compile_source(lua, &bytes, &name) {
        Ok(f) => {
            with_env(&f, env)?;
            Ok(MultiValue::from_iter([Value::Function(f)]))
        }
        Err(e) => failed(lua, e),
    }
}

fn is_lua_function(value: &Value) -> bool {
    matches!(value, Value::Function(f) if f.info().what != "C")
}

fn refuse(function: &str, what: &str) -> mlua::Error {
    mlua::Error::RuntimeError(format!("debug.{function}: not allowed on {what}"))
}

/// Replaces the `debug` library with the part that keeps the runtime's own values out of
/// reach. The original functions live on only inside these Rust functions, whose upvalues
/// Lua cannot read.
fn install_debug(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    let full: Table = globals.get("debug")?;
    let debug = lua.create_table()?;
    for name in ["getinfo", "traceback", "sethook", "gethook"] {
        debug.raw_set(name, full.get::<Function>(name)?)?;
    }

    // upvalues: of Lua functions only, not of the Rust functions behind the API
    for name in ["getupvalue", "setupvalue", "upvalueid"] {
        let original: Function = full.get(name)?;
        debug.raw_set(
            name,
            lua.create_function(move |_, args: MultiValue| {
                if !args.front().is_some_and(is_lua_function) {
                    return Err(refuse(name, "a function that is not Lua"));
                }
                original.call::<MultiValue>(args)
            })?,
        )?;
    }
    let upvaluejoin: Function = full.get("upvaluejoin")?;
    debug.raw_set(
        "upvaluejoin",
        lua.create_function(move |_, args: MultiValue| {
            let lua_functions = args.front().is_some_and(is_lua_function) && args.get(2).is_some_and(is_lua_function);
            if !lua_functions {
                return Err(refuse("upvaluejoin", "a function that is not Lua"));
            }
            upvaluejoin.call::<MultiValue>(args)
        })?,
    )?;

    // metatables and environments: not of userdata, whose metatables are the runtime's
    let getmetatable: Function = full.get("getmetatable")?;
    let protected_getmetatable: Function = globals.get("getmetatable")?;
    debug.raw_set(
        "getmetatable",
        lua.create_function(move |_, value: Value| match value {
            Value::UserData(_) | Value::LightUserData(_) => protected_getmetatable.call::<Value>(value),
            other => getmetatable.call::<Value>(other),
        })?,
    )?;
    let setmetatable: Function = full.get("setmetatable")?;
    debug.raw_set(
        "setmetatable",
        lua.create_function(move |_, (value, meta): (Value, Value)| match value {
            Value::UserData(_) | Value::LightUserData(_) => Err(refuse("setmetatable", "userdata")),
            other => setmetatable.call::<Value>((other, meta)),
        })?,
    )?;
    let getfenv: Function = full.get("getfenv")?;
    debug.raw_set(
        "getfenv",
        lua.create_function(move |_, value: Value| match value {
            Value::UserData(_) | Value::LightUserData(_) => Err(refuse("getfenv", "userdata")),
            other => getfenv.call::<Value>(other),
        })?,
    )?;
    let setfenv: Function = full.get("setfenv")?;
    debug.raw_set(
        "setfenv",
        lua.create_function(move |_, (value, env): (Value, Value)| {
            if !is_lua_function(&value) && !matches!(value, Value::Thread(_)) {
                return Err(refuse("setfenv", "anything but Lua functions and coroutines"));
            }
            setfenv.call::<Value>((value, env))
        })?,
    )?;

    debug.raw_set("getregistry", lua.create_function(|lua, ()| lua.create_table())?)?;
    globals.raw_set("debug", debug)?;
    Ok(())
}
