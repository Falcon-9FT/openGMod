//! User messages, the network's way before `net`: the server writes one (`umsg.Start`,
//! `umsg.String` ..., `umsg.End`) for some players, a client reads it (`bf_read`) in the
//! handler `usermessage.Hook` gave its name - GMod's `usermessage.IncomingMessage`, which the
//! engine calls with it.

use std::cell::{Cell, RefCell};

use mlua::{AnyUserData, Lua, MultiValue, Table, UserData, UserDataFields, UserDataMethods, Value};

use crate::args::Args;
use crate::entity::{self, EntityHandle};
use crate::host;
use crate::meta::{self, type_id};
use crate::vector::{Angle, Vector};

/// A value written into a user message.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Part {
    Int(i64),
    Float(f32),
    Bool(bool),
    Text(String),
    Vector([f32; 3]),
    Angle([f32; 3]),
    Entity(EntityHandle),
}

/// A message: its name, who gets it, what is in it.
type Message = (String, Vec<EntityHandle>, Vec<Part>);

/// The message being written.
#[derive(Default)]
pub(crate) struct Writing(RefCell<Option<Message>>);

fn def(lua: &Lua, table: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<Value>) -> mlua::Result<()> {
    table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
}

/// Adds a value to the message being written; none is, outside `umsg.Start`.
fn write(a: &Args, part: Part) -> mlua::Result<Value> {
    match host(a.lua).user_messages.0.borrow_mut().as_mut() {
        Some((_, _, parts)) => parts.push(part),
        None => return Err(crate::args::error(a.lua, "umsg: no message started (umsg.Start)")),
    }
    Ok(Value::Nil)
}

fn wrap(n: f64, bits: u32) -> i64 {
    let n = n as i64;
    let shift = 64 - bits;
    (n << shift) >> shift
}

pub(crate) fn install_server(lua: &Lua) -> mlua::Result<()> {
    let umsg = lua.create_table()?;
    // `umsg.Start(name, filter)`: everyone, a player, or a `CRecipientFilter`
    def(lua, &umsg, "Start", |a| {
        let name = a.string(1)?;
        let players = match a.get(2) {
            None => crate::player::all(a.lua),
            Some(value) => crate::recipients::players_of(a.lua, Some(value)),
        };
        *host(a.lua).user_messages.0.borrow_mut() = Some((name, players, Vec::new()));
        Ok(Value::Nil)
    })?;
    def(lua, &umsg, "End", |a| {
        let Some((name, players, parts)) = host(a.lua).user_messages.0.borrow_mut().take() else { return Ok(Value::Nil) };
        for player in players {
            crate::player::to_client(a.lua, player, crate::player::ToClient::UserMessage(name.clone(), parts.clone()));
        }
        Ok(Value::Nil)
    })?;
    def(lua, &umsg, "Angle", |a| {
        let v = a.userdata::<Angle>(1, "Angle")?;
        write(a, Part::Angle([v.p, v.y, v.r]))
    })?;
    def(lua, &umsg, "Bool", |a| write(a, Part::Bool(a.boolean(1))))?;
    def(lua, &umsg, "Char", |a| write(a, Part::Int(wrap(a.number(1)?, 8))))?;
    def(lua, &umsg, "Short", |a| write(a, Part::Int(wrap(a.number(1)?, 16))))?;
    def(lua, &umsg, "Long", |a| write(a, Part::Int(wrap(a.number(1)?, 32))))?;
    def(lua, &umsg, "Float", |a| write(a, Part::Float(a.number(1)? as f32)))?;
    def(lua, &umsg, "String", |a| write(a, Part::Text(a.string(1)?)))?;
    def(lua, &umsg, "PoolString", |a| write(a, Part::Text(a.string(1)?)))?;
    def(lua, &umsg, "Entity", |a| write(a, Part::Entity(entity::handle_of(a.get(1)).unwrap_or(EntityHandle::NULL))))?;
    let vector = |a: &Args| {
        let v = a.userdata::<Vector>(1, "Vector")?;
        write(a, Part::Vector([v.x, v.y, v.z]))
    };
    def(lua, &umsg, "Vector", vector)?;
    def(lua, &umsg, "VectorNormal", vector)?;
    lua.globals().raw_set("umsg", umsg)
}

const NAME: &str = "bf_read";

/// A user message as a client reads it: its values, one after another.
struct Reader {
    parts: Vec<Part>,
    at: Cell<usize>,
}

impl UserData for Reader {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("MetaName", NAME);
        fields.add_meta_field("MetaID", type_id::USERMSG);
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function("__index", |lua, (ud, key): (AnyUserData, Value)| meta::index(lua, NAME, ud, key));
        methods.add_meta_function("__newindex", |lua, (ud, key, value): (AnyUserData, Value, Value)| {
            meta::new_index(lua, NAME, ud, key, value)
        });
        methods.add_meta_method("__tostring", |_, r, ()| Ok(format!("bf_read [{} values]", r.parts.len())));
    }
}

/// The next value of the message, or none past its end.
fn next(a: &Args) -> mlua::Result<Option<Part>> {
    match a.get(1) {
        Some(Value::UserData(ud)) => {
            let r = ud.borrow::<Reader>().map_err(|_| a.bad(1, NAME))?;
            let at = r.at.get();
            r.at.set(at + 1);
            Ok(r.parts.get(at).cloned())
        }
        _ => Err(a.bad(1, NAME)),
    }
}

fn number(part: Option<Part>) -> f64 {
    match part {
        Some(Part::Int(n)) => n as f64,
        Some(Part::Float(n)) => f64::from(n),
        Some(Part::Bool(b)) => f64::from(u8::from(b)),
        _ => 0.0,
    }
}

fn three(part: Option<Part>) -> [f32; 3] {
    match part {
        Some(Part::Vector(v) | Part::Angle(v)) => v,
        _ => [0.0; 3],
    }
}

pub(crate) fn install_client(lua: &Lua) -> mlua::Result<()> {
    let t = meta::meta_table(lua, NAME, type_id::USERMSG)?;
    def(lua, &t, "ReadAngle", |a| {
        let [p, y, r] = three(next(a)?);
        entity::angle(a.lua, Angle::new(p, y, r))
    })?;
    let vector = |a: &Args| {
        let [x, y, z] = three(next(a)?);
        entity::vector(a.lua, Vector::new(x, y, z))
    };
    def(lua, &t, "ReadVector", vector)?;
    def(lua, &t, "ReadVectorNormal", vector)?;
    def(lua, &t, "ReadBool", |a| Ok(Value::Boolean(number(next(a)?) != 0.0)))?;
    let integer = |a: &Args| Ok(Value::Integer(number(next(a)?) as i64));
    def(lua, &t, "ReadChar", integer)?;
    def(lua, &t, "ReadShort", integer)?;
    def(lua, &t, "ReadLong", integer)?;
    def(lua, &t, "ReadFloat", |a| Ok(Value::Number(number(next(a)?))))?;
    def(lua, &t, "ReadString", |a| {
        let text = match next(a)? {
            Some(Part::Text(s)) => s,
            _ => String::new(),
        };
        Ok(Value::String(a.lua.create_string(text)?))
    })?;
    def(lua, &t, "ReadEntity", |a| {
        let handle = match next(a)? {
            Some(Part::Entity(h)) => h,
            _ => EntityHandle::NULL,
        };
        entity::push(a.lua, handle)
    })?;
    def(lua, &t, "Reset", |a| {
        if let Some(Value::UserData(ud)) = a.get(1) {
            ud.borrow::<Reader>().map_err(|_| a.bad(1, NAME))?.at.set(0);
        }
        Ok(Value::Nil)
    })?;
    Ok(())
}

/// A client gets a user message: `usermessage.IncomingMessage(name, msg)`.
pub(crate) fn receive(lua: &Lua, name: &str, parts: &[Part]) {
    let call = || -> mlua::Result<Option<(mlua::Function, MultiValue)>> {
        let Some(library) = lua.globals().get::<Option<Table>>("usermessage")? else { return Ok(None) };
        let Some(incoming) = library.get::<Option<mlua::Function>>("IncomingMessage")? else { return Ok(None) };
        let reader = lua.create_userdata(Reader { parts: parts.to_vec(), at: Cell::new(0) })?;
        Ok(Some((incoming, MultiValue::from_iter([Value::String(lua.create_string(name)?), Value::UserData(reader)]))))
    };
    match call() {
        Ok(Some((f, args))) => {
            let _ = crate::files::call_reporting(lua, f, args);
        }
        Ok(None) => {}
        Err(e) => host(lua).report(crate::ScriptError::from_lua(&e)),
    }
}
