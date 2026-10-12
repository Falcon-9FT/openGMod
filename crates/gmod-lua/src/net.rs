//! The `net` library: messages built bit by bit and read back the same way.
//!
//! A message is a 16-bit ID (from `util.AddNetworkString`) and the bits written after it,
//! least significant bit first. A message that is sent leaves through
//! [`crate::Runtime::take_net_messages`] with the players it is for (`net.Send` takes a player,
//! a table of them or a `RecipientFilter`, [`crate::recipients`]), and the program carries it;
//! one that arrives is handed in with [`crate::Runtime::receive_net_message`], which runs
//! `net.Incoming` as the engine does. Vectors and angles are sent as three 32-bit floats, without the precision
//! Source's encoding loses; a matrix is its sixteen floats, row by row.

use std::cell::RefCell;

use mlua::{Lua, MultiValue, Table, Value};

use crate::args::Args;
use crate::entity::EntityHandle;
use crate::matrix::Matrix;
use crate::vector::{Angle, Vector};
use crate::{Realm, host, util};

/// A message may hold 64 KiB, the ID and the engine's own bytes included.
const MAX_BYTES: usize = 65533;
/// What the engine adds to every message, counted by `net.BytesWritten`.
const OVERHEAD_BITS: usize = 24;

/// Bits, least significant first.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Bits {
    pub bytes: Vec<u8>,
    pub len: usize,
}

impl Bits {
    pub fn push(&mut self, value: u64, count: usize) {
        for i in 0..count {
            if self.len.is_multiple_of(8) {
                self.bytes.push(0);
            }
            if value >> i & 1 != 0 {
                *self.bytes.last_mut().unwrap() |= 1 << (self.len % 8);
            }
            self.len += 1;
        }
    }

    pub fn push_bytes(&mut self, data: &[u8]) {
        for &byte in data {
            self.push(byte.into(), 8);
        }
    }
}

/// Reading position in a message.
#[derive(Debug, Clone, Default)]
struct Reader {
    bits: Bits,
    at: usize,
}

impl Reader {
    fn left(&self) -> usize {
        self.bits.len.saturating_sub(self.at)
    }

    /// `count` bits; past the end they read as 0.
    fn take(&mut self, count: usize) -> u64 {
        let mut value = 0u64;
        for i in 0..count {
            if self.at < self.bits.len && self.bits.bytes[self.at / 8] >> (self.at % 8) & 1 != 0 {
                value |= 1 << i;
            }
            self.at = (self.at + 1).min(self.bits.len);
        }
        value
    }

    fn bytes(&mut self, count: usize) -> Vec<u8> {
        (0..count.min(self.left() / 8)).map(|_| self.take(8) as u8).collect()
    }
}

/// A message that was sent: its name, its data after the ID, and who it is for.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NetMessage {
    pub name: String,
    pub data: Bits,
    pub unreliable: bool,
    /// The players it goes to, by entity index; on a client, empty: it goes to the server.
    pub to: Vec<u32>,
}

struct Outgoing {
    name: String,
    bits: Bits,
    unreliable: bool,
}

#[derive(Default)]
pub(crate) struct Net {
    outgoing: RefCell<Option<Outgoing>>,
    incoming: RefCell<Option<Reader>>,
    pub sent: RefCell<Vec<NetMessage>>,
}

fn def(lua: &Lua, table: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<Value>) -> mlua::Result<()> {
    table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
}

/// Writes into the message being built; an error when there is none.
fn write(a: &Args, f: impl FnOnce(&mut Bits)) -> mlua::Result<Value> {
    let net = &host(a.lua).net;
    let mut outgoing = net.outgoing.borrow_mut();
    let Some(message) = outgoing.as_mut() else {
        return Err(crate::args::error(a.lua, "net message not started (net.Start)"));
    };
    f(&mut message.bits);
    if message.bits.len.div_ceil(8) + 2 > MAX_BYTES {
        *outgoing = None;
        return Err(crate::args::error(a.lua, format!("net message is too large (more than {MAX_BYTES} bytes)")));
    }
    Ok(Value::Nil)
}

/// Reads from the message being received; `None` when there is none.
fn read<R>(a: &Args, f: impl FnOnce(&mut Reader) -> R) -> Option<R> {
    host(a.lua).net.incoming.borrow_mut().as_mut().map(f)
}

fn uint_bits(a: &Args, position: usize) -> mlua::Result<usize> {
    let bits = a.number(position)?;
    if !(1.0..=32.0).contains(&bits) {
        return Err(a.bad(position, "bit count between 1 and 32"));
    }
    Ok(bits as usize)
}

/// Sends the message being built to `to`; it is done with either way.
fn send(a: &Args, to: Vec<EntityHandle>) -> mlua::Result<Value> {
    let to = to.into_iter().filter_map(|p| p.index).collect();
    let net = &host(a.lua).net;
    let Some(message) = net.outgoing.borrow_mut().take() else {
        return Err(crate::args::error(a.lua, "net message not started (net.Start)"));
    };
    net.sent.borrow_mut().push(NetMessage { name: message.name, data: message.bits, unreliable: message.unreliable, to });
    Ok(Value::Nil)
}

pub(crate) fn install(lua: &Lua, realm: Realm) -> mlua::Result<()> {
    let net = lua.create_table()?;
    def(lua, &net, "Start", |a| {
        let name = a.string(1)?;
        let host = host(a.lua);
        let pooled = host.network_strings.0.borrow().iter().any(|s| s.eq_ignore_ascii_case(&name));
        if !pooled {
            if host.realm == Realm::Server {
                return Err(crate::args::error(a.lua, format!("Calling net.Start with unpooled message name! [{name}]")));
            }
            // a client knows the names the server pooled; without a server, any name is fine
            drop(host);
            util::add_network_string(a.lua, &name);
        }
        *crate::host(a.lua).net.outgoing.borrow_mut() = Some(Outgoing { name, bits: Bits::default(), unreliable: a.boolean(2) });
        Ok(Value::Boolean(true))
    })?;
    def(lua, &net, "Abort", |a| {
        host(a.lua).net.outgoing.borrow_mut().take();
        Ok(Value::Nil)
    })?;
    def(lua, &net, "BytesWritten", |a| {
        let bits = host(a.lua).net.outgoing.borrow().as_ref().map(|m| m.bits.len + 16 + OVERHEAD_BITS);
        Ok(bits.map_or(Value::Nil, |bits| Value::Integer(bits.div_ceil(8) as i64)))
    })?;
    def(lua, &net, "BytesLeft", |a| Ok(read(a, |r| r.left()).map_or(Value::Nil, |bits| Value::Integer(bits.div_ceil(8) as i64))))?;

    def(lua, &net, "WriteBit", |a| {
        let bit = a.boolean(1);
        write(a, |b| b.push(bit.into(), 1))
    })?;
    def(lua, &net, "WriteUInt", |a| {
        let (value, bits) = (a.number(1)?, uint_bits(a, 2)?);
        write(a, |b| b.push(value as i64 as u64, bits))
    })?;
    def(lua, &net, "WriteInt", |a| {
        let (value, bits) = (a.number(1)?, uint_bits(a, 2)?);
        write(a, |b| b.push(value as i64 as u64, bits))
    })?;
    def(lua, &net, "WriteUInt64", |a| {
        let text = a.string(1)?;
        let value = text.trim().parse::<u64>().unwrap_or(0);
        write(a, |b| b.push(value, 64))
    })?;
    def(lua, &net, "WriteFloat", |a| {
        let value = a.number(1)? as f32;
        write(a, |b| b.push(value.to_bits().into(), 32))
    })?;
    def(lua, &net, "WriteDouble", |a| {
        let value = a.number(1)?;
        write(a, |b| b.push(value.to_bits(), 64))
    })?;
    def(lua, &net, "WriteString", |a| {
        let text = match a.get(1) {
            Some(Value::String(s)) => s.as_bytes().to_vec(),
            _ => a.string(1)?.into_bytes(),
        };
        let end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
        write(a, |b| {
            b.push_bytes(&text[..end]);
            b.push(0, 8);
        })
    })?;
    def(lua, &net, "WriteData", |a| {
        let data = match a.get(1) {
            Some(Value::String(s)) => s.as_bytes().to_vec(),
            _ => a.string(1)?.into_bytes(),
        };
        let length = a.opt_number(2)?.map_or(data.len(), |n| n.max(0.0) as usize);
        write(a, |b| {
            let mut data = data;
            data.resize(length, 0);
            b.push_bytes(&data);
        })
    })?;
    def(lua, &net, "WriteVector", |a| {
        let v = a.userdata::<Vector>(1, "Vector")?;
        write(a, |b| {
            for c in [v.x, v.y, v.z] {
                b.push(c.to_bits().into(), 32);
            }
        })
    })?;
    def(lua, &net, "WriteNormal", |a| {
        let v = a.userdata::<Vector>(1, "Vector")?;
        write(a, |b| {
            for c in [v.x, v.y, v.z] {
                b.push(c.to_bits().into(), 32);
            }
        })
    })?;
    def(lua, &net, "WriteAngle", |a| {
        let angle = a.userdata::<Angle>(1, "Angle")?;
        write(a, |b| {
            for c in [angle.p, angle.y, angle.r] {
                b.push(c.to_bits().into(), 32);
            }
        })
    })?;
    def(lua, &net, "WriteMatrix", |a| {
        let m = a.userdata::<Matrix>(1, "VMatrix")?;
        write(a, |b| {
            for c in m.m.as_flattened() {
                b.push(c.to_bits().into(), 32);
            }
        })
    })?;
    def(lua, &net, "WriteEntity", |a| {
        let index = match a.get(1) {
            Some(Value::UserData(ud)) => {
                ud.borrow::<EntityHandle>().ok().filter(|e| host(a.lua).entities.is_valid(**e)).and_then(|e| e.index).unwrap_or(0)
            }
            _ => 0,
        };
        write(a, |b| b.push(index.into(), 16))
    })?;

    def(lua, &net, "ReadHeader", |a| Ok(Value::Integer(read(a, |r| r.take(16) as i64).unwrap_or(0))))?;
    def(lua, &net, "ReadBit", |a| Ok(Value::Integer(read(a, |r| r.take(1) as i64).unwrap_or(0))))?;
    def(lua, &net, "ReadUInt", |a| {
        let bits = uint_bits(a, 1)?;
        Ok(Value::Integer(read(a, |r| r.take(bits) as i64).unwrap_or(0)))
    })?;
    def(lua, &net, "ReadInt", |a| {
        let bits = uint_bits(a, 1)?;
        let value = read(a, |r| r.take(bits)).unwrap_or(0);
        let shift = 64 - bits;
        Ok(Value::Integer(((value << shift) as i64) >> shift))
    })?;
    def(lua, &net, "ReadUInt64", |a| {
        let value = read(a, |r| r.take(64)).unwrap_or(0);
        Ok(Value::String(a.lua.create_string(value.to_string())?))
    })?;
    def(lua, &net, "ReadFloat", |a| Ok(Value::Number(read(a, |r| f32::from_bits(r.take(32) as u32)).unwrap_or(0.0).into())))?;
    def(lua, &net, "ReadDouble", |a| Ok(Value::Number(read(a, |r| f64::from_bits(r.take(64))).unwrap_or(0.0))))?;
    def(lua, &net, "ReadString", |a| {
        let text = read(a, |r| {
            let mut out = Vec::new();
            while r.left() >= 8 {
                match r.take(8) as u8 {
                    0 => break,
                    byte => out.push(byte),
                }
            }
            out
        })
        .unwrap_or_default();
        Ok(Value::String(a.lua.create_string(text)?))
    })?;
    def(lua, &net, "ReadData", |a| {
        let length = a.number(1)?.max(0.0) as usize;
        let data = read(a, |r| r.bytes(length)).unwrap_or_default();
        Ok(Value::String(a.lua.create_string(data)?))
    })?;
    def(lua, &net, "ReadVector", |a| {
        let [x, y, z] = read(a, |r| [0; 3].map(|_| f32::from_bits(r.take(32) as u32))).unwrap_or_default();
        Ok(Value::UserData(a.lua.create_userdata(Vector::new(x, y, z))?))
    })?;
    def(lua, &net, "ReadNormal", |a| {
        let [x, y, z] = read(a, |r| [0; 3].map(|_| f32::from_bits(r.take(32) as u32))).unwrap_or_default();
        Ok(Value::UserData(a.lua.create_userdata(Vector::new(x, y, z))?))
    })?;
    def(lua, &net, "ReadMatrix", |a| {
        let values = read(a, |r| [0; 16].map(|_| f32::from_bits(r.take(32) as u32)));
        let mut m = Matrix::ZERO;
        m.m.as_flattened_mut().copy_from_slice(&values.unwrap_or_default());
        Ok(Value::UserData(a.lua.create_userdata(m)?))
    })?;
    def(lua, &net, "ReadAngle", |a| {
        let [p, y, r] = read(a, |reader| [0; 3].map(|_| f32::from_bits(reader.take(32) as u32))).unwrap_or_default();
        Ok(Value::UserData(a.lua.create_userdata(Angle::new(p, y, r))?))
    })?;
    def(lua, &net, "ReadEntity", |a| {
        let index = read(a, |r| r.take(16) as u32).unwrap_or(0);
        crate::entity::push_index(a.lua, index)
    })?;

    if realm == Realm::Server {
        def(lua, &net, "Send", |a| send(a, crate::recipients::players_of(a.lua, a.get(1))))?;
        def(lua, &net, "SendOmit", |a| send(a, crate::recipients::all_but(a.lua, a.get(1))))?;
        for name in ["SendPAS", "SendPVS"] {
            def(lua, &net, name, |a| send(a, crate::recipients::near(a.lua, a.get(1))))?;
        }
        def(lua, &net, "Broadcast", |a| send(a, crate::player::all(a.lua)))?;
    } else {
        def(lua, &net, "SendToServer", |a| send(a, Vec::new()))?;
    }
    lua.globals().raw_set("net", net)?;
    Ok(())
}

/// Hands a message in as the engine does: the message becomes the one `net.Read*` reads,
/// and `net.Incoming(length, sender)` runs.
pub(crate) fn receive(lua: &Lua, name: &str, data: &Bits, from: EntityHandle) -> mlua::Result<()> {
    let id = util::add_network_string(lua, name);
    let mut bits = Bits::default();
    bits.push(id as u64, 16);
    for i in 0..data.len {
        bits.push((data.bytes[i / 8] >> (i % 8) & 1).into(), 1);
    }
    let length = bits.len;
    *host(lua).net.incoming.borrow_mut() = Some(Reader { bits, at: 0 });
    let incoming = lua.globals().get::<Table>("net")?.get::<Option<mlua::Function>>("Incoming")?;
    if let Some(incoming) = incoming {
        let args = mlua::IntoLuaMulti::into_lua_multi((length, from), lua)?;
        let _ = crate::files::call_reporting(lua, incoming, args);
    }
    host(lua).net.incoming.borrow_mut().take();
    Ok(())
}
