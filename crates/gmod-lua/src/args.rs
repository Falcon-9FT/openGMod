//! Arguments of the functions given to Lua, checked with GMod's messages:
//! `lua/file.lua:12: bad argument #1 to 'Dot' (Vector expected, got number)`.

use mlua::{Lua, MultiValue, Value};

use crate::meta;

/// Where the Lua code that called the running Rust function is: `lua/file.lua:12: `, or
/// nothing when that is not Lua.
pub(crate) fn place(lua: &Lua) -> String {
    lua.inspect_stack(1, |debug| {
        let source = debug.source();
        match (source.what, debug.current_line(), source.short_src) {
            ("C", ..) | (_, None, _) | (_, _, None) => String::new(),
            (_, Some(line), Some(src)) => format!("{src}:{line}: "),
        }
    })
    .unwrap_or_default()
}

/// A Lua error raised by the running Rust function, with the place of its caller in front.
pub(crate) fn error(lua: &Lua, message: impl std::fmt::Display) -> mlua::Error {
    mlua::Error::RuntimeError(format!("{}{message}", place(lua)))
}

/// The name of a value's type as GMod's `type` gives it.
pub(crate) fn type_name(value: Option<&Value>) -> String {
    match value {
        None => "no value".into(),
        Some(Value::UserData(ud)) => meta::type_name(ud).unwrap_or_else(|| "userdata".into()),
        Some(Value::Error(_)) => "userdata".into(),
        // LuaJIT has one kind of number; mlua tells integers apart
        Some(Value::Integer(_)) => "number".into(),
        Some(other) => other.type_name().into(),
    }
}

pub(crate) fn bad(lua: &Lua, position: usize, function: &str, expected: &str, got: Option<&Value>) -> mlua::Error {
    error(lua, format!("bad argument #{position} to '{function}' ({expected} expected, got {})", type_name(got)))
}

/// The arguments of one call, for the function `name`.
pub(crate) struct Args<'a> {
    pub lua: &'a Lua,
    pub values: MultiValue,
    pub name: &'static str,
}

impl<'a> Args<'a> {
    pub fn new(lua: &'a Lua, values: MultiValue, name: &'static str) -> Args<'a> {
        Args { lua, values, name }
    }

    /// The argument at `position`, counted from 1.
    pub fn get(&self, position: usize) -> Option<&Value> {
        self.values.get(position - 1).filter(|v| !v.is_nil())
    }

    pub fn bad(&self, position: usize, expected: &str) -> mlua::Error {
        bad(self.lua, position, self.name, expected, self.values.get(position - 1))
    }

    pub fn number(&self, position: usize) -> mlua::Result<f64> {
        self.opt_number(position)?.ok_or_else(|| self.bad(position, "number"))
    }

    /// A number, or a string Lua would turn into one, as `luaL_checknumber` takes it.
    pub fn opt_number(&self, position: usize) -> mlua::Result<Option<f64>> {
        match self.get(position) {
            None => Ok(None),
            Some(Value::Integer(n)) => Ok(Some(*n as f64)),
            Some(Value::Number(n)) => Ok(Some(*n)),
            Some(value @ Value::String(_)) => match self.lua.coerce_number(value.clone())? {
                Some(n) => Ok(Some(n)),
                None => Err(self.bad(position, "number")),
            },
            Some(_) => Err(self.bad(position, "number")),
        }
    }

    pub fn string(&self, position: usize) -> mlua::Result<String> {
        self.opt_string(position)?.ok_or_else(|| self.bad(position, "string"))
    }

    pub fn opt_string(&self, position: usize) -> mlua::Result<Option<String>> {
        match self.get(position) {
            None => Ok(None),
            Some(value @ (Value::String(_) | Value::Integer(_) | Value::Number(_))) => {
                Ok(self.lua.coerce_string(value.clone())?.map(|s| s.to_string_lossy()))
            }
            Some(_) => Err(self.bad(position, "string")),
        }
    }

    /// Lua's truthiness: anything but `nil` and `false`.
    pub fn boolean(&self, position: usize) -> bool {
        !matches!(self.values.get(position - 1), None | Some(Value::Nil) | Some(Value::Boolean(false)))
    }

    pub fn table(&self, position: usize) -> mlua::Result<mlua::Table> {
        match self.get(position) {
            Some(Value::Table(t)) => Ok(t.clone()),
            _ => Err(self.bad(position, "table")),
        }
    }

    pub fn function(&self, position: usize) -> mlua::Result<mlua::Function> {
        match self.get(position) {
            Some(Value::Function(f)) => Ok(f.clone()),
            _ => Err(self.bad(position, "function")),
        }
    }

    /// A userdata of the engine, by value.
    pub fn userdata<T: Copy + 'static>(&self, position: usize, expected: &str) -> mlua::Result<T> {
        match self.get(position) {
            Some(Value::UserData(ud)) => ud.borrow::<T>().map(|v| *v).map_err(|_| self.bad(position, expected)),
            _ => Err(self.bad(position, expected)),
        }
    }
}
