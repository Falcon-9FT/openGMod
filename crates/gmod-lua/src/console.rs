//! What scripts print: `print`, `Msg`, `MsgN`, `MsgC`, and the errors they report without
//! stopping (`ErrorNoHalt`, `ErrorNoHaltWithStack`) or with (`Error`).

use mlua::{Function, Lua, MultiValue, Value};

use crate::{ScriptError, host};

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    globals.raw_set(
        "print",
        lua.create_function(|lua, args: MultiValue| {
            let mut text = join(lua, &args, "\t")?;
            text.push('\n');
            host(lua).print(&text);
            Ok(())
        })?,
    )?;
    globals.raw_set(
        "Msg",
        lua.create_function(|lua, args: MultiValue| {
            host(lua).print(&join(lua, &args, "")?);
            Ok(())
        })?,
    )?;
    globals.raw_set(
        "MsgN",
        lua.create_function(|lua, args: MultiValue| {
            let mut text = join(lua, &args, "")?;
            text.push('\n');
            host(lua).print(&text);
            Ok(())
        })?,
    )?;
    // colours are for the console's eyes; the text is what is printed
    globals.raw_set(
        "MsgC",
        lua.create_function(|lua, args: MultiValue| {
            let text: MultiValue = args.into_iter().filter(|v| !is_color(v)).collect();
            host(lua).print(&join(lua, &text, "")?);
            Ok(())
        })?,
    )?;
    globals.raw_set(
        "ErrorNoHalt",
        lua.create_function(|lua, args: MultiValue| {
            let message = join(lua, &args, "")?;
            host(lua).report(ScriptError::new(message.trim_end()));
            Ok(())
        })?,
    )?;
    globals.raw_set(
        "ErrorNoHaltWithStack",
        lua.create_function(|lua, args: MultiValue| {
            let message = join(lua, &args, "")?;
            let mut error = ScriptError::new(message.trim_end());
            error.stack = stack(lua);
            host(lua).report(error);
            Ok(())
        })?,
    )?;
    globals.raw_set(
        "Error",
        lua.create_function(|lua, args: MultiValue| -> mlua::Result<()> { Err(mlua::Error::RuntimeError(join(lua, &args, "")?)) })?,
    )?;
    Ok(())
}

/// The arguments through Lua's own `tostring` (which scripts may have replaced), joined.
pub(crate) fn join(lua: &Lua, args: &MultiValue, separator: &str) -> mlua::Result<String> {
    let tostring: Function = lua.globals().get("tostring")?;
    let mut text = String::new();
    for (i, arg) in args.iter().enumerate() {
        if i > 0 {
            text.push_str(separator);
        }
        match arg {
            Value::String(s) => text.push_str(&s.to_string_lossy()),
            other => text.push_str(&tostring.call::<mlua::LuaString>(other)?.to_string_lossy()),
        }
    }
    Ok(text)
}

/// A `Color`: a table with `r`, `g` and `b`.
fn is_color(value: &Value) -> bool {
    let Value::Table(t) = value else { return false };
    ["r", "g", "b"].iter().all(|k| matches!(t.raw_get::<Value>(*k), Ok(Value::Integer(_) | Value::Number(_))))
}

/// The Lua stack of the caller, innermost first, as `ScriptError` lists it.
pub(crate) fn stack(lua: &Lua) -> Vec<String> {
    let mut frames = Vec::new();
    // level 0 is the Rust function that asks
    for level in 1.. {
        let Some(frame) = lua.inspect_stack(level, |debug| {
            let source = debug.source();
            let name = debug.names().name.map(|n| n.into_owned());
            // the runtime's own calls (`xpcall` around a file) are no part of the script's stack
            if source.what == "C" && name.is_none() {
                return None;
            }
            let name = name.unwrap_or_else(|| "unknown".into());
            let place = source.short_src.as_deref().unwrap_or("?").to_string();
            Some(match debug.current_line() {
                Some(line) => format!("{name} - {place}:{line}"),
                None => format!("{name} - {place}"),
            })
        }) else {
            break;
        };
        frames.extend(frame);
    }
    frames
}
